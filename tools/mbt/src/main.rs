use std::fmt::Write as _;
use std::fs::File;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use log::info;

use mcrl2::read_lps;
use mcrl2::read_lps_text;
use mcrl2::set_reporting_level;
use mcrl2::verbosity_to_log_level;
use merc_lps::ExplicitLinearProcessSpecification;
use merc_mbt::LpsInfo;
use merc_mbt::ModelState;
use merc_mbt::PROTOCOL_VERSION;
use merc_mbt::PeerInfo;
use merc_mbt::SessionLimits;
use merc_mbt::ToolHello;
use merc_mbt::ToolMessage;
use merc_mbt::collect_lps_actions;
use merc_mbt::connect_adapter;
use merc_mbt::parse_partition;
use merc_mbt::run_session_with_message_log;
use merc_tools::VerbosityFlag;
use merc_tools::Version;
use merc_tools::VersionFlag;
use merc_tools::report_error;
use merc_unsafety::print_allocator_metrics;
use merc_utilities::MercError;
use merc_utilities::Timing;
use sha2::Digest;
use sha2::Sha256;

#[derive(clap::ValueEnum, Clone, Debug)]
enum LpsFormat {
    Lps,
    Text,
}

/// A model-based test (MBT) tool for mCRL2 linear process specifications,
/// speaking the mCRL2 MBT <-> Adapter protocol as a WebSocket client.
#[derive(clap::Parser, Debug)]
#[command(arg_required_else_help = true)]
struct Cli {
    #[command(flatten)]
    version: VersionFlag,

    #[command(flatten)]
    verbosity: VerbosityFlag,

    #[arg(long, global = true)]
    timings: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Connect to an adapter and run an MBT session against an LPS, checking IOCO conformance.
    Run(RunArgs),

    /// Print every observable action of an LPS, to help author its partition file.
    Info(InfoArgs),
}

#[derive(clap::Args, Debug)]
struct RunArgs {
    /// The input LPS file.
    filename: String,

    /// Explicitly choose the format of the input LPS file.
    #[arg(long, short('i'), value_enum)]
    format: Option<LpsFormat>,

    /// The input/output action partition file classifying every observable action.
    #[arg(long, short('p'))]
    partition: PathBuf,

    /// The adapter WebSocket endpoint, for example ws://localhost:8080/mbt.
    #[arg(long, short('u'))]
    url: String,

    /// Identifier reported to the adapter in the `hello` message; defaults to the LPS file name.
    #[arg(long)]
    lps_identifier: Option<String>,

    /// Reject an adapter tau-closure depth above this bound with `unsupported_config`; 0 disables the check.
    #[arg(long, default_value_t = 10_000)]
    max_tau_closure_depth: usize,

    /// Number of per-state transition summaries retained; 0 disables the cache.
    #[arg(long, default_value_t = 100_000)]
    state_cache_limit: usize,

    /// Append every sent/received wire message to FILE, for debugging a session after the fact.
    #[arg(long)]
    log_messages: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct InfoArgs {
    /// The input LPS file.
    filename: String,

    /// Explicitly choose the format of the input LPS file.
    #[arg(long, short('i'), value_enum)]
    format: Option<LpsFormat>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    env_logger::Builder::new()
        .filter_level(cli.verbosity.log_level_filter())
        .parse_default_env()
        .init();

    // Enable logging on the mCRL2 side.
    set_reporting_level(verbosity_to_log_level(cli.verbosity.verbosity()));

    if cli.version.into() {
        eprintln!("{}", Version);
        return ExitCode::SUCCESS;
    }

    let timing = Timing::new();
    let result = match &cli.command {
        Some(Command::Run(args)) => handle_run_command(args, &timing),
        Some(Command::Info(args)) => handle_info_command(args),
        None => Err("no subcommand given; run with `--help` for usage".into()),
    };

    if cli.timings {
        timing.print();
    }

    print_allocator_metrics();
    report_error(result)
}

/// Reads the LPS at `filename` in the given (or default) format.
fn read_lps_file(filename: &str, format: Option<&LpsFormat>) -> Result<mcrl2::LinearProcessSpecification, MercError> {
    match format.cloned().unwrap_or(LpsFormat::Lps) {
        LpsFormat::Lps => read_lps(filename),
        LpsFormat::Text => read_lps_text(filename),
    }
}

/// Loads the LPS and action partition, validates them against each other,
/// connects to the adapter, and runs the session to completion: the protocol
/// handshake followed by the synchronous event loop that processes
/// `get_enabled`/`input`/`output`/`quiescence`/`reset` until the connection
/// closes.
fn handle_run_command(args: &RunArgs, _timing: &Timing) -> Result<(), MercError> {
    let partition_file = File::open(&args.partition)
        .map_err(|err| format!("failed to open partition file `{}`: {err}", args.partition.display()))?;
    let partition: merc_mbt::ActionPartition = parse_partition(partition_file)
        .map_err(|err| format!("failed to parse partition file `{}`: {err}", args.partition.display()))?;

    let lps = read_lps_file(&args.filename, args.format.as_ref())
        .map_err(|err| format!("failed to load LPS file `{}`: {err}", args.filename))?;
    let explicit_lps = ExplicitLinearProcessSpecification::new(lps)?;
    partition.validate_against_lps(&explicit_lps)?;
    info!(
        "Loaded LPS `{}` and validated the action partition `{}`.",
        args.filename,
        args.partition.display()
    );

    let identifier = args
        .lps_identifier
        .clone()
        .unwrap_or_else(|| lps_identifier_from_path(&args.filename));

    let model = ModelState::new(explicit_lps, partition, args.state_cache_limit);

    let socket = connect_adapter(&args.url)?;
    info!("Connected to the adapter at `{}`.", args.url);

    let hello = ToolMessage::Hello(ToolHello {
        role: "mbt_tool".to_string(),
        protocol_version: PROTOCOL_VERSION.to_string(),
        tool: PeerInfo {
            name: "merc-mbt".to_string(),
            version: Version.to_string(),
        },
        lps: LpsInfo {
            identifier,
            hash: Some(lps_file_hash(&args.filename)?),
        },
    });

    let limits = SessionLimits {
        max_tau_closure_depth: args.max_tau_closure_depth,
    };

    let message_log = match &args.log_messages {
        Some(path) => {
            let file = File::create(path)
                .map_err(|err| format!("failed to create message log file `{}`: {err}", path.display()))?;
            info!("Logging every sent/received wire message to `{}`.", path.display());
            Some(file)
        }
        None => None,
    };

    run_session_with_message_log(socket, model, limits, hello, message_log)?;
    info!("Session ended.");

    Ok(())
}

/// Loads the LPS and prints every distinct `(name, arity)` observable action
/// found in its summands, plus a ready-to-edit partition file template, so a
/// user can classify each one as `input` or `output` without having to read
/// the LPS by hand first.
fn handle_info_command(args: &InfoArgs) -> Result<(), MercError> {
    let lps = read_lps_file(&args.filename, args.format.as_ref())
        .map_err(|err| format!("failed to load LPS file `{}`: {err}", args.filename))?;
    let explicit_lps = ExplicitLinearProcessSpecification::new(lps)?;

    let actions = collect_lps_actions(&explicit_lps);

    if actions.is_empty() {
        println!("LPS `{}` has no observable actions (only tau summands).", args.filename);
        return Ok(());
    }

    println!(
        "LPS `{}` declares {} observable action(s):\n",
        args.filename,
        actions.len()
    );

    println!("\nPartition file template (move each line into `input` or `output`):\n");
    println!("input");
    for (name, arity) in &actions {
        println!("  {};", render_action_pattern(name, *arity));
    }
    println!("output");

    Ok(())
}

/// Renders `name(v0, v1, ...)` for an action of the given arity, matching the
/// partition grammar's `action := ident ("(" ident ("," ident)* ")")?`
/// (arguments there are bare placeholder names, not the LPS's own argument
/// expressions, since only the variable name — not its value — is needed for
/// classification).
fn render_action_pattern(name: &str, arity: usize) -> String {
    if arity == 0 {
        return name.to_string();
    }
    let args = (0..arity).map(|i| format!("v{i}")).collect::<Vec<_>>().join(", ");
    format!("{name}({args})")
}

/// Falls back to the LPS file's base name when `--lps-identifier` is absent.
fn lps_identifier_from_path(filename: &str) -> String {
    Path::new(filename)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| filename.to_string())
}

/// Hashes the raw bytes of the LPS file at `filename`, for `hello.lps.hash`:
/// a SHA-256 digest an adapter can compare against its own copy of the LPS to
/// catch a tool/adapter mismatch before any observation is exchanged. Self-
/// describing (`sha256:<hex>`) since the wire format names no fixed
/// algorithm.
fn lps_file_hash(filename: &str) -> Result<String, MercError> {
    let bytes =
        std::fs::read(filename).map_err(|err| format!("failed to read LPS file `{filename}` for hashing: {err}"))?;
    let digest = Sha256::digest(&bytes);

    let mut hex = String::with_capacity("sha256:".len() + digest.len() * 2);
    hex.push_str("sha256:");
    for byte in digest.iter() {
        // `digest`'s `Array<u8, _>` output type no longer implements
        // `LowerHex` (unlike the `GenericArray` it replaced), so each byte
        // is hex-encoded by hand instead of via a single `{:x}` format.
        write!(hex, "{byte:02x}").expect("writing to a String never fails");
    }
    Ok(hex)
}

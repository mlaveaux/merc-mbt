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
use merc_mbt::PeerInfo;
use merc_mbt::ToolHello;
use merc_mbt::ToolMessage;
use merc_mbt::close_session;
use merc_mbt::connect_adapter;
use merc_mbt::parse_partition;
use merc_mbt::read_frame;
use merc_mbt::send_message;
use merc_tools::VerbosityFlag;
use merc_tools::Version;
use merc_tools::VersionFlag;
use merc_tools::report_error;
use merc_unsafety::print_allocator_metrics;
use merc_utilities::MercError;
use merc_utilities::Timing;

/// The protocol version this tool speaks (mCRL2 MBT <-> Adapter Protocol).
const PROTOCOL_VERSION: &str = "0.2";

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
    let result = handle_command(&cli, &timing);

    if cli.timings {
        timing.print();
    }

    print_allocator_metrics();
    report_error(result)
}

/// Loads the LPS and action partition, validates them against each other,
/// connects to the adapter, and exchanges the protocol handshake.
///
/// The full session — the state-set/tau-closure semantics and the
/// synchronous event loop that processes `get_enabled`/`input`/`output`/
/// `quiescence` after the handshake — is not implemented yet; see
/// `docs/merc-mbt-implementation-plan.md` (phases 3 and 4).
fn handle_command(cli: &Cli, _timing: &Timing) -> Result<(), MercError> {
    let format = cli.format.clone().unwrap_or(LpsFormat::Lps);
    let lps = match format {
        LpsFormat::Lps => read_lps(&cli.filename)?,
        LpsFormat::Text => read_lps_text(&cli.filename)?,
    };
    let explicit_lps = ExplicitLinearProcessSpecification::new(&lps)?;

    let partition_text = std::fs::read_to_string(&cli.partition)?;
    let partition = parse_partition(&partition_text)?;
    partition.validate_against_lps(&explicit_lps)?;
    info!(
        "Loaded LPS `{}` and validated the action partition `{}`.",
        cli.filename,
        cli.partition.display()
    );

    let identifier = cli
        .lps_identifier
        .clone()
        .unwrap_or_else(|| lps_identifier_from_path(&cli.filename));

    let mut socket = connect_adapter(&cli.url)?;
    info!("Connected to the adapter at `{}`.", cli.url);

    let hello = ToolMessage::Hello(ToolHello {
        role: "mbt_tool".to_string(),
        protocol_version: PROTOCOL_VERSION.to_string(),
        tool: PeerInfo {
            name: "merc-mbt".to_string(),
            version: Version.to_string(),
        },
        lps: LpsInfo { identifier, hash: None },
    });
    send_message(&mut socket, &hello)?;

    let reply = read_frame(&mut socket)?;
    info!("Adapter replied: {reply:?}");

    close_session(&mut socket)?;

    Ok(())
}

/// Falls back to the LPS file's base name when `--lps-identifier` is absent.
fn lps_identifier_from_path(filename: &str) -> String {
    Path::new(filename)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| filename.to_string())
}

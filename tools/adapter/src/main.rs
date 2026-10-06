use std::process::ExitCode;

use clap::Parser;
use merc_tools::VerbosityFlag;
use merc_tools::Version;
use merc_tools::VersionFlag;
use merc_tools::report_error;
use merc_utilities::MercError;

/// An interactive adapter for debugging the MBT implementation by hand.
#[derive(clap::Parser, Debug)]
struct Cli {
    #[command(flatten)]
    version: VersionFlag,

    #[command(flatten)]
    verbosity: VerbosityFlag,

    /// Address to listen on
    #[arg(long, short('b'), default_value = "127.0.0.1:0")]
    bind: String,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    env_logger::Builder::new()
        .filter_level(cli.verbosity.log_level_filter())
        .parse_default_env()
        .init();

    if cli.version.into() {
        eprintln!("{}", Version);
        return ExitCode::SUCCESS;
    }

    report_error(handle_command(&cli))
}

/// REPL on the stdin and stdout.
fn handle_command(cli: &Cli) -> Result<(), MercError> {
    let listener = merc_mbt_adapter::bind(&cli.bind)?;

    merc_mbt_adapter::serve(listener, std::io::stdin().lock(), &mut std::io::stdout())?;
    Ok(())
}

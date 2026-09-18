//! CLI argument parsing and command dispatch.

use std::error::Error;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "data-backup",
    version,
    about = "Data Backup command-line client"
)]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Verify that the compiled framework and local protocol agree.
    Check,
}

pub(crate) fn run() -> Result<(), Box<dyn Error>> {
    match Arguments::parse().command {
        Command::Check => {
            let status =
                data_backup_core::framework_status("data-backup", env!("CARGO_PKG_VERSION"))?;
            serde_json::to_writer(std::io::stdout().lock(), &status)?;
            println!();
            Ok(())
        }
    }
}

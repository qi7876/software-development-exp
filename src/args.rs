use clap::{Arg, ArgAction, Command, value_parser};
use std::{error::Error, path::PathBuf};

pub(crate) fn parse() -> Result<PathBuf, Box<dyn Error>> {
    let command = Command::new("bak")
        .version(env!("CARGO_PKG_VERSION"))
        .disable_version_flag(true)
        .arg(
            Arg::new("version")
                .short('v')
                .long("version")
                .action(ArgAction::Version),
        )
        .arg(
            Arg::new("working_directory")
                .short('d')
                .long("working-directory")
                .value_name("PATH")
                .required(true)
                .value_parser(value_parser!(PathBuf)),
        )
        .after_help("Configuration: README.md");

    let directory = command
        .get_matches()
        .get_one::<PathBuf>("working_directory")
        .cloned()
        .ok_or("Lack of working directory")?;

    Ok(directory)
}

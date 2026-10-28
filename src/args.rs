use clap::{Arg, ArgAction, Command, value_parser};
use std::{error::Error, path::PathBuf};

pub(crate) enum Action {
    Serve(PathBuf),
    Keygen(PathBuf),
}

pub(crate) fn parse() -> Result<Action, Box<dyn Error>> {
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
        .subcommand(
            Command::new("keygen").arg(
                Arg::new("output")
                    .long("output")
                    .required(true)
                    .value_name("PATH")
                    .value_parser(value_parser!(PathBuf)),
            ),
        )
        .subcommand_negates_reqs(true)
        .after_help("Configuration: README.md");

    let matches = command.get_matches();
    if let Some(("keygen", args)) = matches.subcommand() {
        return Ok(Action::Keygen(
            args.get_one::<PathBuf>("output")
                .cloned()
                .ok_or("missing output path")?,
        ));
    }
    let directory = matches
        .get_one::<PathBuf>("working_directory")
        .cloned()
        .ok_or("Lack of working directory")?;

    Ok(Action::Serve(directory))
}

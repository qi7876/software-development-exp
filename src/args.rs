use clap::{Arg, ArgAction, Command, value_parser};
use std::{error::Error, path::PathBuf};

pub(crate) enum Action {
    Start {
        directory: PathBuf,
        foreground: bool,
        background_child: bool,
    },
    Status,
    Stop,
    Version,
    Keygen(PathBuf),
}

fn subcommand(name: &'static str) -> Command {
    Command::new(name).arg(
        Arg::new("help")
            .short('h')
            .long("help")
            .help("Print help")
            .action(ArgAction::Help),
    )
}

fn command() -> Command {
    Command::new("bak")
        .subcommand_required(true)
        .subcommand(subcommand("version").about("Print the version"))
        .subcommand(
            subcommand("keygen")
                .about("Generate an encryption key")
                .arg(
                    Arg::new("output")
                        .long("output")
                        .required(true)
                        .value_name("PATH")
                        .value_parser(value_parser!(PathBuf)),
                ),
        )
        .subcommand(
            subcommand("server")
                .about("Manage the server")
                .subcommand_required(true)
                .subcommand(
                    subcommand("start")
                        .about("Start the server in the background")
                        .arg(
                            Arg::new("working_directory")
                                .short('d')
                                .long("working-directory")
                                .required(true)
                                .value_name("PATH")
                                .value_parser(value_parser!(PathBuf)),
                        )
                        .arg(
                            Arg::new("foreground")
                                .long("foreground")
                                .help("Run attached to this terminal")
                                .action(ArgAction::SetTrue)
                                .conflicts_with("background-child"),
                        )
                        .arg(
                            Arg::new("background-child")
                                .long("background-child")
                                .hide(true)
                                .action(ArgAction::SetTrue),
                        ),
                )
                .subcommand(subcommand("status").about("Show the current user's background server"))
                .subcommand(subcommand("stop").about("Stop the background server gracefully")),
        )
}

pub(crate) fn parse() -> Result<Action, Box<dyn Error>> {
    let matches = command().get_matches();
    match matches.subcommand() {
        Some(("version", _)) => Ok(Action::Version),
        Some(("keygen", args)) => Ok(Action::Keygen(
            args.get_one::<PathBuf>("output")
                .cloned()
                .ok_or("missing output path")?,
        )),
        Some(("server", args)) => match args.subcommand() {
            Some(("start", args)) => Ok(Action::Start {
                directory: args
                    .get_one::<PathBuf>("working_directory")
                    .cloned()
                    .ok_or("missing working directory")?,
                foreground: args.get_flag("foreground"),
                background_child: args.get_flag("background-child"),
            }),
            Some(("status", _)) => Ok(Action::Status),
            Some(("stop", _)) => Ok(Action::Stop),
            _ => Err("missing server command".into()),
        },
        _ => Err("missing command".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_belong_to_commands() {
        for args in [
            vec!["bak", "--help"],
            vec!["bak", "--version"],
            vec!["bak", "-d", "."],
            vec!["bak", "server", "start"],
        ] {
            assert!(command().try_get_matches_from(args).is_err());
        }
        for args in [
            vec!["bak", "version"],
            vec!["bak", "server", "status"],
            vec!["bak", "server", "stop"],
            vec!["bak", "server", "start", "-d", "."],
            vec!["bak", "keygen", "--output", "key"],
        ] {
            assert!(command().try_get_matches_from(args).is_ok());
        }
        assert_eq!(
            command()
                .try_get_matches_from(["bak", "help"])
                .err()
                .map(|e| e.kind()),
            Some(clap::error::ErrorKind::DisplayHelp)
        );
    }
}

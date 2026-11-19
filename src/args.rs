use clap::{Arg, ArgAction, Command, ValueHint, value_parser};
use clap_complete::{aot::Fish, generate};
use std::{
    error::Error,
    io::{self, Write},
    path::PathBuf,
};

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
    FishCompletions,
}

fn command(include_background_child: bool) -> Command {
    let mut start = Command::new("start")
        .about("Start the server in the background")
        .arg(
            Arg::new("working_directory")
                .short('d')
                .long("working-directory")
                .required(true)
                .value_name("PATH")
                .value_parser(value_parser!(PathBuf))
                .value_hint(ValueHint::DirPath),
        )
        .arg(
            Arg::new("foreground")
                .long("foreground")
                .help("Run attached to this terminal")
                .action(ArgAction::SetTrue),
        );
    // The Fish generator includes hidden flags, so add the internal flag only for parsing.
    if include_background_child {
        start = start.arg(
            Arg::new("background-child")
                .long("background-child")
                .hide(true)
                .action(ArgAction::SetTrue)
                .conflicts_with("foreground"),
        );
    }

    Command::new("bak")
        .subcommand_required(true)
        .subcommand(Command::new("version").about("Print the version"))
        .subcommand(
            Command::new("completions")
                .about("Generate a shell completion script")
                .subcommand_required(true)
                .subcommand(Command::new("fish").about("Generate Fish completions")),
        )
        .subcommand(
            Command::new("keygen")
                .about("Generate an encryption key")
                .arg(
                    Arg::new("output")
                        .long("output")
                        .required(true)
                        .value_name("PATH")
                        .value_parser(value_parser!(PathBuf))
                        .value_hint(ValueHint::FilePath),
                ),
        )
        .subcommand(
            Command::new("server")
                .about("Manage the server")
                .subcommand_required(true)
                .subcommand(start)
                .subcommand(
                    Command::new("status").about("Show the current user's background server"),
                )
                .subcommand(Command::new("stop").about("Stop the background server gracefully")),
        )
}

pub(crate) fn write_fish_completions() -> io::Result<()> {
    let mut script = Vec::new();
    generate(Fish, &mut command(false), "bak", &mut script);

    let mut stdout = io::stdout().lock();
    stdout.write_all(&script)?;
    stdout.flush()
}

pub(crate) fn parse() -> Result<Action, Box<dyn Error>> {
    let matches = command(true).get_matches();
    match matches.subcommand() {
        Some(("version", _)) => Ok(Action::Version),
        Some(("completions", args)) => match args.subcommand() {
            Some(("fish", _)) => Ok(Action::FishCompletions),
            _ => Err("missing completion shell".into()),
        },
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
    fn foreground_conflicts_with_background_child() {
        let result = command(true).try_get_matches_from([
            "bak",
            "server",
            "start",
            "-d",
            ".",
            "--foreground",
            "--background-child",
        ]);
        assert_eq!(
            result.err().map(|error| error.kind()),
            Some(clap::error::ErrorKind::ArgumentConflict)
        );
    }

    #[test]
    fn options_belong_to_commands() {
        for args in [
            vec!["bak", "--version"],
            vec!["bak", "-d", "."],
            vec!["bak", "server", "start"],
        ] {
            assert!(command(true).try_get_matches_from(args).is_err());
        }
        for args in [
            vec!["bak", "version"],
            vec!["bak", "server", "status"],
            vec!["bak", "server", "stop"],
            vec!["bak", "server", "start", "-d", "."],
            vec!["bak", "keygen", "--output", "key"],
        ] {
            assert!(command(true).try_get_matches_from(args).is_ok());
        }
        for help in ["help", "--help", "-h"] {
            assert_eq!(
                command(true)
                    .try_get_matches_from(["bak", help])
                    .err()
                    .map(|error| error.kind()),
                Some(clap::error::ErrorKind::DisplayHelp)
            );
        }
    }
}

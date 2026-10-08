//! Startup flags for the single server binary.

use std::{ffi::OsString, path::PathBuf};

pub(crate) enum Arguments {
    Help,
    Version,
    Serve { working_directory: PathBuf },
}

impl Arguments {
    pub(crate) fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut arguments = arguments.into_iter();
        let mut working_directory = PathBuf::from(".");
        while let Some(argument) = arguments.next() {
            match argument.to_str() {
                Some("--help" | "-h") => return Ok(Self::Help),
                Some("--version" | "-v") => return Ok(Self::Version),
                Some("--working-directory" | "-d") => {
                    let value = arguments.next().ok_or_else(|| {
                        "--working-directory requires a directory path".to_owned()
                    })?;
                    if value.is_empty()
                        || value.to_str().is_some_and(|value| value.starts_with('-'))
                    {
                        return Err("--working-directory requires a directory path".to_owned());
                    }
                    working_directory = value.into();
                }
                Some(value) if value.starts_with("--working-directory=") => {
                    let value = &value["--working-directory=".len()..];
                    if value.is_empty() {
                        return Err("--working-directory requires a directory path".to_owned());
                    }
                    working_directory = value.into();
                }
                _ => return Err(format!("unknown argument: {}", argument.to_string_lossy())),
            }
        }
        Ok(Self::Serve { working_directory })
    }
}

pub(crate) const HELP: &str = r#"Usage: data-backup [OPTIONS]

Start the web console and authenticated HTTP API using config.json in the
working directory. The default working directory is the current directory.

Options:
  -v, --version                     Show the software version and exit
  -h, --help                        Show this help and configuration manual
  -d, --working-directory <PATH>    Set the working directory before startup

Configuration manual:
  Create config.json in the working directory with these required fields:

  {
    "listen": "127.0.0.1:8080",
    "secret_key": "REPLACE_WITH_A_RANDOM_SECRET"
  }

  listen      IP address and port, such as 127.0.0.1:8080 or [::1]:8080.
  secret_key  Non-empty visible ASCII characters without spaces. Generate a
              random key (for example: openssl rand -hex 32). Do not use the
              placeholder above as a real key or commit the config file.

  Relative paths are resolved from the working directory. Configuration is
  read once at startup; restart the server after changing it. Missing or invalid
  configuration, unknown fields, and occupied ports cause startup to fail.
  DATA_BACKUP_CONFIG is no longer used. There are no subcommands.

Examples:
  data-backup
  data-backup -d /path/to/backups
  data-backup --working-directory="/path/with spaces"

Open the configured HTTP address in a browser and enter the secret key.
For API access: curl -H 'Authorization: Bearer YOUR_SECRET_KEY' \
  http://127.0.0.1:8080/api/status

Ctrl-C or SIGTERM shuts down the server. HTTP has no built-in TLS; use HTTPS
through a reverse proxy for remote access. Backup, restore, persistence, and
scheduling are not implemented yet; the current API provides server status.
"#;

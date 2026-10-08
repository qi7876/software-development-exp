use std::{error::Error, process::ExitCode};

mod api;
mod args;
mod backup;
mod config;
mod jobs;
mod restore;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let directory = args::parse()?;
    let working_directory = std::fs::canonicalize(&directory).map_err(|error| {
        format!(
            "cannot open working directory {}: {error}",
            directory.display()
        )
    })?;
    println!("working_directory: {}", working_directory.display());

    let config = config::Config::load(&working_directory)?;
    let log_file = if config.logging.enabled {
        Some(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&config.logging.file)
                .map_err(|error| {
                    format!(
                        "cannot open task log {}: {error}",
                        config.logging.file.display()
                    )
                })?,
        )
    } else {
        None
    };
    let jobs = jobs::JobManager::new(working_directory, log_file);

    api::serve(config, jobs)
}

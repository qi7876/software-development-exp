use std::{error::Error, process::ExitCode};

mod args;
mod config;
mod daemon;
mod jobs;
mod server;
#[cfg(test)]
mod test_support;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let (directory, managed) = match args::parse()? {
        args::Action::Start {
            directory,
            foreground,
            background_child,
        } => {
            if !foreground && !background_child {
                return daemon::start(&directory).await;
            }
            (directory, background_child)
        }
        args::Action::Status => return daemon::status().await,
        args::Action::Stop => return daemon::stop().await,
        args::Action::Version => {
            println!("bak {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        args::Action::Keygen(path) => return jobs::generate_key(&path),
    };
    let working_directory = std::fs::canonicalize(&directory).map_err(|error| {
        format!(
            "cannot open working directory {}: {error}",
            directory.display()
        )
    })?;
    println!("working_directory: {}", working_directory.display());
    let _directory_lock = daemon::lock_directory(&working_directory)?;
    let controller = if managed {
        Some(daemon::Controller::prepare(&working_directory)?)
    } else {
        None
    };
    let config = config::Config::load(&working_directory)?;
    let keys = jobs::Keyring::load(&working_directory, &config.encryption.keys)?;
    let jobs = jobs::JobManager::open(working_directory, keys).await?;
    server::serve(config, jobs, controller).await
}

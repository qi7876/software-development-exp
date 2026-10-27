use std::{error::Error, process::ExitCode};

mod args;
mod config;
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
    let directory = match args::parse()? {
        args::Action::Serve(directory) => directory,
        args::Action::Keygen(path) => return jobs::crypto::generate(&path),
    };
    let working_directory = std::fs::canonicalize(&directory).map_err(|error| {
        format!(
            "cannot open working directory {}: {error}",
            directory.display()
        )
    })?;
    println!("working_directory: {}", working_directory.display());
    let config = config::Config::load(&working_directory)?;
    let keys = jobs::crypto::Keyring::load(&working_directory, &config.encryption.keys)?;
    let jobs = jobs::JobManager::open(working_directory, keys).await?;
    server::serve(config, jobs).await
}

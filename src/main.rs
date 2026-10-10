use std::{error::Error, process::ExitCode};

mod api;
mod args;
mod backup;
mod config;
mod jobs;
mod restore;
mod storage;
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
    let directory = args::parse()?;
    let working_directory = std::fs::canonicalize(&directory).map_err(|error| {
        format!(
            "cannot open working directory {}: {error}",
            directory.display()
        )
    })?;
    println!("working_directory: {}", working_directory.display());
    let config = config::Config::load(&working_directory)?;
    let jobs = jobs::JobManager::open(working_directory).await?;
    api::serve(config, jobs).await
}

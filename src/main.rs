//! The single Data Backup process: HTTP API, web console, and backup services.

mod arguments;
mod config;
pub mod domain;
mod server;

use std::{error::Error, path::PathBuf, process::ExitCode};

use arguments::Arguments;

#[tokio::main]
async fn main() -> ExitCode {
    let working_directory = match Arguments::parse(std::env::args_os().skip(1)) {
        Ok(Arguments::Help) => {
            println!(
                "data-backup {}\n\n{}",
                env!("CARGO_PKG_VERSION"),
                arguments::HELP
            );
            return ExitCode::SUCCESS;
        }
        Ok(Arguments::Version) => {
            println!("data-backup {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Ok(Arguments::Serve { working_directory }) => working_directory,
        Err(error) => {
            eprintln!("error: {error}\nTry 'data-backup --help' for usage.");
            return ExitCode::from(2);
        }
    };
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_ansi(false)
        .init();

    match run(working_directory).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "server failed");
            ExitCode::FAILURE
        }
    }
}

async fn run(working_directory: PathBuf) -> Result<(), Box<dyn Error>> {
    std::env::set_current_dir(&working_directory).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!(
                "cannot use working directory {}: {error}",
                working_directory.display()
            ),
        )
    })?;
    let config = config::Config::load(std::env::current_dir()?.join("config.json"))?;
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("cannot listen on {}: {error}", config.listen),
            )
        })?;
    // Install fallible signal handlers before serving, so failures reach main.
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tracing::info!(address = %listener.local_addr()?, "web console and API listening");
    axum::serve(listener, server::router(config))
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = interrupt.recv() => {},
                _ = terminate.recv() => {},
            }
            tracing::info!("shutting down");
        })
        .await?;
    Ok(())
}

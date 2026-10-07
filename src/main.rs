//! The single Data Backup process: HTTP API, web console, and backup services.

mod config;
mod server;

use std::{error::Error, process::ExitCode};

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "server failed");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    if std::env::args_os().len() != 1 {
        return Err(
            "data-backup accepts no arguments; configure it using DATA_BACKUP_CONFIG".into(),
        );
    }
    let config = config::Config::load()?;
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("cannot listen on {}: {error}", config.listen),
            )
        })?;
    tracing::info!(address = %listener.local_addr()?, "web console and API listening");
    // Install fallible signal handlers before serving, so failures reach main.
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
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

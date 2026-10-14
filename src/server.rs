mod api;
use crate::{config::Config, jobs::JobManager};
use std::error::Error;
use tokio::sync::watch;

pub(crate) async fn serve(config: Config, jobs: JobManager) -> Result<(), Box<dyn Error>> {
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|error| format!("cannot listen on {}: {error}", config.listen))?;
    println!("Listening on {}", listener.local_addr()?);
    let (stop, mut server_stop) = watch::channel(false);
    let worker_jobs = jobs.clone();
    let worker_stop = stop.subscribe();
    let mut worker = tokio::spawn(async move { worker_jobs.run_worker(worker_stop).await });
    let server = axum::serve(listener, api::router(jobs.clone(), config.secret_key))
        .with_graceful_shutdown(async move {
            // Sender closure also means the owner has stopped the service.
            while !*server_stop.borrow() {
                if server_stop.changed().await.is_err() {
                    break;
                }
            }
        })
        .into_future();
    tokio::pin!(server);
    let mut worker_finished = false;
    let mut server_finished = false;
    let outcome: Result<(), Box<dyn Error>> = tokio::select! {
        result = shutdown_signal() => result.map_err(Into::into),
        result = &mut server => {
            server_finished = true;
            match result {
                Ok(()) => Err("HTTP server stopped unexpectedly".into()),
                Err(error) => Err(format!("HTTP server failed: {error}").into()),
            }
        }
        result = &mut worker => {
            worker_finished = true;
            match result {
                Ok(Ok(())) => Err("job worker stopped unexpectedly".into()),
                Ok(Err(error)) => Err(error.into()),
                Err(error) => Err(format!("job worker task failed: {error}").into()),
            }
        }
    };
    stop.send_replace(true);
    let (worker_outcome, server_outcome) = tokio::join!(
        async {
            if worker_finished {
                return Ok(());
            }
            match worker.await {
                Ok(result) => result.map_err(|error| -> Box<dyn Error> { error.into() }),
                Err(error) => {
                    Err(format!("job worker task failed during shutdown: {error}").into())
                }
            }
        },
        async {
            if server_finished {
                return Ok(());
            }
            server.await.map_err(|error| -> Box<dyn Error> {
                format!("HTTP server failed during shutdown: {error}").into()
            })
        }
    );
    jobs.close().await;
    // Report secondary shutdown failures as well as the original cause.
    if outcome.is_err() {
        if let Err(error) = &worker_outcome {
            eprintln!("{error}");
        }
        if let Err(error) = &server_outcome {
            eprintln!("{error}");
        }
    }
    outcome?;
    worker_outcome?;
    server_outcome
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}

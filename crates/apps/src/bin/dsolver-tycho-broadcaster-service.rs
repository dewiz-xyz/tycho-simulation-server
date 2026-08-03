#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = runtime::broadcaster::app::build_broadcaster_service().await?;
    let addr = std::net::SocketAddr::from((service.config.host, service.config.port));
    let app_state = service.app_state;
    let mut tasks = service.tasks;
    let app = rpc::create_broadcaster_router(app_state.clone());

    let result = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            let server = std::future::IntoFuture::into_future(
                axum::serve(listener, app.into_make_service())
                    .with_graceful_shutdown(shutdown_signal(app_state.clone())),
            );
            tokio::pin!(server);
            let feed = tasks.wait_for_feed();
            tokio::select! {
                result = &mut server => {
                    result.map_err(|error| anyhow::anyhow!("Failed to start server: {error}"))
                },
                result = feed => {
                    if app_state.is_stopping() {
                        let server_result = server
                            .await
                            .map_err(|error| anyhow::anyhow!("Failed to start server: {error}"));
                        result.and(server_result)
                    } else {
                        // An early return here would skip the state history drain below.
                        result.and_then(|()| {
                            Err(anyhow::anyhow!("Broadcaster feed task stopped unexpectedly"))
                        })
                    }
                }
            }
        }
        Err(error) => Err(error.into()),
    };
    app_state.begin_shutdown();
    let task_result = tasks.stop_and_wait().await;
    app_state.stop_recovery_tasks().await;
    app_state.shutdown_state_history().await;
    result.and(task_result)
}

async fn shutdown_signal(app_state: runtime::broadcaster::app::BroadcasterAppState) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    app_state.begin_shutdown();
}

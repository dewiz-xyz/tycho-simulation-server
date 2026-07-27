#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = runtime::broadcaster::app::build_broadcaster_service().await?;
    let addr = std::net::SocketAddr::from((service.config.host, service.config.port));
    let app_state = service.app_state;
    let app = rpc::create_broadcaster_router(app_state.clone());

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let server = axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_signal(app_state));
    let supervisor = wait_for_supervisor(service.supervisors);
    tokio::select! {
        result = server => result
            .map_err(|error| anyhow::anyhow!("Failed to start server: {error}")),
        result = supervisor => {
            result?;
            Err(anyhow::anyhow!("Broadcaster feed supervisor terminated unexpectedly"))
        }
    }
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

async fn wait_for_supervisor(supervisors: Vec<tokio::task::JoinHandle<()>>) -> anyhow::Result<()> {
    let (finished_tx, mut finished_rx) = tokio::sync::mpsc::channel(supervisors.len().max(1));
    for supervisor in supervisors {
        let finished_tx = finished_tx.clone();
        tokio::spawn(async move {
            let _ = finished_tx.send(supervisor.await).await;
        });
    }
    drop(finished_tx);

    finished_rx
        .recv()
        .await
        .ok_or_else(|| anyhow::anyhow!("No broadcaster feed supervisors were started"))?
        .map_err(|error| anyhow::anyhow!("Broadcaster feed supervisor panicked: {error}"))
}

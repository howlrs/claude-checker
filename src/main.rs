use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use clap::Parser;
use claude_checker::{
    monitor::{Monitor, MonitorConfig},
    server::{router, AppState},
    state::Store,
};
use tokio::net::TcpListener;
use tokio::signal;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(Parser, Debug)]
#[command(version, about = "Local kanban dashboard for Claude Code")]
struct Args {
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, default_value_t = 8081)]
    port: u16,
    #[arg(long, env = "CC_LOG_LEVEL", default_value = "info")]
    log_level: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    tracing_subscriber::registry()
        .with(EnvFilter::try_new(&args.log_level).unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer().compact())
        .init();

    if args.host != "127.0.0.1" && args.host != "localhost" {
        return Err(anyhow!(
            "refusing to bind to non-localhost host {:?}",
            args.host
        ));
    }

    let store = Store::new();
    let monitor = Arc::new(Monitor::new(store.clone(), MonitorConfig::from_home()?));
    monitor.initial_scan().await?;
    let _watcher = monitor.clone().spawn()?;

    let app = router(AppState {
        store: store.clone(),
        port: args.port,
    });

    let addr: SocketAddr = format!("{}:{}", args.host, args.port).parse()?;
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("listening on http://{addr}");

    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());
    server.await?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = signal::unix::signal(signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutdown signal received");
}

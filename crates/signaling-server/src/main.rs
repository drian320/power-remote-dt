use clap::Parser;
use prdt_signaling_server::{router, ServerConfig, ServerState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

#[derive(Parser, Debug)]
#[command(version, about = "power-remote-dt signaling server")]
struct Args {
    /// Listen address.
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: SocketAddr,
    /// Tracing log level.
    #[arg(long, default_value = "info")]
    log: String,
    /// Session inactivity timeout in milliseconds.
    #[arg(long = "session-timeout-ms", default_value_t = 60_000)]
    session_timeout_ms: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(args.log.clone())
        .init();

    let state = Arc::new(ServerState::new());
    let cfg = ServerConfig { session_timeout: Duration::from_millis(args.session_timeout_ms) };
    let app = router(state, cfg);

    info!(bind = %args.bind, "server_started");
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

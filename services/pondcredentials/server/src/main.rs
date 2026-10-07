use std::net::SocketAddr;
use std::sync::Arc;

use pondcredentials::{router, AppState, Config};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn bind_address() -> String {
    std::env::var("PONDCREDENTIALS_BIND").unwrap_or_else(|_| "0.0.0.0:8080".to_string())
}

/// `--health-check`: ask this process's own port for /healthz and exit 0 or 1. The image has no
/// shell and no curl, so the container's health check is the binary itself.
async fn health_check() -> i32 {
    let addr = bind_address();
    let port = addr.rsplit(':').next().unwrap_or("8080");
    let Ok(mut stream) = TcpStream::connect(format!("127.0.0.1:{port}")).await else {
        return 1;
    };
    let request = "GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    if stream.write_all(request.as_bytes()).await.is_err() {
        return 1;
    }
    let mut reply = String::new();
    let _ = stream.read_to_string(&mut reply).await;
    i32::from(!reply.starts_with("HTTP/1.1 200"))
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}

#[tokio::main]
async fn main() {
    if std::env::args().any(|a| a == "--health-check") {
        std::process::exit(health_check().await);
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("info".parse().unwrap()),
        )
        .init();

    let config = match Config::from_env(
        |name| std::env::var(name).ok(),
        |path| std::fs::read_to_string(path).map_err(|e| e.to_string()),
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("pondcredentials: {e}");
            std::process::exit(2);
        }
    };
    let state = match AppState::new(&config) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("pondcredentials: the key cannot sign a token: {e}");
            std::process::exit(2);
        }
    };

    let addr = bind_address();
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("pondcredentials: cannot listen on {addr}: {e}");
            std::process::exit(2);
        }
    };
    // The key id and the lifetimes are configuration; the key itself is never logged.
    tracing::info!(
        %addr,
        key_id = %config.key_id,
        ttl_days = config.ttl.as_secs() / 86_400,
        rate_per_minute = config.rate_per_minute,
        trust_proxy = config.trust_proxy,
        relays_uber = config.uber.is_some(),
        "pondcredentials is serving"
    );

    let app = router(state).into_make_service_with_connect_info::<SocketAddr>();
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await
    {
        eprintln!("pondcredentials: {e}");
        std::process::exit(1);
    }
}

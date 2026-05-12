mod defs;
mod map;
mod sim;
mod transport;
mod types;
mod wire;

use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let sim = Arc::new(Mutex::new(sim::Sim::new()));
    let wt_bind = std::env::var("TRADSCAPE_WT_BIND").unwrap_or_else(|_| "0.0.0.0:8082".to_string());
    let http_bind = std::env::var("TRADSCAPE_HTTP_BIND").unwrap_or_else(|_| "0.0.0.0:8081".to_string());
    let (cert_tx, cert_rx) = tokio::sync::oneshot::channel();

    tokio::spawn({
        let sim = sim.clone();
        let wt_bind = wt_bind.clone();
        async move {
            if let Err(err) = transport::run_server(&wt_bind, sim, cert_tx).await {
                eprintln!("transport error: {err}");
            }
        }
    });

    let cert = cert_rx.await?;
    let static_root = sim::resolve_static_root();
    let cert_path = std::env::var("TRADSCAPE_CERT_HASH_FILE")
        .unwrap_or_else(|_| {
            static_root.join("cert_hash.txt").to_string_lossy().into_owned()
        });
    std::fs::write(&cert_path, format!("{}\n", cert.hash_hex))?;
    println!("Wrote cert hash to {}", cert_path);

    // Static HTTP server for the client.
    let app = axum::Router::new()
        .fallback_service(
            tower_http::services::ServeDir::new(&static_root)
                .append_index_html_on_directories(true),
        );
    let listener = tokio::net::TcpListener::bind(&http_bind).await?;
    println!("Static client on http://{}", http_bind);
    axum::serve(listener, app).await?;
    Ok(())
}

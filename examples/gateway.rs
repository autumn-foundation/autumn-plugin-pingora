//! An Autumn app on port 3000 with a Pingora proxy on port 8080.
//!
//! ```text
//! cargo run --example gateway
//!
//! curl -s localhost:8080/            # the Autumn app (fallback)
//! curl -s localhost:8080/billing/42  # the billing service, prefix removed
//! curl -s localhost:3000/actuator/health
//! curl -s localhost:3000/actuator/prometheus | grep pingora_proxy_
//! ```
//!
//! The example starts a small "billing" service on `127.0.0.1:7001`.

use autumn_plugin_pingora::{PingoraPlugin, Route};

/// Autumn needs one typed route to boot.
#[autumn_web::get("/")]
async fn index() -> &'static str {
    "Autumn app, behind the Pingora proxy\n"
}

/// A second service, as a stand-in for a real upstream.
async fn billing_service() {
    let app = axum::Router::new()
        .fallback(|uri: axum::http::Uri| async move { format!("billing service: {uri}\n") });
    match tokio::net::TcpListener::bind("127.0.0.1:7001").await {
        Ok(listener) => {
            let _ = axum::serve(listener, app).await;
        }
        Err(error) => eprintln!("billing service: {error}"),
    }
}

#[autumn_web::main]
async fn main() {
    tokio::spawn(billing_service());
    autumn_web::app()
        .routes(autumn_web::routes![index])
        .plugin(
            PingoraPlugin::new()
                .bind("127.0.0.1:8080")
                .route(
                    Route::new("billing")
                        .path_prefix("/billing")
                        .upstream("127.0.0.1:7001")
                        .strip_prefix(true),
                )
                .public(),
        )
        .run()
        .await;
}

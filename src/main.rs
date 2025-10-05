#![allow(dead_code)]
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install ring crypto provider");

    tracing_subscriber::fmt::init();

    let config = mindshard::config::Config::default();
    tracing::info!("Starting MindShard with config: {:?}", config);

    mindshard::run(config).await
}

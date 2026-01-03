#![allow(dead_code)]
use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::mpsc;

pub mod config;
pub(crate) mod extractor;
pub(crate) mod loader;
pub(crate) mod proxy;
pub mod semantic_store;
pub(crate) mod splitter;
pub(crate) mod web;

pub async fn run(config: config::Config) -> Result<()> {
    let embedding_provider = semantic_store::EmbedAnythingProvider::new()?;
    let vector_db_provider =
        semantic_store::LibSqlProvider::new(&config.db_path, embedding_provider.dimensions).await?;
    vector_db_provider.init_tables().await?;

    let semantic_store = Arc::new(semantic_store::VectorStore::new(
        embedding_provider,
        vector_db_provider,
    ));
    run_with_semantic_store(config, semantic_store).await
}

pub async fn run_with_semantic_store<SS>(
    config: config::Config,
    semantic_store: Arc<SS>,
) -> Result<()>
where
    SS: semantic_store::SemanticStore + Send + Sync + 'static,
{
    let splitter = Arc::new(splitter::HtmlSplitter::new(config.chunk_size)?);
    let loader = loader::HtmlLoader::new(semantic_store.clone(), splitter);

    let (tx, rx) = mpsc::channel::<loader::HtmlDocument>(100);
    let extractor = extractor::HttpBodyExtractor::new(tx, config.browser_only);

    let host_exclusion_patterns = config
        .compile_host_exclusion_patterns()
        .context("Failed to compile host exclusion patterns")?;

    let proxy_result = proxy::start_http_proxy(
        config.proxy_port,
        config.private_key_bytes()?,
        config.ca_cert_bytes()?,
        extractor,
        host_exclusion_patterns,
    );
    let loader_result = loader.start(rx);
    let web_result = web::start_web_server(config.web_port, semantic_store.clone());

    tokio::select! {
        result = proxy_result => {
            if let Err(e) = result {
                tracing::error!("Proxy server error: {e}");
            }
        }
        result = loader_result => {
            if let Err(e) = result {
                tracing::error!("Load error: {e}");
            }
        }
        result = web_result => {
            if let Err(e) = result {
                tracing::error!("Web server error: {e}");
            }
        }
    }

    Ok(())
}

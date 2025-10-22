use crate::semantic_store::SemanticStore;
use crate::splitter::Splitter;
use anyhow::Result;
use async_compression::tokio::bufread::{BrotliDecoder, DeflateDecoder, GzipDecoder, ZstdDecoder};
use bytes::Bytes;
use http::uri::Uri;
use std::sync::Arc;
use tokio::io::BufReader;
use tokio::sync::mpsc;

pub struct HtmlDocument {
    uri: Uri,
    body: Bytes,
    content_encoding: Option<String>,
}

impl HtmlDocument {
    pub fn new(uri: Uri, body: Bytes, content_encoding: Option<String>) -> Self {
        Self {
            uri,
            body,
            content_encoding,
        }
    }
}

pub struct HtmlLoader<SS, SP> {
    splitter: Arc<SP>,
    semantic_store: Arc<SS>,
}

impl<SS, SP> Clone for HtmlLoader<SS, SP> {
    fn clone(&self) -> Self {
        Self {
            splitter: self.splitter.clone(),
            semantic_store: self.semantic_store.clone(),
        }
    }
}

impl<SS, SP> HtmlLoader<SS, SP>
where
    SS: SemanticStore + Send + Sync + 'static,
    SP: Splitter + Send + Sync + 'static,
{
    pub fn new(semantic_store: Arc<SS>, splitter: Arc<SP>) -> Self {
        Self {
            semantic_store,
            splitter,
        }
    }

    pub async fn start(&self, mut rx: mpsc::Receiver<HtmlDocument>) -> Result<()> {
        loop {
            if let Some(document) = rx.recv().await {
                let loader_clone = self.clone();
                tokio::task::spawn(async move {
                    loader_clone.process_document(document).await;
                });
            } else {
                tracing::info!("Channel closed, stopping loader");
                return Ok(());
            }
        }
    }

    async fn process_document(self, document: HtmlDocument) {
        let uri_str = document.uri.to_string();
        match self.split_document(document).await {
            Ok(chunks) => {
                tracing::info!("Loading document for URI: {}", uri_str);
                // First delete existing entries for the same URI to avoid duplicates
                if let Err(e) = self.semantic_store.delete_entries_by_url(&uri_str).await {
                    tracing::error!("Failed to delete existing entries for {}: {}", uri_str, e);
                    return;
                }
                for chunk in chunks {
                    if let Err(e) = self.semantic_store.add_entry(&uri_str, &chunk).await {
                        tracing::error!("Failed to store the entry: {}", e);
                    } else {
                        tracing::debug!("Added entry (size {}) for URI: {}", chunk.len(), uri_str);
                    }
                }
            }
            Err(e) => tracing::error!("Failed to split the HTML document: {}", e),
        }
    }

    async fn split_document(&self, document: HtmlDocument) -> Result<Vec<String>> {
        let decompressed_body = match document.content_encoding.as_deref() {
            Some("identity") | None => &document.body,
            Some(enc) => &Self::decompress_body(&document.body, enc).await?,
        };
        let decoded_body = std::str::from_utf8(decompressed_body)?;
        let decoded_body = decoded_body.trim_start();
        if !decoded_body[..50].contains("<html") {
            // A very naive check to see if the document is HTML
            anyhow::bail!(
                "Document does not appear to be HTML ({}):\n{}...",
                document.uri,
                &decoded_body[..50]
            );
        }
        let chunks = self.splitter.split(decoded_body)?;
        Ok(chunks)
    }

    async fn decompress_body(body: &Bytes, encoding: &str) -> Result<Bytes> {
        let reader = BufReader::new(&body[..]);
        let mut decompressed_buffer = Vec::new();
        match encoding {
            "gzip" => {
                tokio::io::copy(&mut GzipDecoder::new(reader), &mut decompressed_buffer).await?
            }
            "br" => {
                tokio::io::copy(&mut BrotliDecoder::new(reader), &mut decompressed_buffer).await?
            }
            "deflate" => {
                tokio::io::copy(&mut DeflateDecoder::new(reader), &mut decompressed_buffer).await?
            }
            "zstd" => {
                tokio::io::copy(&mut ZstdDecoder::new(reader), &mut decompressed_buffer).await?
            }
            _ => {
                anyhow::bail!("Unsupported Content-Encoding: {encoding}");
            }
        };
        Ok(Bytes::from(decompressed_buffer))
    }
}

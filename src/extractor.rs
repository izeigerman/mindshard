use crate::loader::HtmlDocument;
use crate::proxy::http::HttpResponseHandler;
use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use http::uri::Uri;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::{body::Body, header::CONTENT_TYPE, Response};
use tokio::sync::mpsc;

const SUPPORTED_ENCODINGS: &[&str] = &["gzip", "br", "deflate", "zstd", "identity"];

const BROWSER_USER_AGENTS: &[&str] = &[
    // Chrome (Desktop & Mobile)
    "chrome/",
    "crios/", // Chrome iOS
    // Firefox (Desktop & Mobile)
    "firefox/",
    "fxios/", // Firefox iOS
    // Safari (Desktop & Mobile)
    "safari/",
    "version/", // Safari version indicator
    // Edge
    "edg/",
    "edgios/", // Edge iOS
    "edga/",   // Edge Android
    // Opera
    "opr/",
    "opera/",
    "opt/", // Opera Touch
    // Samsung Internet
    "samsungbrowser/",
    // Brave (uses Chrome user agent but sometimes includes Brave)
    "brave/",
    // Vivaldi
    "vivaldi/",
    // UC Browser
    "ucbrowser/",
    "ucweb/",
];

fn is_browser(user_agent: &str) -> bool {
    let ua_lower = user_agent.to_lowercase();
    BROWSER_USER_AGENTS
        .iter()
        .any(|&marker| ua_lower.contains(marker))
}

pub struct HttpBodyExtractor {
    tx: mpsc::Sender<HtmlDocument>,
    min_body_size: usize,
    max_body_size: usize,
    browser_only: bool,
}

impl HttpBodyExtractor {
    pub fn new(tx: mpsc::Sender<HtmlDocument>, browser_only: bool) -> Self {
        Self {
            tx,
            min_body_size: 50,
            max_body_size: 10 * 1024 * 1024,
            browser_only,
        }
    }
}

#[async_trait]
impl HttpResponseHandler for HttpBodyExtractor {
    async fn filter_response(
        &self,
        request_headers: &hyper::HeaderMap,
        response: &Response<hyper::body::Incoming>,
    ) -> Result<bool> {
        if self.browser_only {
            if let Some(user_agent) = request_headers.get(hyper::header::USER_AGENT) {
                if let Ok(user_agent_str) = user_agent.to_str() {
                    if !is_browser(user_agent_str) {
                        tracing::debug!(
                            "Skipping request from a non-browser User-Agent: {user_agent_str}"
                        );
                        return Ok(false);
                    }
                } else {
                    tracing::debug!("Invalid User-Agent header: {user_agent:?}");
                    return Ok(false);
                }
            } else {
                tracing::debug!("No User-Agent header was found");
                return Ok(false);
            }
        }

        let encoding = response
            .headers()
            .get(hyper::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_lowercase());

        if let Some(enc) = encoding {
            if !SUPPORTED_ENCODINGS.contains(&enc.as_str()) {
                tracing::debug!("Skipping unsupported Content-Encoding: {}", enc);
                return Ok(false);
            }
        }

        if let Some(content_type) = response.headers().get(CONTENT_TYPE) {
            if let Ok(content_type_str) = content_type.to_str() {
                if content_type_str.starts_with("text/html") {
                    // Make sure the body is not too large
                    let result = response.body().size_hint().upper().is_none_or(|len| {
                        len as usize <= self.max_body_size && len as usize >= self.min_body_size
                    });
                    return Ok(result);
                } else {
                    tracing::debug!(
                        "Skipping non-text response with Content-Type: {}",
                        content_type_str
                    );
                    return Ok(false);
                }
            }
        }
        Ok(false)
    }

    async fn handle_response(
        &self,
        uri: Uri,
        response: Response<hyper::body::Incoming>,
    ) -> Result<Response<BoxBody<Bytes, anyhow::Error>>> {
        let (parts, body) = response.into_parts();
        tracing::debug!(
            "Response for {}, status {}, headers: {:#?}",
            uri.to_string(),
            parts.status,
            parts.headers
        );

        let full_body = body.collect().await?.to_bytes();
        if full_body.len() > self.min_body_size {
            let encoding = parts
                .headers
                .get(hyper::header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_lowercase());
            self.tx
                .send(HtmlDocument::new(uri, full_body.clone(), encoding))
                .await?
        } else {
            tracing::debug!(
                "Response body too small (size {}) for URI {}, skipping this entry",
                full_body.len(),
                uri,
            );
        }

        Ok(Response::from_parts(
            parts,
            Full::new(full_body).map_err(|e| match e {}).boxed(),
        ))
    }
}

use crate::loader::HtmlDocument;
use crate::proxy::http::HttpResponseHandler;
use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use http::uri::Uri;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::{body::Body, header::CONTENT_TYPE, Response};
use regex::Regex;
use std::sync::LazyLock;
use tokio::sync::mpsc;

const SUPPORTED_ENCODINGS: &[&str] = &["gzip", "br", "deflate", "zstd", "identity"];

static BROWSER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(chrome/|crios/|firefox/|fxios/|safari/|edg/|edgios/|edga/|opr/|opera/|opt/|samsungbrowser/|brave/|vivaldi/|ucbrowser/|ucweb/)").unwrap()
});

fn is_browser(user_agent: &str) -> bool {
    BROWSER_REGEX.is_match(&user_agent.to_lowercase())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_browser() {
        // Chrome
        assert!(is_browser("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"));
        assert!(is_browser("Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36"));
        assert!(is_browser("Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/131.0.0.0 Mobile/15E148 Safari/604.1"));

        // Firefox
        assert!(is_browser(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:132.0) Gecko/20100101 Firefox/132.0"
        ));
        assert!(is_browser(
            "Mozilla/5.0 (Android 13; Mobile; rv:132.0) Gecko/132.0 Firefox/132.0"
        ));
        assert!(is_browser("Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) FxiOS/132.0 Mobile/15E148 Safari/605.1.15"));

        // Safari
        assert!(is_browser("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15"));
        assert!(is_browser("Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1"));

        // Edge
        assert!(is_browser("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0"));
        assert!(is_browser("Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36 EdgA/131.0.0.0"));
        assert!(is_browser("Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) EdgiOS/131.0.0.0 Mobile/15E148 Safari/605.1.15"));

        // Other browsers
        assert!(is_browser("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 OPR/115.0.0.0"));
        assert!(is_browser("Mozilla/5.0 (Linux; Android 13; SM-S918B) AppleWebKit/537.36 (KHTML, like Gecko) SamsungBrowser/26.0 Chrome/122.0.0.0 Mobile Safari/537.36"));
        assert!(is_browser("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Brave/131.0.0.0"));
        assert!(is_browser("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Vivaldi/7.0.3495.11"));
        assert!(is_browser("Mozilla/5.0 (Linux; U; Android 13; en-US; SM-G998B) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/57.0.2987.108 UCBrowser/13.4.0.1306 Mobile Safari/537.36"));

        // Case insensitive
        assert!(is_browser("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) CHROME/131.0.0.0 Safari/537.36"));

        // Common HTTP clients
        assert!(!is_browser("curl/7.68.0"));
        assert!(!is_browser("Wget/1.20.3 (linux-gnu)"));
        assert!(!is_browser("Python-urllib/3.9"));
        assert!(!is_browser("axios/0.21.1"));
        assert!(!is_browser(
            "node-fetch/1.0 (+https://github.com/bitinn/node-fetch)"
        ));

        // Bots
        assert!(!is_browser(
            "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)"
        ));

        // Empty
        assert!(!is_browser(""));
    }
}

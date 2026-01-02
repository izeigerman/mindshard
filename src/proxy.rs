use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

pub(crate) mod ca;
pub(crate) mod http;
pub(crate) mod rewind;

#[async_trait]
trait Proxy {
    async fn start(&self, port: u16) -> Result<()>;
}

pub async fn start_http_proxy<H>(
    port: u16,
    private_key_bytes: Vec<u8>,
    ca_cert_bytes: Vec<u8>,
    response_handler: H,
    host_exclusion_patterns: Vec<regex::Regex>,
) -> Result<()>
where
    H: http::HttpResponseHandler + Send + Sync + 'static,
{
    let certificate_authority = ca::OpenSslAuthority::new(&private_key_bytes, &ca_cert_bytes)?;
    let http_proxy = http::HttpProxy::new(
        Arc::new(certificate_authority),
        Arc::new(response_handler),
        host_exclusion_patterns,
    )?;
    http_proxy.start(port).await
}

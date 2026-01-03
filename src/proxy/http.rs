use super::ca::CertificateAuthority;
use super::rewind::Rewind;
use super::Proxy;
use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http::uri::{Authority, Scheme, Uri};
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::{service::service_fn, upgrade::Upgraded, Method, Request, Response};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use regex::Regex;
use std::{future::Future, net::SocketAddr, pin::Pin, sync::Arc, sync::LazyLock};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite},
    net::TcpListener,
};
use tokio_rustls::TlsAcceptor;

type ServerBuilder = hyper::server::conn::http1::Builder;
type HttpsConnector =
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>;

/// Trait for handling HTTP responses.
#[async_trait]
pub trait HttpResponseHandler {
    /// Return true if the response should be processed by `handle_response`.
    async fn filter_response(&self, _response: &Response<hyper::body::Incoming>) -> Result<bool> {
        Ok(true)
    }

    /// Handle the HTTP response and return a modified response.
    async fn handle_response(
        &self,
        uri: Uri,
        response: Response<hyper::body::Incoming>,
    ) -> Result<Response<BoxBody<Bytes, anyhow::Error>>>;
}

pub(crate) struct HttpProxy<CA, H> {
    ca: Arc<CA>,
    client: Client<HttpsConnector, hyper::body::Incoming>,
    response_handler: Arc<H>,
    host_exclusion_patterns: Arc<Vec<regex::Regex>>,
    browser_only: bool,
}

impl<CA, H> Clone for HttpProxy<CA, H> {
    fn clone(&self) -> Self {
        Self {
            ca: self.ca.clone(),
            client: self.client.clone(),
            response_handler: self.response_handler.clone(),
            host_exclusion_patterns: self.host_exclusion_patterns.clone(),
            browser_only: self.browser_only,
        }
    }
}

impl<CA, H> HttpProxy<CA, H>
where
    CA: CertificateAuthority + Send + Sync + 'static,
    H: HttpResponseHandler + Send + Sync + 'static,
{
    pub fn new(
        ca: Arc<CA>,
        response_handler: Arc<H>,
        host_exclusion_patterns: Arc<Vec<regex::Regex>>,
        browser_only: bool,
    ) -> Result<Self> {
        let https = HttpsConnectorBuilder::new()
            .with_native_roots()?
            .https_or_http()
            .enable_http1()
            .build();

        let client: Client<HttpsConnector, hyper::body::Incoming> =
            Client::builder(TokioExecutor::new())
                .http1_preserve_header_case(true)
                .http1_title_case_headers(true)
                .build(https);

        Ok(Self {
            ca,
            client,
            response_handler,
            host_exclusion_patterns,
            browser_only,
        })
    }

    #[allow(clippy::type_complexity)]
    fn proxy_service(
        self,
        req: Request<hyper::body::Incoming>,
        scheme: Scheme,
        upgraded: bool,
    ) -> Pin<Box<impl Future<Output = Result<Response<BoxBody<Bytes, anyhow::Error>>>> + Send>>
    {
        Box::pin(async move { self.proxy_service_impl(req, scheme, upgraded).await })
    }

    async fn proxy_service_impl(
        self,
        req: Request<hyper::body::Incoming>,
        scheme: Scheme,
        upgraded: bool,
    ) -> Result<Response<BoxBody<Bytes, anyhow::Error>>> {
        tracing::debug!("Request: {:?}", req);

        if hyper_tungstenite::is_upgrade_request(&req) {
            let req = recover_uri_authority(req, scheme)?;
            tracing::info!(
                "WebSocket upgrade request detected for URI: {:?}",
                req.uri()
            );
            return self.upgrade_websocket(req);
        }

        if Method::CONNECT == req.method() {
            let uri = req.uri().clone();
            if let Some(authority) = uri.authority() {
                let authority = authority.clone();
                let should_skip = self.should_skip(Some(&authority), req.headers());
                tokio::task::spawn(async move {
                    let serve_updated_result = match hyper::upgrade::on(req).await {
                        Ok(upgraded) if should_skip => {
                            Self::serve_passthrough(upgraded, authority).await
                        }
                        Ok(upgraded) => self.clone().serve_upgraded(upgraded, authority).await,
                        Err(e) => Err(anyhow::anyhow!(
                            "Failed to upgrade connection for URI {uri}: {e}"
                        )),
                    };
                    if let Err(e) = serve_updated_result {
                        tracing::error!("Failed to serve upgraded connection for URI {uri}: {e}");
                    }
                });
                Ok(Response::new(empty()))
            } else {
                tracing::error!("CONNECT URI is missing host: {uri}");
                let mut resp = Response::new(full("CONNECT URI must specify host"));
                *resp.status_mut() = http::StatusCode::BAD_REQUEST;

                Ok(resp)
            }
        } else {
            let mut req = req;
            if upgraded {
                // If the connection was upgraded, then we need to rewrite the request URI to set
                // the correct auhority
                req = recover_uri_authority(req, scheme)?;
            }

            let uri = req.uri().clone();

            // Check if the host should be excluded from interception
            let mut should_skip = self.should_skip(uri.authority(), req.headers());

            let resp = self.client.request(req).await?;
            if !should_skip {
                // Let the response handler decide if we should skip processing
                should_skip = !self.response_handler.filter_response(&resp).await?
            }

            if should_skip {
                // Skip response handling for excluded hosts
                Ok(resp.map(|b| b.map_err(anyhow::Error::new).boxed()))
            } else {
                self.response_handler.handle_response(uri, resp).await
            }
        }
    }

    async fn serve_upgraded(self, upgraded: Upgraded, authority: Authority) -> Result<()> {
        let mut io = TokioIo::new(upgraded);
        let mut read_buffer = [0; 4];
        let bytes_read = io.read(&mut read_buffer).await?;

        let io = TokioIo::new(Rewind::new_buffered(
            io.into_inner(),
            Bytes::copy_from_slice(read_buffer[..bytes_read].as_ref()),
        ));

        if read_buffer == *b"GET " {
            self.serve_stream(io, Scheme::HTTP, true).await
        } else if read_buffer[..2] == *b"\x16\x03" {
            self.serve_tls_stream(io, authority, true).await
        } else {
            anyhow::bail!("Unknown protocol in upgraded connection")
        }
    }

    async fn serve_tls_stream<I>(
        self,
        stream: I,
        authority: Authority,
        upgraded: bool,
    ) -> Result<()>
    where
        I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let server_config = self.ca.get_server_config(&authority).await?;
        let tls_stream = TlsAcceptor::from(server_config).accept(stream).await?;
        self.serve_stream(tls_stream, Scheme::HTTPS, upgraded).await
    }

    async fn serve_stream<I>(self, stream: I, scheme: Scheme, upgraded: bool) -> Result<()>
    where
        I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let io = TokioIo::new(stream);
        ServerBuilder::new()
            .preserve_header_case(true)
            .title_case_headers(true)
            .serve_connection(
                io,
                service_fn(move |req| self.clone().proxy_service(req, scheme.clone(), upgraded)),
            )
            .with_upgrades()
            .await?;
        Ok(())
    }

    async fn serve_websocket(
        self,
        client_ws: hyper_tungstenite::WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>,
        target_uri: &Uri,
    ) -> Result<()> {
        tracing::debug!("Handling WebSocket connection for URI: {target_uri}");

        let (server_ws, _) = tokio_tungstenite::connect_async(target_uri.to_string()).await?;

        tracing::info!("Connected to the WebSocket destination: {target_uri}");

        let (mut client_sink, mut client_stream) = client_ws.split();
        let (mut server_sink, mut server_stream) = server_ws.split();

        let client_to_server = async move {
            while let Some(msg) = client_stream.next().await {
                match msg {
                    Ok(msg) => {
                        tracing::debug!("WebSocket Client -> Server");
                        if let Err(e) = server_sink.send(msg).await {
                            tracing::error!("Error sending message to server: {e}");
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::error!("Error receiving message from client: {e}");
                        break;
                    }
                }
            }
            tracing::debug!("Client to server stream closed");
        };

        let server_to_client = async move {
            while let Some(msg) = server_stream.next().await {
                match msg {
                    Ok(msg) => {
                        tracing::debug!("WebSocket Server -> Client");
                        if let Err(e) = client_sink.send(msg).await {
                            tracing::error!("Error sending message to client: {e}");
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::error!("Error receiving message from server: {e}");
                        break;
                    }
                }
            }
            tracing::debug!("Server to client WebSocket stream closed");
        };

        tokio::select! {
            _ = client_to_server => {
                tracing::debug!("WebSocket client to server task completed");
            }
            _ = server_to_client => {
                tracing::debug!("WebSocket server to client task completed");
            }
        }
        tracing::info!("WebSocket connection closed for URI: {}", target_uri);
        Ok(())
    }

    async fn serve_passthrough(upgraded: Upgraded, authority: Authority) -> Result<()> {
        tracing::debug!("Starting passthrough tunnel to {authority}");

        let mut target_stream = tokio::net::TcpStream::connect(authority.to_string()).await?;
        tracing::info!("Connected to {authority} for passthrough tunneling");

        let mut client_io = TokioIo::new(upgraded);

        let (client_to_server, server_to_client) =
            tokio::io::copy_bidirectional(&mut client_io, &mut target_stream).await?;

        tracing::debug!(
            "Passthrough tunnel closed for {authority}: {client_to_server} bytes C->T, {server_to_client} bytes T->C"
        );

        Ok(())
    }

    fn upgrade_websocket(
        self,
        req: Request<hyper::body::Incoming>,
    ) -> Result<Response<BoxBody<Bytes, anyhow::Error>>> {
        let mut req = {
            let (mut parts, _) = req.into_parts();

            parts.uri = {
                let mut parts = parts.uri.into_parts();

                parts.scheme = if parts.scheme.unwrap_or(Scheme::HTTP) == Scheme::HTTP {
                    Some("ws".try_into()?)
                } else {
                    Some("wss".try_into()?)
                };

                Uri::from_parts(parts)?
            };

            Request::from_parts(parts, ())
        };

        let (res, websocket) = hyper_tungstenite::upgrade(&mut req, None)?;

        let fut = async move {
            match websocket.await {
                Ok(ws) => {
                    if let Err(e) = self.serve_websocket(ws, req.uri()).await {
                        tracing::error!("Failed to handle WebSocket stream: {e}");
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to upgrade to WebSocket: {e}");
                }
            }
        };

        tokio::task::spawn(fut);
        Ok(res.map(|b| b.map_err(|never| match never {}).boxed()))
    }

    fn should_skip(
        &self,
        authority: Option<&Authority>,
        request_headers: &hyper::HeaderMap,
    ) -> bool {
        if self.browser_only {
            if let Some(user_agent) = request_headers.get(hyper::header::USER_AGENT) {
                if let Ok(user_agent_str) = user_agent.to_str() {
                    if !is_browser(user_agent_str) {
                        tracing::debug!(
                            "Skipping request from a non-browser User-Agent: {user_agent_str}"
                        );
                        return true;
                    }
                } else {
                    tracing::debug!("Invalid User-Agent header: {user_agent:?}");
                    return true;
                }
            }
        }
        authority
            .map(|auth| self.should_exclude_host(auth))
            .unwrap_or(false)
    }

    fn should_exclude_host(&self, authority: &Authority) -> bool {
        let host = authority.host();
        let is_excluded = self
            .host_exclusion_patterns
            .iter()
            .any(|pattern| pattern.is_match(host));

        if is_excluded {
            tracing::info!("Host '{host}' matches exclusion pattern, using passthrough tunnel");
        }

        is_excluded
    }
}

#[async_trait]
impl<CA, H> Proxy for HttpProxy<CA, H>
where
    CA: CertificateAuthority + Send + Sync + 'static,
    H: HttpResponseHandler + Send + Sync + 'static,
{
    async fn start(&self, port: u16) -> Result<()> {
        let addr = SocketAddr::from(([0, 0, 0, 0], port));

        let listener = TcpListener::bind(addr).await?;
        tracing::info!("HTTP proxy is listening on {}", addr);

        loop {
            let (stream, _) = listener.accept().await?;
            let proxy = self.clone();
            tokio::task::spawn(async move {
                if let Err(err) = proxy.serve_stream(stream, Scheme::HTTP, false).await {
                    tracing::error!("Failed to serve connection: {err:?}");
                }
            });
        }
    }
}

fn empty() -> BoxBody<Bytes, anyhow::Error> {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}

fn full<T: Into<Bytes>>(chunk: T) -> BoxBody<Bytes, anyhow::Error> {
    Full::new(chunk.into())
        .map_err(|never| match never {})
        .boxed()
}

fn recover_uri_authority(
    req: Request<hyper::body::Incoming>,
    scheme: Scheme,
) -> Result<Request<hyper::body::Incoming>> {
    let (mut parts, body) = req.into_parts();

    let authority = parts
        .headers
        .get(hyper::header::HOST)
        .ok_or_else(|| anyhow::anyhow!("Missing HOST header"))?
        .as_bytes();
    parts.uri = {
        let mut parts = parts.uri.into_parts();
        parts.scheme = Some(scheme);
        parts.authority = Some(Authority::try_from(authority)?);
        Uri::from_parts(parts)?
    };

    Ok(Request::from_parts(parts, body))
}

static BROWSER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(chrome/|crios/|firefox/|fxios/|safari/|edg/|edgios/|edga/|opr/|opera/|opt/|samsungbrowser/|brave/|vivaldi/|ucbrowser/|ucweb/)").unwrap()
});

fn is_browser(user_agent: &str) -> bool {
    BROWSER_REGEX.is_match(&user_agent.to_lowercase())
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

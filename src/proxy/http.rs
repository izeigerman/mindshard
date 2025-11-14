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
use std::{future::Future, net::SocketAddr, pin::Pin, sync::Arc};
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
}

impl<CA, H> Clone for HttpProxy<CA, H> {
    fn clone(&self) -> Self {
        Self {
            ca: self.ca.clone(),
            client: self.client.clone(),
            response_handler: self.response_handler.clone(),
        }
    }
}

impl<CA, H> HttpProxy<CA, H>
where
    CA: CertificateAuthority + Send + Sync + 'static,
    H: HttpResponseHandler + Send + Sync + 'static,
{
    pub fn new(ca: Arc<CA>, response_handler: Arc<H>) -> Result<Self> {
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
                tokio::task::spawn(async move {
                    match hyper::upgrade::on(req).await {
                        Ok(upgraded) => {
                            if let Err(e) = self.clone().serve_upgraded(upgraded, authority).await {
                                tracing::error!(
                                    "Server IO error for URI {}: {}",
                                    uri.to_string(),
                                    e
                                );
                            };
                        }
                        Err(e) => {
                            tracing::error!("Upgrade error for URI {}: {}", uri.to_string(), e)
                        }
                    }
                });

                Ok(Response::new(empty()))
            } else {
                tracing::error!("CONNECT URI is missing host: {}", uri.to_string());
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
            let resp = self.client.request(req).await?;
            if self.response_handler.filter_response(&resp).await? {
                self.response_handler.handle_response(uri, resp).await
            } else {
                Ok(resp.map(|b| b.map_err(anyhow::Error::new).boxed()))
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
        tracing::debug!("Handling WebSocket connection for URI: {:?}", target_uri);

        let (server_ws, _) = tokio_tungstenite::connect_async(target_uri.to_string()).await?;

        tracing::info!("Connected to the WebSocket destination: {}", target_uri);

        let (mut client_sink, mut client_stream) = client_ws.split();
        let (mut server_sink, mut server_stream) = server_ws.split();

        let client_to_server = async move {
            while let Some(msg) = client_stream.next().await {
                match msg {
                    Ok(msg) => {
                        tracing::debug!("WebSocket Client -> Server: {:?}", msg);
                        if let Err(e) = server_sink.send(msg).await {
                            tracing::error!("Error sending message to server: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::error!("Error receiving message from client: {}", e);
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
                        tracing::debug!("WebSocket Server -> Client: {:?}", msg);
                        if let Err(e) = client_sink.send(msg).await {
                            tracing::error!("Error sending message to client: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::error!("Error receiving message from server: {}", e);
                        break;
                    }
                }
            }
            tracing::debug!("Server to client WebSocket stream closed");
        };

        tokio::select! {
            _ = client_to_server => {},
            _ = server_to_client => {},
        }

        tracing::info!("WebSocket connection closed for URI: {}", target_uri);
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
                    tracing::error!("Failed to serve connection: {:?}", err);
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

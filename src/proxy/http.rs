use super::ca::CertificateAuthority;
use super::rewind::Rewind;
use super::Proxy;
use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
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

type ClientBuilder = hyper::client::conn::http1::Builder;
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
        &self,
        req: Request<hyper::body::Incoming>,
        scheme: Scheme,
        upgraded: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Response<BoxBody<Bytes, anyhow::Error>>>> + Send>> {
        let proxy_clone = self.clone();
        Box::pin(async move { proxy_clone.proxy_service_impl(req, scheme, upgraded).await })
    }

    async fn proxy_service_impl(
        &self,
        req: Request<hyper::body::Incoming>,
        scheme: Scheme,
        upgraded: bool,
    ) -> Result<Response<BoxBody<Bytes, anyhow::Error>>> {
        tracing::debug!("Request: {:?}", req);

        if Method::CONNECT == req.method() {
            let uri = req.uri().clone();
            if let Some(authority) = uri.authority() {
                let authority = authority.clone();
                let proxy_clone = self.clone();
                tokio::task::spawn(async move {
                    match hyper::upgrade::on(req).await {
                        Ok(upgraded) => {
                            if let Err(e) = proxy_clone.serve_upgraded(upgraded, authority).await {
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

    async fn serve_upgraded(&self, upgraded: Upgraded, authority: Authority) -> Result<()> {
        let mut io = TokioIo::new(upgraded);
        let mut read_buffer = [0; 4];
        let bytes_read = io.read(&mut read_buffer).await?;

        let io = TokioIo::new(Rewind::new_buffered(
            io.into_inner(),
            bytes::Bytes::copy_from_slice(read_buffer[..bytes_read].as_ref()),
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
        &self,
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

    async fn serve_stream<I>(&self, stream: I, scheme: Scheme, upgraded: bool) -> Result<()>
    where
        I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let io = TokioIo::new(stream);
        let proxy_clone = self.clone();

        ServerBuilder::new()
            .preserve_header_case(true)
            .title_case_headers(true)
            .serve_connection(
                io,
                service_fn(move |req| proxy_clone.proxy_service(req, scheme.clone(), upgraded)),
            )
            .with_upgrades()
            .await?;
        Ok(())
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

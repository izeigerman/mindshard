use anyhow::Result;
use bytes::Bytes;
use http_body_util::Full;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioIo;
use mindshard::semantic_store::SemanticStore;
use mindshard::*;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

// Simple test HTTP server that serves HTML content
async fn test_server_handler(
    _req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let html_content = r#"
        <!DOCTYPE html>
        <html>
        <head><title>Test Page</title></head>
        <body>
            <h1>Integration Test Content</h1>
            <p>This is a test paragraph with some meaningful content for semantic search.</p>
            <p>The proxy should capture this HTML and store it in the database.</p>
        </body>
        </html>
    "#;

    Ok(Response::builder()
        .status(200)
        .header("Content-Type", "text/html; charset=utf-8")
        .body(Full::new(Bytes::from(html_content)))
        .unwrap())
}

async fn start_test_http_server(port: u16) -> Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr).await?;

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);

        tokio::task::spawn(async move {
            if let Err(err) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service_fn(test_server_handler))
                .await
            {
                eprintln!("Error serving connection: {:?}", err);
            }
        });
    }
}

#[tokio::test]
async fn test_proxy_end_to_end_with_in_memory_db() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let proxy_port = 19999;
    let test_server_port = 19998;
    let web_port = 19997;

    let config = config::Config {
        proxy_port,
        web_port,
        ..config::Config::default()
    };

    // Setup in-memory database with semantic store
    let embedding_provider = semantic_store::EmbedAnythingProvider::new()?;
    let vector_db_provider =
        semantic_store::LibSqlProvider::new(":memory:", embedding_provider.dimensions).await?;
    vector_db_provider.init_tables().await?;
    let semantic_store = Arc::new(semantic_store::VectorStore::new(
        embedding_provider,
        vector_db_provider,
    ));

    // Start the proxy server
    let semantic_store_clone = semantic_store.clone();
    tokio::spawn(async move {
        if let Err(e) = mindshard::run_with_semantic_store(config, semantic_store_clone).await {
            eprintln!("Proxy error: {}", e);
        }
    });

    // Start the test HTTP server
    tokio::spawn(async move {
        if let Err(e) = start_test_http_server(test_server_port).await {
            eprintln!("Test server error: {}", e);
        }
    });

    // Give the proxy time to start
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    let mut connector = HttpConnector::new();
    connector.set_connect_timeout(Some(std::time::Duration::from_secs(5)));

    // Make a request through the proxy to the test server
    let target_url = format!("http://127.0.0.1:{}/test.html", test_server_port);

    // Build the request that will go through the proxy
    let req = Request::builder()
        .uri(&target_url)
        .header("Host", format!("127.0.0.1:{}", test_server_port))
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36")
        .body(Full::new(Bytes::new()))
        .unwrap();

    // Connect to the proxy and send the request
    let proxy_addr = format!("127.0.0.1:{}", proxy_port);
    let stream = tokio::net::TcpStream::connect(&proxy_addr).await?;
    let io = TokioIo::new(stream);

    // Send the HTTP request through the proxy connection
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;

    tokio::spawn(async move {
        if let Err(err) = conn.await {
            eprintln!("Connection error: {:?}", err);
        }
    });

    let response = sender.send_request(req).await?;
    assert_eq!(response.status(), 200, "Response status should be 200 OK");

    // Give the loader time to process the captured content
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    // Verify that the content was stored in the database
    let entries = semantic_store.get_all_entries(10, 0).await?;
    assert!(
        !entries.is_empty(),
        "Expected at least one entry in the database"
    );
    assert_eq!(
        entries[0].url, target_url,
        "URL should match the request URL"
    );
    assert_eq!(
        entries[0].domain, "127.0.0.1",
        "Domain should be extracted from URL"
    );
    let search_results = semantic_store.search("Integration Test Content", 5).await?;
    assert!(
        !search_results.is_empty(),
        "Should be able to find the stored content through semantic search"
    );

    Ok(())
}

#[tokio::test]
async fn test_host_exclusion_patterns() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let proxy_port = 19996;
    let test_server_port = 19995;
    let web_port = 19994;

    let config = config::Config {
        proxy_port,
        web_port,
        host_exclusion_patterns: vec!["^127\\.0\\.0\\.1$".to_string()],
        ..config::Config::default()
    };

    // Setup in-memory database with semantic store
    let embedding_provider = semantic_store::EmbedAnythingProvider::new()?;
    let vector_db_provider =
        semantic_store::LibSqlProvider::new(":memory:", embedding_provider.dimensions).await?;
    vector_db_provider.init_tables().await?;
    let semantic_store = Arc::new(semantic_store::VectorStore::new(
        embedding_provider,
        vector_db_provider,
    ));

    // Start the proxy server
    let semantic_store_clone = semantic_store.clone();
    tokio::spawn(async move {
        if let Err(e) = mindshard::run_with_semantic_store(config, semantic_store_clone).await {
            eprintln!("Proxy error: {}", e);
        }
    });

    // Start the test HTTP server
    tokio::spawn(async move {
        if let Err(e) = start_test_http_server(test_server_port).await {
            eprintln!("Test server error: {}", e);
        }
    });

    // Give the proxy time to start
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    let mut connector = HttpConnector::new();
    connector.set_connect_timeout(Some(std::time::Duration::from_secs(5)));

    // Make a request through the proxy to the test server
    let target_url = format!("http://127.0.0.1:{}/test.html", test_server_port);

    // Build the request that will go through the proxy
    let req = Request::builder()
        .uri(&target_url)
        .header("Host", format!("127.0.0.1:{}", test_server_port))
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36")
        .body(Full::new(Bytes::new()))
        .unwrap();

    // Connect to the proxy and send the request
    let proxy_addr = format!("127.0.0.1:{}", proxy_port);
    let stream = tokio::net::TcpStream::connect(&proxy_addr).await?;
    let io = TokioIo::new(stream);

    // Send the HTTP request through the proxy connection
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;

    tokio::spawn(async move {
        if let Err(err) = conn.await {
            eprintln!("Connection error: {:?}", err);
        }
    });

    let response = sender.send_request(req).await?;
    assert_eq!(response.status(), 200, "Response status should be 200 OK");

    // Give the loader time to process (if it were to be processed)
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    // Verify that the content was NOT stored in the database (host was excluded)
    let entries = semantic_store.get_all_entries(10, 0).await?;
    assert!(
        entries.is_empty(),
        "Expected no entries in the database because host '127.0.0.1' matches exclusion pattern"
    );

    // Verify that semantic search returns no results
    let search_results = semantic_store.search("Integration Test Content", 5).await?;
    assert!(
        search_results.is_empty(),
        "Should not find any content because host was excluded from interception"
    );

    Ok(())
}

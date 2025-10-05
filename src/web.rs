use anyhow::Result;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

use crate::semantic_store::{SearchResult, SemanticStore};

#[derive(Debug, Deserialize)]
struct SearchRequest {
    query: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    10
}

#[derive(Debug, Serialize)]
struct SearchResponse {
    results: Vec<SearchResult>,
}

// This entire HTML was vibe coded
const HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>MindShard - Vector Search</title>
    <script src="https://cdn.jsdelivr.net/npm/marked@11.1.1/marked.min.js"></script>
    <style>
        * {
            margin: 0;
            padding: 0;
            box-sizing: border-box;
        }

        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, Cantarell, sans-serif;
            background: #0f172a;
            min-height: 100vh;
            padding: 20px;
        }

        .container {
            max-width: 900px;
            margin: 0 auto;
        }

        h1 {
            color: #f1f5f9;
            text-align: center;
            margin-bottom: 40px;
            font-size: 2.5em;
            font-weight: 700;
            letter-spacing: -0.02em;
        }

        .search-box {
            background: #1e293b;
            border-radius: 16px;
            padding: 32px;
            box-shadow: 0 4px 6px rgba(0,0,0,0.3);
            margin-bottom: 32px;
            border: 1px solid #334155;
        }

        .search-form {
            display: flex;
            gap: 10px;
            flex-wrap: wrap;
        }

        .search-input-wrapper {
            flex: 1;
            min-width: 200px;
        }

        input[type="text"] {
            width: 100%;
            padding: 15px;
            border: 1px solid #475569;
            border-radius: 10px;
            font-size: 16px;
            background: #0f172a;
            color: #f1f5f9;
            transition: all 0.2s;
        }

        input[type="text"]:focus {
            outline: none;
            border-color: #3b82f6;
            box-shadow: 0 0 0 3px rgba(59, 130, 246, 0.1);
        }

        input[type="text"]::placeholder {
            color: #64748b;
        }

        select {
            padding: 15px;
            border: 1px solid #475569;
            border-radius: 10px;
            font-size: 16px;
            background: #0f172a;
            color: #f1f5f9;
            cursor: pointer;
            transition: all 0.2s;
        }

        select:focus {
            outline: none;
            border-color: #3b82f6;
            box-shadow: 0 0 0 3px rgba(59, 130, 246, 0.1);
        }

        button {
            padding: 15px 30px;
            background: #3b82f6;
            color: white;
            border: none;
            border-radius: 10px;
            font-size: 16px;
            font-weight: 600;
            cursor: pointer;
            transition: all 0.2s;
        }

        button:hover {
            background: #2563eb;
            box-shadow: 0 4px 12px rgba(59, 130, 246, 0.4);
        }

        button:active {
            transform: scale(0.98);
        }

        button:disabled {
            opacity: 0.5;
            cursor: not-allowed;
        }

        .results {
            display: flex;
            flex-direction: column;
            gap: 15px;
        }

        .result-card {
            background: #1e293b;
            border: 1px solid #334155;
            border-radius: 16px;
            padding: 24px;
            transition: all 0.2s;
        }

        .result-card:hover {
            border-color: #3b82f6;
            box-shadow: 0 8px 24px rgba(0,0,0,0.4);
        }

        .result-url {
            color: #3b82f6;
            text-decoration: none;
            font-weight: 600;
            font-size: 1.1em;
            display: block;
            margin-bottom: 12px;
            word-break: break-all;
        }

        .result-url:hover {
            color: #60a5fa;
        }

        .result-meta {
            display: flex;
            gap: 15px;
            color: #94a3b8;
            font-size: 0.9em;
            margin-bottom: 16px;
            flex-wrap: wrap;
        }

        .result-meta span {
            display: flex;
            align-items: center;
            gap: 5px;
        }

        .result-text {
            color: #cbd5e1;
            line-height: 1.7;
            white-space: normal;
            word-wrap: break-word;
            max-height: 200px;
            overflow: hidden;
            position: relative;
        }

        .result-text.expanded {
            max-height: none;
        }

        .result-text h1,
        .result-text h2,
        .result-text h3,
        .result-text h4,
        .result-text h5,
        .result-text h6 {
            color: #f1f5f9;
            margin-top: 1em;
            margin-bottom: 0.5em;
            font-weight: 600;
        }

        .result-text h1 { font-size: 1.5em; }
        .result-text h2 { font-size: 1.3em; }
        .result-text h3 { font-size: 1.1em; }

        .result-text p {
            margin-bottom: 1em;
        }

        .result-text ul,
        .result-text ol {
            margin-left: 1.5em;
            margin-bottom: 1em;
        }

        .result-text li {
            margin-bottom: 0.5em;
        }

        .result-text code {
            background: #0f172a;
            padding: 2px 6px;
            border-radius: 4px;
            font-family: 'Courier New', monospace;
            font-size: 0.9em;
            color: #60a5fa;
        }

        .result-text pre {
            background: #0f172a;
            padding: 12px;
            border-radius: 8px;
            overflow-x: auto;
            margin-bottom: 1em;
            border: 1px solid #334155;
        }

        .result-text pre code {
            background: none;
            padding: 0;
            color: #cbd5e1;
        }

        .result-text blockquote {
            border-left: 3px solid #3b82f6;
            padding-left: 1em;
            margin: 1em 0;
            color: #94a3b8;
        }

        .result-text a {
            color: #3b82f6;
            text-decoration: underline;
        }

        .result-text a:hover {
            color: #60a5fa;
        }

        .result-text strong {
            font-weight: 600;
            color: #f1f5f9;
        }

        .result-text em {
            font-style: italic;
        }

        .result-text hr {
            border: none;
            border-top: 1px solid #334155;
            margin: 1.5em 0;
        }

        .expand-btn {
            background: none;
            color: #3b82f6;
            padding: 5px 0;
            margin-top: 12px;
            font-size: 0.9em;
            font-weight: 500;
        }

        .expand-btn:hover {
            color: #60a5fa;
            box-shadow: none;
        }

        .loading {
            text-align: center;
            color: #cbd5e1;
            font-size: 1.2em;
            padding: 40px;
        }

        .error {
            background: #991b1b;
            color: #fecaca;
            padding: 16px 20px;
            border-radius: 12px;
            margin-bottom: 20px;
            border: 1px solid #dc2626;
        }

        .no-results {
            text-align: center;
            color: #94a3b8;
            font-size: 1.2em;
            padding: 60px 20px;
            background: #1e293b;
            border-radius: 16px;
            border: 1px solid #334155;
        }
    </style>
</head>
<body>
    <div class="container">
        <h1>🧠 MindShard</h1>

        <div class="search-box">
            <form class="search-form" id="searchForm">
                <div class="search-input-wrapper">
                    <input
                        type="text"
                        id="searchInput"
                        placeholder="Search your browsing history..."
                        autocomplete="off"
                        required
                    >
                </div>
                <select id="limitSelect">
                    <option value="5">5 results</option>
                    <option value="10" selected>10 results</option>
                    <option value="20">20 results</option>
                    <option value="50">50 results</option>
                    <option value="100">100 results</option>
                </select>
                <button type="submit" id="searchBtn">Search</button>
            </form>
        </div>

        <div id="error" class="error" style="display: none;"></div>
        <div id="loading" class="loading" style="display: none;">Searching...</div>
        <div id="results" class="results"></div>
    </div>

    <script>
        const searchForm = document.getElementById('searchForm');
        const searchInput = document.getElementById('searchInput');
        const limitSelect = document.getElementById('limitSelect');
        const searchBtn = document.getElementById('searchBtn');
        const resultsDiv = document.getElementById('results');
        const loadingDiv = document.getElementById('loading');
        const errorDiv = document.getElementById('error');

        searchForm.addEventListener('submit', async (e) => {
            e.preventDefault();

            const query = searchInput.value.trim();
            if (!query) return;

            const limit = parseInt(limitSelect.value);

            resultsDiv.innerHTML = '';
            errorDiv.style.display = 'none';
            loadingDiv.style.display = 'block';
            searchBtn.disabled = true;

            try {
                const response = await fetch('/api/search', {
                    method: 'POST',
                    headers: {
                        'Content-Type': 'application/json',
                    },
                    body: JSON.stringify({ query, limit }),
                });

                if (!response.ok) {
                    throw new Error(`Search failed: ${response.statusText}`);
                }

                const data = await response.json();
                displayResults(data.results);
            } catch (error) {
                errorDiv.textContent = error.message;
                errorDiv.style.display = 'block';
            } finally {
                loadingDiv.style.display = 'none';
                searchBtn.disabled = false;
            }
        });

        function displayResults(results) {
            if (results.length === 0) {
                resultsDiv.innerHTML = '<div class="no-results">No results found</div>';
                return;
            }

            window.resultTexts = {};

            resultsDiv.innerHTML = results.map((result, index) => {
                const entry = result.entry;
                const date = new Date(entry.timestamp * 1000).toLocaleString();
                const trimmedText = entry.text.trim();
                const preview = trimmedText.length > 300
                    ? trimmedText.substring(0, 300) + '...'
                    : trimmedText;

                window.resultTexts[index] = trimmedText;

                return `
                    <div class="result-card">
                        <a href="${escapeHtml(entry.url)}" class="result-url" target="_blank">
                            ${escapeHtml(entry.url)}
                        </a>
                        <div class="result-meta">
                            <span>📅 ${date}</span>
                            <span>🌐 ${escapeHtml(entry.domain)}</span>
                        </div>
                        <div class="result-text" id="text-${index}">
                            ${marked.parse(preview)}
                        </div>
                        ${trimmedText.length > 300 ? `
                            <button class="expand-btn" onclick="toggleExpand(${index})">
                                Show more
                            </button>
                        ` : ''}
                    </div>
                `;
            }).join('');
        }

        function toggleExpand(index) {
            const textDiv = document.getElementById(`text-${index}`);
            const btn = event.target;
            const fullText = window.resultTexts[index];

            if (textDiv.classList.contains('expanded')) {
                const preview = fullText.length > 300
                    ? fullText.substring(0, 300) + '...'
                    : fullText;
                textDiv.innerHTML = marked.parse(preview);
                textDiv.classList.remove('expanded');
                btn.textContent = 'Show more';
            } else {
                textDiv.innerHTML = marked.parse(fullText);
                textDiv.classList.add('expanded');
                btn.textContent = 'Show less';
            }
        }

        function escapeHtml(text) {
            const div = document.createElement('div');
            div.textContent = text;
            return div.innerHTML;
        }
    </script>
</body>
</html>
"#;

async fn handle_request<S>(
    req: Request<hyper::body::Incoming>,
    store: Arc<S>,
) -> Result<Response<Full<Bytes>>, hyper::Error>
where
    S: SemanticStore + Send + Sync + 'static,
{
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/") => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(Full::new(Bytes::from(HTML)))
            .unwrap()),
        (&Method::POST, "/api/search") => {
            let body = req.collect().await?.to_bytes();
            let search_req: SearchRequest = match serde_json::from_slice(&body) {
                Ok(req) => req,
                Err(_) => {
                    return Ok(Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(Full::new(Bytes::from("Invalid JSON")))
                        .unwrap());
                }
            };

            match store.search(&search_req.query, search_req.limit).await {
                Ok(results) => {
                    let response = SearchResponse { results };
                    let json = serde_json::to_string(&response).unwrap();
                    Ok(Response::builder()
                        .status(StatusCode::OK)
                        .header("Content-Type", "application/json")
                        .body(Full::new(Bytes::from(json)))
                        .unwrap())
                }
                Err(e) => {
                    tracing::error!("Search error: {}", e);
                    Ok(Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Full::new(Bytes::from(format!("Search error: {e}"))))
                        .unwrap())
                }
            }
        }
        _ => Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::from("Not Found")))
            .unwrap()),
    }
}

pub async fn start_web_server<S>(port: u16, store: Arc<S>) -> Result<()>
where
    S: SemanticStore + Send + Sync + 'static,
{
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;

    tracing::info!("Web UI server listening on {}", addr);

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let store = Arc::clone(&store);

        tokio::spawn(async move {
            let service = service_fn(move |req| {
                let store = Arc::clone(&store);
                handle_request(req, store)
            });

            if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                tracing::error!("Error serving connection: {:?}", err);
            }
        });
    }
}

use anyhow::Result;
use async_trait::async_trait;
use embed_anything::config::TextEmbedConfig;
use embed_anything::embeddings::embed::{Embedder, EmbedderBuilder};
use embed_anything::Dtype;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use url::Url;
use uuid::Uuid;

/// Trait for generating embeddings from text.
#[async_trait]
pub trait EmbeddingGenerator {
    /// Generates an embedding for the given text.
    async fn generate_embedding(&self, text: &str) -> Result<Vec<f32>>;
}

pub struct EmbedAnythingProvider {
    embedder: Arc<Embedder>,
    config: TextEmbedConfig,
    pub dimensions: u16,
}

impl EmbedAnythingProvider {
    pub fn new() -> Result<Self> {
        let embedder = Arc::new(
            EmbedderBuilder::new()
                .model_architecture("model2vec")
                .model_id(Some("minishlab/potion-base-8M"))
                .revision(None)
                .token(None)
                .dtype(Some(Dtype::F32))
                .from_pretrained_hf()?,
        );
        let config = TextEmbedConfig::default();
        Ok(Self {
            embedder,
            config,
            dimensions: 256,
        })
    }
}

#[async_trait]
impl EmbeddingGenerator for EmbedAnythingProvider {
    async fn generate_embedding(&self, text: &str) -> Result<Vec<f32>> {
        let embeddings = self
            .embedder
            .embed_query(&[text], Some(&self.config))
            .await?;
        if embeddings.is_empty() {
            anyhow::bail!("No embedding returned from EmbedAnything");
        }
        embeddings[0].embedding.to_dense()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorStoreEntry {
    pub id: String,
    pub url: String,
    pub text: String,
    pub text_length: i32,
    pub timestamp: i64,
    pub domain: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub entry: VectorStoreEntry,
    pub score: f64,
}

/// Interface for vector store backends.
#[async_trait]
pub trait VectorStoreBackend {
    /// Adds a new entry with its embedding to the vector store.
    async fn add_entry(&self, entry: VectorStoreEntry, embedding: Vec<f32>) -> Result<()>;
    /// Searches the vector store for entries similar to the given query embedding. Returns at most `limit` results.
    async fn search(&self, query: Vec<f32>, limit: usize) -> Result<Vec<SearchResult>>;
    /// Retrieves all entries from the vector store with pagination support.
    async fn get_all_entries(&self, limit: usize, offset: usize) -> Result<Vec<VectorStoreEntry>>;
    /// Deletes all entries associated with the given URL.
    async fn delete_entries_by_url(&self, url: &str) -> Result<()>;
}

pub struct LibSqlProvider {
    conn: libsql::Connection,
    embedding_size: u16,
}

impl LibSqlProvider {
    pub async fn new(database_path: &str, embedding_size: u16) -> Result<Self> {
        let db = libsql::Builder::new_local(database_path).build().await?;
        let conn = db.connect()?;
        conn.query("PRAGMA journal_mode = WAL", ()).await?;
        conn.query("PRAGMA foreign_keys = ON", ()).await?;
        conn.query("PRAGMA synchronous = NORMAL", ()).await?;
        conn.query("PRAGMA temp_store = MEMORY", ()).await?;
        conn.query("PRAGMA busy_timeout = 5000", ()).await?;
        conn.query("PRAGMA journal_size_limit = 67108864", ())
            .await?;
        conn.query("PRAGMA cache_size = 2000", ()).await?;
        Ok(Self {
            conn,
            embedding_size,
        })
    }

    pub async fn init_tables(&self) -> Result<()> {
        self.conn
            .execute(
                &format!(
                    "CREATE TABLE IF NOT EXISTS entries (
                        id TEXT PRIMARY KEY,
                        url TEXT NOT NULL,
                        text TEXT NOT NULL,
                        text_length INTEGER NOT NULL,
                        timestamp INTEGER NOT NULL,
                        domain TEXT NOT NULL,
                        embedding F32_BLOB({}) NOT NULL
                    )",
                    self.embedding_size,
                ),
                (),
            )
            .await?;

        // Embedding index
        self.conn
            .execute(
                "CREATE INDEX IF NOT EXISTS entries_embedding_idx ON entries (libsql_vector_idx(embedding))",
                (),
            )
            .await?;

        // URL index
        self.conn
            .execute(
                "CREATE INDEX IF NOT EXISTS entries_url_idx ON entries (url)",
                (),
            )
            .await?;
        Ok(())
    }
}

#[async_trait]
impl VectorStoreBackend for LibSqlProvider {
    async fn add_entry(&self, entry: VectorStoreEntry, embedding: Vec<f32>) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO entries (id, url, text, text_length, timestamp, domain, embedding) VALUES (?1, ?2, ?3, ?4, ?5, ?6, vector32(?7))",
                libsql::params![
                    entry.id.as_str(),
                    entry.url.as_str(),
                    entry.text.as_str(),
                    entry.text_length,
                    entry.timestamp,
                    entry.domain.as_str(),
                    embedding_to_str(&embedding).as_str(),
                ],
            )
            .await?;
        Ok(())
    }

    async fn search(&self, query: Vec<f32>, limit: usize) -> Result<Vec<SearchResult>> {
        let mut rows = self
            .conn
            .query(
                "SELECT entries.id, url, text, text_length, timestamp, domain, vector_distance_cos(entries.embedding, vector32(?1)) AS score
                FROM vector_top_k('entries_embedding_idx', vector32(?1), ?2) AS top_k
                JOIN entries ON entries.rowid = top_k.id
                ORDER BY score ASC",
                libsql::params![embedding_to_str(&query).as_str(), limit as i64],
            )
            .await?;

        let mut results = Vec::new();
        while let Some(row) = rows.next().await? {
            results.push(SearchResult {
                entry: libsql::de::from_row::<VectorStoreEntry>(&row)?,
                score: row.get::<f64>(6)?,
            });
        }

        Ok(results)
    }

    async fn get_all_entries(&self, limit: usize, offset: usize) -> Result<Vec<VectorStoreEntry>> {
        let mut rows = self
            .conn
            .query(
                "SELECT id, url, text, text_length, timestamp, domain
                FROM entries
                LIMIT ?1
                OFFSET ?2",
                libsql::params![limit as i64, offset as i64],
            )
            .await?;

        let mut entries = Vec::new();
        while let Some(row) = rows.next().await? {
            entries.push(libsql::de::from_row::<VectorStoreEntry>(&row)?);
        }

        Ok(entries)
    }

    async fn delete_entries_by_url(&self, url: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM entries WHERE url = ?1", libsql::params![url])
            .await?;
        Ok(())
    }
}

/// High-level interface for the semantic store combining embedding generation and vector storage.
#[async_trait]
pub trait SemanticStore {
    /// Adds a new entry with the given URL and text to the semantic store.
    async fn add_entry(&self, url: &str, text: &str) -> Result<()>;
    /// Searches the semantic store for entries similar to the given query text. Returns at most `limit` results.
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>>;
    /// Retrieves all entries from the semantic store with pagination support.
    async fn get_all_entries(&self, limit: usize, offset: usize) -> Result<Vec<VectorStoreEntry>>;
    /// Deletes all entries associated with the given URL.
    async fn delete_entries_by_url(&self, url: &str) -> Result<()>;
}

pub struct VectorStore<E, B> {
    embedding_generator: E,
    backend: B,
}

impl<E, B> VectorStore<E, B>
where
    E: EmbeddingGenerator + Send + Sync,
    B: VectorStoreBackend + Send + Sync,
{
    pub fn new(embedding_generator: E, backend: B) -> Self {
        Self {
            embedding_generator,
            backend,
        }
    }
}

#[async_trait]
impl<E, B> SemanticStore for VectorStore<E, B>
where
    E: EmbeddingGenerator + Send + Sync,
    B: VectorStoreBackend + Send + Sync,
{
    async fn add_entry(&self, url: &str, text: &str) -> Result<()> {
        let url_parsed = Url::parse(url).map_err(|e| anyhow::anyhow!("Invalid URL: {e}"))?;
        let embedding = self.embedding_generator.generate_embedding(text).await?;

        let entry = VectorStoreEntry {
            id: Uuid::new_v4().to_string(),
            url: url.to_string(),
            text: text.to_string(),
            text_length: text.len() as i32,
            timestamp: chrono::Utc::now().timestamp(),
            domain: url_parsed.host_str().unwrap_or("unknown").to_string(),
        };
        self.backend.add_entry(entry, embedding).await
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let query_embedding = self.embedding_generator.generate_embedding(query).await?;
        self.backend.search(query_embedding, limit).await
    }

    async fn get_all_entries(&self, limit: usize, offset: usize) -> Result<Vec<VectorStoreEntry>> {
        self.backend.get_all_entries(limit, offset).await
    }

    async fn delete_entries_by_url(&self, url: &str) -> Result<()> {
        self.backend.delete_entries_by_url(url).await
    }
}

fn embedding_to_str(embedding: &[f32]) -> String {
    format!(
        "[{}]",
        embedding
            .iter()
            .map(|x| format!("{x}"))
            .collect::<Vec<String>>()
            .join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // Mock implementation of EmbeddingGenerator for testing
    struct MockEmbeddingGenerator {
        embeddings: HashMap<String, Vec<f32>>,
    }

    impl MockEmbeddingGenerator {
        fn new() -> Self {
            let mut embeddings = HashMap::new();
            embeddings.insert("test query".to_string(), vec![0.1, 0.2, 0.3, 0.4]);
            embeddings.insert("another query".to_string(), vec![0.5, 0.6, 0.7, 0.8]);
            embeddings.insert("sample text".to_string(), vec![0.2, 0.3, 0.4, 0.5]);
            Self { embeddings }
        }

        fn with_embeddings(embeddings: HashMap<String, Vec<f32>>) -> Self {
            Self { embeddings }
        }
    }

    #[async_trait]
    impl EmbeddingGenerator for MockEmbeddingGenerator {
        async fn generate_embedding(&self, text: &str) -> Result<Vec<f32>> {
            self.embeddings
                .get(text)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("No embedding found for text: {}", text))
        }
    }

    #[test]
    fn test_embedding_to_str() {
        let embedding = vec![0.1, 0.2, 0.3];
        let result = embedding_to_str(&embedding);
        assert_eq!(result, "[0.1,0.2,0.3]");
    }

    #[test]
    fn test_embedding_to_str_empty() {
        let embedding: Vec<f32> = vec![];
        let result = embedding_to_str(&embedding);
        assert_eq!(result, "[]");
    }

    #[test]
    fn test_embedding_to_str_single_value() {
        let embedding = vec![0.5];
        let result = embedding_to_str(&embedding);
        assert_eq!(result, "[0.5]");
    }

    #[tokio::test]
    async fn test_vector_store_add_entry() {
        let generator = MockEmbeddingGenerator::new();
        let backend = LibSqlProvider::new(":memory:", 4).await.unwrap();
        backend.init_tables().await.unwrap();
        let store = VectorStore::new(generator, backend);

        let result = store.add_entry("https://example.com", "sample text").await;
        assert!(result.is_ok());

        let entries = store.get_all_entries(10, 0).await.unwrap();
        assert_eq!(entries.len(), 1);

        let entry = &entries[0];
        assert_eq!(entry.url, "https://example.com");
        assert_eq!(entry.text, "sample text");
        assert_eq!(entry.text_length, 11);
        assert_eq!(entry.domain, "example.com");
    }

    #[tokio::test]
    async fn test_vector_store_add_entry_invalid_url() {
        let generator = MockEmbeddingGenerator::new();
        let backend = LibSqlProvider::new(":memory:", 4).await.unwrap();
        backend.init_tables().await.unwrap();
        let store = VectorStore::new(generator, backend);

        let result = store.add_entry("not a valid url", "sample text").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid URL"));
    }

    #[tokio::test]
    async fn test_vector_store_search() {
        let generator = MockEmbeddingGenerator::new();
        let backend = LibSqlProvider::new(":memory:", 4).await.unwrap();
        backend.init_tables().await.unwrap();
        let store = VectorStore::new(generator, backend);

        store
            .add_entry("https://example.com/1", "sample text")
            .await
            .unwrap();
        store
            .add_entry("https://example.com/2", "sample text")
            .await
            .unwrap();

        let results = store.search("test query", 10).await.unwrap();

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.entry.text == "sample text"));
    }

    #[tokio::test]
    async fn test_vector_store_search_empty_query() {
        let generator = MockEmbeddingGenerator::new();
        let backend = LibSqlProvider::new(":memory:", 4).await.unwrap();
        backend.init_tables().await.unwrap();
        let store = VectorStore::new(generator, backend);

        let results = store.search("", 10).await.unwrap();
        assert_eq!(results.len(), 0);
    }

    #[tokio::test]
    async fn test_vector_store_search_whitespace_query() {
        let generator = MockEmbeddingGenerator::new();
        let backend = LibSqlProvider::new(":memory:", 4).await.unwrap();
        backend.init_tables().await.unwrap();
        let store = VectorStore::new(generator, backend);

        let results = store.search("   ", 10).await.unwrap();
        assert_eq!(results.len(), 0);
    }

    #[tokio::test]
    async fn test_vector_store_search_with_limit() {
        let generator = MockEmbeddingGenerator::new();
        let backend = LibSqlProvider::new(":memory:", 4).await.unwrap();
        backend.init_tables().await.unwrap();
        let store = VectorStore::new(generator, backend);

        for i in 0..5 {
            store
                .add_entry(&format!("https://example.com/{}", i), "sample text")
                .await
                .unwrap();
        }

        let results = store.search("test query", 2).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn test_vector_store_get_all_entries() {
        let generator = MockEmbeddingGenerator::new();
        let backend = LibSqlProvider::new(":memory:", 4).await.unwrap();
        backend.init_tables().await.unwrap();
        let store = VectorStore::new(generator, backend);

        store
            .add_entry("https://example.com/1", "sample text")
            .await
            .unwrap();
        store
            .add_entry("https://example.com/2", "sample text")
            .await
            .unwrap();
        store
            .add_entry("https://example.com/3", "sample text")
            .await
            .unwrap();

        let entries = store.get_all_entries(10, 0).await.unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[tokio::test]
    async fn test_vector_store_get_all_entries_with_pagination() {
        let generator = MockEmbeddingGenerator::new();
        let backend = LibSqlProvider::new(":memory:", 4).await.unwrap();
        backend.init_tables().await.unwrap();
        let store = VectorStore::new(generator, backend);

        for i in 0..5 {
            store
                .add_entry(&format!("https://example.com/{}", i), "sample text")
                .await
                .unwrap();
        }

        let page1 = store.get_all_entries(2, 0).await.unwrap();
        assert_eq!(page1.len(), 2);

        let page2 = store.get_all_entries(2, 2).await.unwrap();
        assert_eq!(page2.len(), 2);

        let page3 = store.get_all_entries(2, 4).await.unwrap();
        assert_eq!(page3.len(), 1);
    }

    #[tokio::test]
    async fn test_vector_store_delete_entries_by_url() {
        let generator = MockEmbeddingGenerator::new();
        let backend = LibSqlProvider::new(":memory:", 4).await.unwrap();
        backend.init_tables().await.unwrap();
        let store = VectorStore::new(generator, backend);

        store
            .add_entry("https://example.com/delete", "sample text")
            .await
            .unwrap();
        store
            .add_entry("https://example.com/keep", "sample text")
            .await
            .unwrap();

        store
            .delete_entries_by_url("https://example.com/delete")
            .await
            .unwrap();

        let entries = store.get_all_entries(10, 0).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].url, "https://example.com/keep");
    }
}

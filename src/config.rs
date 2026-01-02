use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub proxy_port: u16,
    pub web_port: u16,
    pub db_path: String,
    pub private_key_path: PathBuf,
    pub ca_cert_path: PathBuf,
    pub chunk_size: usize,
    pub browser_only: bool,
    pub host_exclusion_patterns: Vec<String>,
}

impl Config {
    pub fn private_key_bytes(&self) -> std::io::Result<Vec<u8>> {
        std::fs::read(&self.private_key_path)
    }

    pub fn ca_cert_bytes(&self) -> std::io::Result<Vec<u8>> {
        std::fs::read(&self.ca_cert_path)
    }

    pub fn compile_host_exclusion_patterns(&self) -> Result<Vec<regex::Regex>, regex::Error> {
        self.host_exclusion_patterns
            .iter()
            .map(|pattern| regex::Regex::new(pattern))
            .collect()
    }
}

impl Default for Config {
    fn default() -> Self {
        let host_exclusion_patterns = std::env::var("MINDSHARD_HOST_EXCLUSION_PATTERNS")
            .ok()
            .map(|patterns_str| {
                if patterns_str.trim().is_empty() {
                    Vec::new()
                } else {
                    patterns_str
                        .split(',')
                        .map(|pattern| pattern.trim().to_string())
                        .collect()
                }
            })
            .unwrap_or_default();

        Self {
            proxy_port: std::env::var("MINDSHARD_PROXY_PORT")
                .unwrap_or_else(|_| "8080".to_string())
                .parse()
                .unwrap_or(8080),
            web_port: std::env::var("MINDSHARD_WEB_PORT")
                .unwrap_or_else(|_| "3000".to_string())
                .parse()
                .unwrap_or(3000),
            db_path: std::env::var("MINDSHARD_DB_PATH")
                .unwrap_or_else(|_| "mindshard.db".to_string()),
            private_key_path: std::env::var("MINDSHARD_PRIVATE_KEY_PATH")
                .unwrap_or_else(|_| "mindshard.key".to_string())
                .into(),
            ca_cert_path: std::env::var("MINDSHARD_CA_CERT_PATH")
                .unwrap_or_else(|_| "mindshard.cer".to_string())
                .into(),
            chunk_size: 1000,
            browser_only: std::env::var("MINDSHARD_BROWSER_ONLY")
                .unwrap_or_else(|_| "true".to_string())
                .parse()
                .unwrap_or(true),
            host_exclusion_patterns,
        }
    }
}

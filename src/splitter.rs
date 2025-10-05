use anyhow::Result;
use processors_rs::{html_processor::HtmlProcessor, processor::DocumentProcessor};

/// A trait for splitting content into chunks.
pub trait Splitter {
    /// Splits the given string into chunks.
    fn split(&self, content: &str) -> Result<Vec<String>>;
}

pub struct HtmlSplitter {
    html_processor: HtmlProcessor,
}

impl HtmlSplitter {
    pub fn new(chunk_size: usize) -> Result<Self> {
        let html_processor = HtmlProcessor::new(chunk_size, 0)?;
        Ok(Self { html_processor })
    }
}

impl Splitter for HtmlSplitter {
    fn split(&self, content: &str) -> Result<Vec<String>> {
        self.html_processor
            .process_document(content)
            .map(|doc| doc.chunks)
    }
}

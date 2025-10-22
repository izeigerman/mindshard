use anyhow::Result;
use htmd::HtmlToMarkdown;
use text_splitter::{Characters, MarkdownSplitter};

/// A trait for splitting content into chunks.
pub trait Splitter {
    /// Splits the given string into chunks.
    fn split(&self, content: &str) -> Result<Vec<String>>;
}

pub struct HtmlSplitter {
    html_to_markdown: HtmlToMarkdown,
    markdown_splitter: MarkdownSplitter<Characters>,
}

impl HtmlSplitter {
    pub fn new(chunk_size: usize) -> Result<Self> {
        let html_to_markdown = HtmlToMarkdown::builder()
            .skip_tags(vec!["script", "style"])
            .build();
        let markdown_splitter = MarkdownSplitter::new(chunk_size);
        Ok(Self {
            html_to_markdown,
            markdown_splitter,
        })
    }
}

impl Splitter for HtmlSplitter {
    fn split(&self, content: &str) -> Result<Vec<String>> {
        let markdown = self.html_to_markdown.convert(content)?;
        let chunks = self
            .markdown_splitter
            .chunks(&markdown)
            .map(|s| s.to_string())
            .collect();
        Ok(chunks)
    }
}

//! SizeTagger — classifies files by byte count.
//!
//! - `size:small`  — < 1 KB
//! - `size:medium` — 1–10 KB
//! - `size:large`  — > 10 KB

use std::path::Path;

use biem_core::enrich::{EnrichError, Tagger, TaggerResult};
use biem_core::types::{ParseResult, SourceType};

pub struct SizeTagger;

impl Tagger for SizeTagger {
    fn name(&self) -> &str {
        "size"
    }

    fn applies_to(&self, _source: &SourceType) -> bool {
        true
    }

    fn tag(
        &self,
        _path: &Path,
        content: &[u8],
        _parse_result: &ParseResult,
    ) -> Result<TaggerResult, EnrichError> {
        let tag = match content.len() {
            0..=1023 => "size:small",
            1024..=10240 => "size:medium",
            _ => "size:large",
        };
        Ok(TaggerResult {
            tagger_name: self.name().into(),
            tags: vec![tag.into()],
            confidence: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn empty_pr() -> ParseResult {
        ParseResult {
            chunks: vec![], tags: vec![], links: vec![],
            auto_type: None, frontmatter: HashMap::new(),
        }
    }

    #[test]
    fn small_file() {
        let r = SizeTagger.tag(Path::new("x.md"), b"hello", &empty_pr()).unwrap();
        assert_eq!(r.tags, vec!["size:small"]);
    }

    #[test]
    fn medium_file() {
        let content = vec![b'x'; 5000];
        let r = SizeTagger.tag(Path::new("x.md"), &content, &empty_pr()).unwrap();
        assert_eq!(r.tags, vec!["size:medium"]);
    }

    #[test]
    fn large_file() {
        let content = vec![b'x'; 20_000];
        let r = SizeTagger.tag(Path::new("x.md"), &content, &empty_pr()).unwrap();
        assert_eq!(r.tags, vec!["size:large"]);
    }
}

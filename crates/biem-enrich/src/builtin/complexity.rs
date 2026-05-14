//! ComplexityTagger — heuristic complexity classification.
//!
//! - `complexity:low`    — ≤ 3 chunks, shallow nesting
//! - `complexity:medium` — 4–10 chunks
//! - `complexity:high`   — > 10 chunks or deep nesting (depth ≥ 4)

use std::path::Path;

use biem_core::enrich::{EnrichError, Tagger, TaggerResult};
use biem_core::types::{ParseResult, SourceType};

pub struct ComplexityTagger;

impl Tagger for ComplexityTagger {
    fn name(&self) -> &str {
        "complexity"
    }

    fn applies_to(&self, _source: &SourceType) -> bool {
        true
    }

    fn tag(
        &self,
        _path: &Path,
        _content: &[u8],
        parse_result: &ParseResult,
    ) -> Result<TaggerResult, EnrichError> {
        let chunk_count = parse_result.chunks.len();
        let max_depth = parse_result
            .chunks
            .iter()
            .map(|c| c.depth)
            .max()
            .unwrap_or(0);

        let tag = if chunk_count > 10 || max_depth >= 4 {
            "complexity:high"
        } else if chunk_count > 3 {
            "complexity:medium"
        } else {
            "complexity:low"
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
    use biem_core::types::{Chunk, ChunkKind, ChunkMetadata};
    use std::collections::HashMap;

    fn pr_with_chunks(count: usize, depth: u8) -> ParseResult {
        let chunks = (0..count)
            .map(|i| Chunk {
                byte_range: (i * 100)..(i * 100 + 50),
                kind: ChunkKind::Section,
                label: None,
                depth,
                metadata: ChunkMetadata::default(),
            })
            .collect();
        ParseResult {
            chunks,
            tags: vec![],
            links: vec![],
            auto_type: None,
            frontmatter: HashMap::new(),
        }
    }

    #[test]
    fn low_complexity() {
        let r = ComplexityTagger
            .tag(Path::new("x.md"), b"", &pr_with_chunks(2, 1))
            .unwrap();
        assert_eq!(r.tags, vec!["complexity:low"]);
    }

    #[test]
    fn medium_complexity() {
        let r = ComplexityTagger
            .tag(Path::new("x.md"), b"", &pr_with_chunks(6, 2))
            .unwrap();
        assert_eq!(r.tags, vec!["complexity:medium"]);
    }

    #[test]
    fn high_complexity_by_count() {
        let r = ComplexityTagger
            .tag(Path::new("x.md"), b"", &pr_with_chunks(15, 2))
            .unwrap();
        assert_eq!(r.tags, vec!["complexity:high"]);
    }

    #[test]
    fn high_complexity_by_depth() {
        let r = ComplexityTagger
            .tag(Path::new("x.md"), b"", &pr_with_chunks(3, 5))
            .unwrap();
        assert_eq!(r.tags, vec!["complexity:high"]);
    }
}

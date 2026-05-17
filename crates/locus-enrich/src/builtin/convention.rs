//! ConventionTagger — infers conventions from file paths.
//!
//! - `convention:test`      — `*_test.*`, `*_spec.*`, `test_*`, `tests/`
//! - `convention:config`    — `*.config.*`, `*.toml`, `*.yaml`, `*.yml`, `*.json` in root-ish dirs
//! - `convention:migration` — `migrations/`, `migrate/`
//! - `convention:readme`    — `README*`, `readme*`
//! - `convention:docs`      — `docs/`, `doc/`

use std::path::Path;

use locus_core::enrich::{EnrichError, Tagger, TaggerResult};
use locus_core::types::{ParseResult, SourceType};

pub struct ConventionTagger;

impl Tagger for ConventionTagger {
    fn name(&self) -> &str {
        "convention"
    }

    fn applies_to(&self, _source: &SourceType) -> bool {
        true
    }

    fn tag(
        &self,
        path: &Path,
        _content: &[u8],
        _parse_result: &ParseResult,
    ) -> Result<TaggerResult, EnrichError> {
        let mut tags = Vec::new();
        let path_str = path.to_string_lossy().to_lowercase();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        // Test files
        if stem.ends_with("_test")
            || stem.ends_with("_spec")
            || stem.starts_with("test_")
            || path_str.contains("tests/")
            || path_str.contains("test/")
            || path_str.contains("__tests__/")
        {
            tags.push("convention:test".into());
        }

        // Config files
        if file_name.contains(".config.")
            || file_name == "config.toml"
            || file_name == "config.yaml"
            || file_name == "config.yml"
            || file_name == "config.json"
            || file_name == ".eslintrc.json"
            || file_name == "tsconfig.json"
            || file_name == "cargo.toml"
            || file_name == "package.json"
        {
            tags.push("convention:config".into());
        }

        // Migrations
        if path_str.contains("migrations/") || path_str.contains("migrate/") {
            tags.push("convention:migration".into());
        }

        // Readme
        if stem.starts_with("readme") {
            tags.push("convention:readme".into());
        }

        // Docs
        if path_str.contains("docs/") || path_str.contains("doc/") {
            tags.push("convention:docs".into());
        }

        Ok(TaggerResult {
            tagger_name: self.name().into(),
            tags,
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
    fn detects_test_file() {
        let r = ConventionTagger.tag(Path::new("src/auth_test.rs"), b"", &empty_pr()).unwrap();
        assert!(r.tags.contains(&"convention:test".to_string()));
    }

    #[test]
    fn detects_tests_dir() {
        let r = ConventionTagger.tag(Path::new("tests/integration.rs"), b"", &empty_pr()).unwrap();
        assert!(r.tags.contains(&"convention:test".to_string()));
    }

    #[test]
    fn detects_config() {
        let r = ConventionTagger.tag(Path::new("Cargo.toml"), b"", &empty_pr()).unwrap();
        assert!(r.tags.contains(&"convention:config".to_string()));
    }

    #[test]
    fn detects_readme() {
        let r = ConventionTagger.tag(Path::new("README.md"), b"", &empty_pr()).unwrap();
        assert!(r.tags.contains(&"convention:readme".to_string()));
    }

    #[test]
    fn detects_docs() {
        let r = ConventionTagger.tag(Path::new("docs/architecture/overview.md"), b"", &empty_pr()).unwrap();
        assert!(r.tags.contains(&"convention:docs".to_string()));
    }

    #[test]
    fn no_match_returns_empty() {
        let r = ConventionTagger.tag(Path::new("src/main.rs"), b"", &empty_pr()).unwrap();
        assert!(r.tags.is_empty());
    }
}

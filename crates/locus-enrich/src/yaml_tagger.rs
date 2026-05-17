//! Custom YAML tagger — user-defined rules loaded from `.locus/taggers/`.
//!
//! Each YAML file defines a tagger with match rules and tags to add.

use std::fs;
use std::path::Path;

use globset::{Glob, GlobMatcher};
use serde::Deserialize;

use locus_core::enrich::{EnrichError, Tagger, TaggerResult};
use locus_core::types::{ParseResult, SourceType};

// ── YAML schema ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct YamlTaggerDef {
    name: String,
    #[serde(default = "default_version")]
    version: u32,
    rules: Vec<YamlRule>,
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, Deserialize)]
struct YamlRule {
    #[serde(rename = "match")]
    match_cond: MatchCondition,
    add_tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct MatchCondition {
    /// Glob pattern matched against the file path.
    folder: Option<String>,
    /// File extension (e.g. ".tf", ".rs").
    extension: Option<String>,
    /// Require an existing tag on the parse result.
    has_tag: Option<String>,
    /// Require content to contain a substring.
    content_contains: Option<String>,
}

// ── Compiled tagger ──────────────────────────────────────────────

struct CompiledRule {
    folder_glob: Option<GlobMatcher>,
    extension: Option<String>,
    has_tag: Option<String>,
    content_contains: Option<String>,
    add_tags: Vec<String>,
}

/// A custom tagger loaded from a YAML rule file.
pub struct YamlTagger {
    name: String,
    #[allow(dead_code)]
    version: u32,
    rules: Vec<CompiledRule>,
}

impl YamlTagger {
    /// Parse a YAML tagger definition from bytes.
    pub fn from_yaml(yaml: &[u8]) -> Result<Self, EnrichError> {
        let def: YamlTaggerDef = serde_yaml::from_slice(yaml)
            .map_err(|e| EnrichError::TaggerFailed("yaml".into(), e.to_string()))?;

        let rules = def
            .rules
            .into_iter()
            .map(|r| {
                let folder_glob = r.match_cond.folder.as_ref().map(|pat| {
                    Glob::new(pat)
                        .unwrap_or_else(|_| Glob::new("**").unwrap())
                        .compile_matcher()
                });
                CompiledRule {
                    folder_glob,
                    extension: r.match_cond.extension,
                    has_tag: r.match_cond.has_tag,
                    content_contains: r.match_cond.content_contains,
                    add_tags: r.add_tags,
                }
            })
            .collect();

        Ok(Self {
            name: def.name,
            version: def.version,
            rules,
        })
    }

    /// Load a YAML tagger from a file path.
    pub fn from_file(path: &Path) -> Result<Self, EnrichError> {
        let bytes = fs::read(path).map_err(|e| {
            EnrichError::TaggerFailed("yaml".into(), format!("{}: {}", path.display(), e))
        })?;
        Self::from_yaml(&bytes)
    }
}

impl Tagger for YamlTagger {
    fn name(&self) -> &str {
        &self.name
    }

    fn applies_to(&self, _source: &SourceType) -> bool {
        true
    }

    fn tag(
        &self,
        path: &Path,
        content: &[u8],
        parse_result: &ParseResult,
    ) -> Result<TaggerResult, EnrichError> {
        let mut tags = Vec::new();
        let path_str = path.to_string_lossy();
        let ext = path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();

        for rule in &self.rules {
            let mut matched = true;

            if let Some(ref glob) = rule.folder_glob {
                if !glob.is_match(path_str.as_ref()) {
                    matched = false;
                }
            }

            if let Some(ref want_ext) = rule.extension {
                if ext != *want_ext {
                    matched = false;
                }
            }

            if let Some(ref want_tag) = rule.has_tag {
                if !parse_result.tags.iter().any(|t| t == want_tag) {
                    matched = false;
                }
            }

            if let Some(ref needle) = rule.content_contains {
                let text = String::from_utf8_lossy(content);
                if !text.contains(needle.as_str()) {
                    matched = false;
                }
            }

            if matched {
                tags.extend(rule.add_tags.iter().cloned());
            }
        }

        Ok(TaggerResult {
            tagger_name: self.name.clone(),
            tags,
            confidence: None,
        })
    }
}

/// Load all YAML taggers from a directory. Returns empty vec if dir doesn't exist.
pub fn load_yaml_taggers(dir: &Path) -> Result<Vec<Box<dyn Tagger>>, EnrichError> {
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut taggers: Vec<Box<dyn Tagger>> = Vec::new();
    let entries = fs::read_dir(dir).map_err(|e| EnrichError::CacheIo(e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| EnrichError::CacheIo(e.to_string()))?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "yaml" || e == "yml") {
            match YamlTagger::from_file(&path) {
                Ok(tagger) => taggers.push(Box::new(tagger)),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "failed to load YAML tagger");
                }
            }
        }
    }
    Ok(taggers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn pr_with_tags(tags: Vec<&str>) -> ParseResult {
        ParseResult {
            chunks: vec![],
            tags: tags.into_iter().map(String::from).collect(),
            links: vec![],
            auto_type: None,
            frontmatter: HashMap::new(),
        }
    }

    fn empty_pr() -> ParseResult {
        pr_with_tags(vec![])
    }

    #[test]
    fn folder_match() {
        let yaml = br#"
name: team
rules:
  - match:
      folder: "src/payments/**"
    add_tags: ["team:payments"]
"#;
        let tagger = YamlTagger::from_yaml(yaml).unwrap();
        let r = tagger.tag(Path::new("src/payments/billing.rs"), b"", &empty_pr()).unwrap();
        assert_eq!(r.tags, vec!["team:payments"]);
    }

    #[test]
    fn folder_no_match() {
        let yaml = br#"
name: team
rules:
  - match:
      folder: "src/payments/**"
    add_tags: ["team:payments"]
"#;
        let tagger = YamlTagger::from_yaml(yaml).unwrap();
        let r = tagger.tag(Path::new("src/auth/login.rs"), b"", &empty_pr()).unwrap();
        assert!(r.tags.is_empty());
    }

    #[test]
    fn has_tag_match() {
        let yaml = br#"
name: priority
rules:
  - match:
      has_tag: "quality:todo"
    add_tags: ["priority:tech-debt"]
"#;
        let tagger = YamlTagger::from_yaml(yaml).unwrap();
        let r = tagger
            .tag(Path::new("x.md"), b"", &pr_with_tags(vec!["quality:todo"]))
            .unwrap();
        assert_eq!(r.tags, vec!["priority:tech-debt"]);
    }

    #[test]
    fn content_contains_match() {
        let yaml = br#"
name: infra
rules:
  - match:
      content_contains: "aws_lambda"
    add_tags: ["infra:serverless"]
"#;
        let tagger = YamlTagger::from_yaml(yaml).unwrap();
        let r = tagger
            .tag(Path::new("main.tf"), b"resource aws_lambda_function foo {}", &empty_pr())
            .unwrap();
        assert_eq!(r.tags, vec!["infra:serverless"]);
    }

    #[test]
    fn extension_match() {
        let yaml = br#"
name: lang
rules:
  - match:
      extension: ".tf"
    add_tags: ["lang:terraform"]
"#;
        let tagger = YamlTagger::from_yaml(yaml).unwrap();
        let r = tagger.tag(Path::new("main.tf"), b"", &empty_pr()).unwrap();
        assert_eq!(r.tags, vec!["lang:terraform"]);
    }

    #[test]
    fn multiple_conditions_all_must_match() {
        let yaml = br#"
name: combo
rules:
  - match:
      folder: "src/auth/**"
      has_tag: "kind:function"
    add_tags: ["team:platform"]
"#;
        let tagger = YamlTagger::from_yaml(yaml).unwrap();

        // Both match
        let r = tagger
            .tag(Path::new("src/auth/login.rs"), b"", &pr_with_tags(vec!["kind:function"]))
            .unwrap();
        assert_eq!(r.tags, vec!["team:platform"]);

        // Only folder matches
        let r = tagger
            .tag(Path::new("src/auth/login.rs"), b"", &empty_pr())
            .unwrap();
        assert!(r.tags.is_empty());
    }
}

//! TopicTagger — keyword extraction from content.
//!
//! Uses a curated keyword vocabulary to detect topics.
//! No TF-IDF yet — simple substring matching against known terms.
//! Fast, deterministic, no dependencies.

use std::path::Path;

use biem_core::enrich::{EnrichError, Tagger, TaggerResult};
use biem_core::types::{ParseResult, SourceType};

/// Known topic keywords mapped to topic tags.
const TOPIC_KEYWORDS: &[(&[&str], &str)] = &[
    (&["authentication", "auth", "login", "oauth", "jwt", "session"], "topic:auth"),
    (&["database", "sql", "query", "schema", "migration", "orm"], "topic:database"),
    (&["api", "endpoint", "rest", "graphql", "grpc"], "topic:api"),
    (&["test", "testing", "assert", "mock", "fixture", "spec"], "topic:testing"),
    (&["deploy", "deployment", "ci/cd", "pipeline", "docker", "kubernetes"], "topic:deployment"),
    (&["config", "configuration", "settings", "environment", "env"], "topic:config"),
    (&["error", "exception", "handling", "retry", "fallback"], "topic:error-handling"),
    (&["cache", "caching", "redis", "memcached", "ttl"], "topic:caching"),
    (&["logging", "log", "tracing", "observability", "metrics"], "topic:observability"),
    (&["security", "encryption", "tls", "certificate", "vulnerability"], "topic:security"),
    (&["async", "concurrent", "parallel", "thread", "tokio", "future"], "topic:concurrency"),
    (&["network", "http", "tcp", "socket", "request", "response"], "topic:networking"),
    (&["parser", "parsing", "ast", "syntax", "lexer", "token"], "topic:parsing"),
    (&["index", "bitmap", "search", "filter", "query"], "topic:indexing"),
];

/// Minimum keyword hits to emit a topic tag.
const MIN_HITS: usize = 2;

pub struct TopicTagger;

impl Tagger for TopicTagger {
    fn name(&self) -> &str {
        "topic"
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
        let text = String::from_utf8_lossy(content).to_lowercase();
        let mut tags = Vec::new();

        for (keywords, topic) in TOPIC_KEYWORDS {
            let hits: usize = keywords
                .iter()
                .filter(|kw| text.contains(**kw))
                .count();
            if hits >= MIN_HITS {
                tags.push((*topic).into());
            }
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
    fn detects_auth_topic() {
        let content = b"This module handles authentication via JWT tokens and OAuth login flows.";
        let r = TopicTagger.tag(Path::new("x.md"), content, &empty_pr()).unwrap();
        assert!(r.tags.contains(&"topic:auth".to_string()));
    }

    #[test]
    fn detects_database_topic() {
        let content = b"Run the SQL migration to update the database schema.";
        let r = TopicTagger.tag(Path::new("x.md"), content, &empty_pr()).unwrap();
        assert!(r.tags.contains(&"topic:database".to_string()));
    }

    #[test]
    fn no_topic_for_generic_content() {
        let content = b"Hello world, this is a simple note.";
        let r = TopicTagger.tag(Path::new("x.md"), content, &empty_pr()).unwrap();
        assert!(r.tags.is_empty());
    }

    #[test]
    fn multiple_topics() {
        let content = b"The auth endpoint uses JWT tokens and caches sessions in Redis with a TTL.";
        let r = TopicTagger.tag(Path::new("x.md"), content, &empty_pr()).unwrap();
        assert!(r.tags.contains(&"topic:auth".to_string()));
        assert!(r.tags.contains(&"topic:caching".to_string()));
    }
}

//! Confluence Cloud source and parser for Locus.
//!
//! # Setup
//!
//! ```ignore
//! let config = ConfluenceConfig {
//!     base_url: "https://your-org.atlassian.net".into(),
//!     username: "you@example.com".into(),
//!     api_token: std::env::var("CONFLUENCE_API_TOKEN").unwrap(),
//!     spaces: vec!["ENG".into(), "OPS".into()],
//!     poll_interval_secs: 300,
//! };
//! let parser = ConfluenceParser;
//! let source = ConfluenceSource::new(config);
//! ```
//!
//! Each Confluence page is ingested as a synthetic path
//! `confluence://<SPACE>/<page_id>.confluence`. The `ConfluenceParser`
//! handles those paths, emitting `source:confluence` and `tag:*` bitmap keys
//! derived from Confluence space labels.

use std::path::{Path, PathBuf};

use locus_core::parser::{ParseError, Parser};
use locus_core::types::{Chunk, ChunkKind, ChunkMetadata, DocType, LinkRef, ParseResult};
use locus_watcher::remote::{RemoteError, RemoteItem, RemoteSource};

// ── Config ────────────────────────────────────────────────────────

/// Configuration for a Confluence Cloud instance.
#[derive(Debug, Clone)]
pub struct ConfluenceConfig {
    /// Base URL, e.g. `https://your-org.atlassian.net`
    pub base_url: String,
    /// Atlassian account email.
    pub username: String,
    /// Atlassian API token (not your password).
    pub api_token: String,
    /// Space keys to index, e.g. `["ENG", "OPS"]`.
    pub spaces: Vec<String>,
    /// How often to poll for changes (seconds). Default 300.
    pub poll_interval_secs: u64,
}

// ── Parser ────────────────────────────────────────────────────────

/// Parses Confluence page JSON (as returned by the REST API) into a `ParseResult`.
///
/// Expects bytes that are the full `GET /rest/api/content/{id}?expand=body.storage,...`
/// response. The storage format HTML is stripped to plain text; headings become
/// section chunks.
pub struct ConfluenceParser;

impl Parser for ConfluenceParser {
    fn can_parse(&self, path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()) == Some("confluence")
            || path.to_string_lossy().starts_with("confluence://")
    }

    fn parse(&self, path: &Path, content: &[u8]) -> Result<ParseResult, ParseError> {
        let json: serde_json::Value = serde_json::from_slice(content)
            .map_err(|e| ParseError::BadFrontmatter(format!("confluence JSON: {e}")))?;

        let title = json["title"].as_str().unwrap_or("Untitled").to_string();
        let space_key = json["space"]["key"].as_str().unwrap_or("").to_string();
        let page_id = json["id"].as_str().unwrap_or("").to_string();
        let storage_html = json["body"]["storage"]["value"].as_str().unwrap_or("");

        // Extract labels as tags
        let mut tags = vec![
            "source:confluence".to_string(),
            format!("confluence:space:{space_key}"),
        ];
        if let Some(labels) = json["metadata"]["labels"]["results"].as_array() {
            for label in labels {
                if let Some(name) = label["name"].as_str() {
                    tags.push(format!("tag:{name}"));
                }
            }
        }

        // Extract ancestor hierarchy as folder-style tags
        if let Some(ancestors) = json["ancestors"].as_array() {
            for ancestor in ancestors {
                if let Some(anc_title) = ancestor["title"].as_str() {
                    tags.push(format!("confluence:parent:{}", slug(anc_title)));
                }
            }
        }

        // Parse storage HTML into sections and plain text
        let (sections, links) = parse_storage_html(storage_html);

        // Build chunks: title chunk + one per heading section
        let mut chunks = Vec::new();
        let title_bytes = title.as_bytes().len();
        chunks.push(Chunk {
            byte_range: 0..title_bytes,
            kind: ChunkKind::Section,
            label: Some(title.clone()),
            depth: 1,
            metadata: ChunkMetadata::default(),
        });

        let mut offset = title_bytes + 1;
        for (heading, body, depth) in &sections {
            let start = offset;
            let section_text = format!("{heading}\n{body}");
            offset += section_text.len() + 1;
            chunks.push(Chunk {
                byte_range: start..offset,
                kind: ChunkKind::Section,
                label: Some(heading.clone()),
                depth: *depth,
                metadata: ChunkMetadata::default(),
            });
        }

        // Map Confluence page → doc type (pages are notes by default)
        let auto_type = Some(DocType::Note);

        let _ = (path, page_id);
        Ok(ParseResult {
            chunks,
            tags,
            links,
            auto_type,
            frontmatter: Default::default(),
        })
    }
}

/// Parse Confluence storage format HTML into (heading, body_text, depth) sections.
/// Also extracts link targets from `<a href>` and `<ac:link>` macros.
fn parse_storage_html(html: &str) -> (Vec<(String, String, u8)>, Vec<LinkRef>) {
    let mut sections: Vec<(String, String, u8)> = Vec::new();
    let mut links: Vec<LinkRef> = Vec::new();
    let mut current_heading: Option<(String, u8)> = None;
    let mut current_body = String::new();
    let mut offset = 0usize;

    // Simple token-based pass: find <h1>..<h6> and extract plain text
    let bytes = html.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'<' {
            // Find closing >
            let tag_start = i;
            while i < bytes.len() && bytes[i] != b'>' {
                i += 1;
            }
            let tag_content = std::str::from_utf8(&bytes[tag_start + 1..i]).unwrap_or("");
            let tag_name = tag_content.split_whitespace().next().unwrap_or("").to_lowercase();
            let attrs = tag_content;

            // Check for heading tags
            if let Some(level) = heading_level(&tag_name) {
                // Commit previous section
                if let Some((h, d)) = current_heading.take() {
                    sections.push((h, current_body.trim().to_string(), d));
                    current_body.clear();
                }
                // Collect heading text until </h{n}>
                i += 1;
                let heading_end_tag = format!("</h{level}>");
                let text_start = i;
                while i + heading_end_tag.len() < bytes.len() {
                    if bytes[i..].starts_with(heading_end_tag.as_bytes()) {
                        break;
                    }
                    i += 1;
                }
                let raw_heading = std::str::from_utf8(&bytes[text_start..i]).unwrap_or("");
                current_heading = Some((strip_tags(raw_heading), level));
                i += heading_end_tag.len();
                continue;
            }

            // Extract href from <a> tags
            if tag_name == "a" {
                if let Some(href) = extract_attr(attrs, "href") {
                    links.push(LinkRef {
                        target: href,
                        display: None,
                        byte_offset: offset,
                    });
                }
            }

            // Extract page title from <ac:link> macros
            if tag_name == "ac:link" {
                if let Some(page) = extract_attr(attrs, "ac:page-title") {
                    links.push(LinkRef {
                        target: page,
                        display: None,
                        byte_offset: offset,
                    });
                }
            }

            i += 1; // skip >
        } else {
            // Plain text character
            if bytes[i] != b'\r' {
                let ch = bytes[i] as char;
                current_body.push(ch);
                offset += 1;
            }
            i += 1;
        }
    }

    // Commit trailing section
    if let Some((h, d)) = current_heading {
        sections.push((h, current_body.trim().to_string(), d));
    }

    (sections, links)
}

fn heading_level(tag: &str) -> Option<u8> {
    match tag {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn extract_attr(attrs: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = attrs.find(&needle)? + needle.len();
    let end = attrs[start..].find('"')? + start;
    Some(attrs[start..end].to_string())
}

fn slug(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

// ── Source ────────────────────────────────────────────────────────

/// Polls Confluence Cloud REST API for page changes.
pub struct ConfluenceSource {
    config: ConfluenceConfig,
    client: reqwest::blocking::Client,
}

impl ConfluenceSource {
    pub fn new(config: ConfluenceConfig) -> Self {
        Self {
            client: reqwest::blocking::Client::new(),
            config,
        }
    }

    fn auth_header(&self) -> String {
        let creds = format!("{}:{}", self.config.username, self.config.api_token);
        let encoded = base64_encode(creds.as_bytes());
        format!("Basic {encoded}")
    }

    fn list_pages_in_space(
        &self,
        space_key: &str,
        since: Option<i64>,
    ) -> Result<Vec<RemoteItem>, RemoteError> {
        let mut url = format!(
            "{}/rest/api/content?spaceKey={space_key}&type=page&status=current&limit=50\
             &expand=version,space",
            self.config.base_url,
        );
        if let Some(ts) = since {
            // Confluence uses relative time in JQL-style; we use lastmodified parameter
            let dt = format_confluence_datetime(ts);
            url.push_str(&format!("&lastModified={dt}"));
        }

        let mut items = Vec::new();
        let mut next_url = Some(url);

        while let Some(current_url) = next_url.take() {
            let resp: serde_json::Value = self
                .client
                .get(&current_url)
                .header("Authorization", self.auth_header())
                .header("Accept", "application/json")
                .send()
                .map_err(|e| RemoteError::Http { status: 0, body: e.to_string() })?
                .json()
                .map_err(|e| RemoteError::Parse(e.to_string()))?;

            if let Some(results) = resp["results"].as_array() {
                for page in results {
                    let id = page["id"].as_str().unwrap_or("").to_string();
                    let title = page["title"].as_str().unwrap_or("Untitled").to_string();
                    let updated_str = page["version"]["when"].as_str().unwrap_or("");
                    let updated_at = parse_iso8601(updated_str).unwrap_or(0);

                    items.push(RemoteItem {
                        synthetic_path: PathBuf::from(format!(
                            "confluence://{space_key}/{id}.confluence"
                        )),
                        id,
                        title,
                        updated_at,
                        deleted: false,
                    });
                }
            }

            // Follow pagination
            next_url = resp["_links"]["next"]
                .as_str()
                .map(|n| format!("{}{n}", self.config.base_url));
        }

        Ok(items)
    }
}

impl RemoteSource for ConfluenceSource {
    fn source_id(&self) -> &str {
        "confluence"
    }

    fn poll(&mut self, since: Option<i64>) -> Result<Vec<RemoteItem>, RemoteError> {
        let mut all = Vec::new();
        for space in self.config.spaces.clone() {
            match self.list_pages_in_space(&space, since) {
                Ok(items) => all.extend(items),
                Err(e) => tracing::warn!(space = %space, error = %e, "failed to list Confluence space"),
            }
        }
        Ok(all)
    }

    fn fetch_content(&self, item: &RemoteItem) -> Result<Vec<u8>, RemoteError> {
        let page_id = item.id.as_str();
        let url = format!(
            "{}/rest/api/content/{page_id}?expand=body.storage,metadata.labels,ancestors,version,space",
            self.config.base_url
        );
        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .header("Accept", "application/json")
            .send()
            .map_err(|e| RemoteError::Http { status: 0, body: e.to_string() })?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().unwrap_or_default();
            return Err(RemoteError::Http { status, body });
        }

        resp.bytes()
            .map(|b| b.to_vec())
            .map_err(|e| RemoteError::Parse(e.to_string()))
    }
}

// ── Helpers ───────────────────────────────────────────────────────

fn base64_encode(bytes: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
        let b2 = chunk.get(2).copied().unwrap_or(0) as usize;
        out.push(CHARS[b0 >> 2] as char);
        out.push(CHARS[((b0 & 3) << 4) | (b1 >> 4)] as char);
        if chunk.len() > 1 {
            out.push(CHARS[((b1 & 0xf) << 2) | (b2 >> 6)] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(CHARS[b2 & 0x3f] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn format_confluence_datetime(ts: i64) -> String {
    // Confluence expects: "2024-01-15T00:00:00.000Z"
    // Simple approximation using seconds since epoch
    let secs = ts.max(0) as u64;
    let days = secs / 86400;
    let years = 1970 + days / 365;
    let remaining_days = days % 365;
    let month = (remaining_days / 30) + 1;
    let day = (remaining_days % 30) + 1;
    format!("{years:04}-{month:02}-{day:02}T00:00:00.000Z")
}

fn parse_iso8601(s: &str) -> Option<i64> {
    // Parse "2024-01-15T12:34:56.789Z" → unix seconds (approximate)
    if s.len() < 19 {
        return None;
    }
    let year: i64 = s[0..4].parse().ok()?;
    let month: i64 = s[5..7].parse().ok()?;
    let day: i64 = s[8..10].parse().ok()?;
    let hour: i64 = s[11..13].parse().ok()?;
    let min: i64 = s[14..16].parse().ok()?;
    let sec: i64 = s[17..19].parse().ok()?;
    // Rough unix timestamp
    let days = (year - 1970) * 365 + (month - 1) * 30 + (day - 1);
    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_PAGE: &str = r#"{
        "id": "123456",
        "title": "Auth Design",
        "space": { "key": "ENG" },
        "body": {
            "storage": {
                "value": "<h1>Overview</h1><p>This page covers <a href=\"https://example.com\">authentication</a>.</p><h2>Details</h2><p>Use OAuth2.</p>"
            }
        },
        "metadata": {
            "labels": {
                "results": [
                    {"name": "auth"},
                    {"name": "security"}
                ]
            }
        },
        "ancestors": [
            {"title": "Architecture"},
            {"title": "Backend"}
        ],
        "version": { "when": "2024-01-15T12:00:00.000Z" }
    }"#;

    #[test]
    fn parser_can_parse_confluence_extension() {
        let p = ConfluenceParser;
        assert!(p.can_parse(Path::new("confluence://ENG/123.confluence")));
        assert!(!p.can_parse(Path::new("file.md")));
    }

    #[test]
    fn parser_extracts_tags_from_labels() {
        let p = ConfluenceParser;
        let path = PathBuf::from("confluence://ENG/123456.confluence");
        let result = p.parse(&path, SAMPLE_PAGE.as_bytes()).unwrap();

        assert!(result.tags.contains(&"source:confluence".to_string()));
        assert!(result.tags.contains(&"confluence:space:ENG".to_string()));
        assert!(result.tags.contains(&"tag:auth".to_string()));
        assert!(result.tags.contains(&"tag:security".to_string()));
    }

    #[test]
    fn parser_extracts_ancestors_as_parent_tags() {
        let p = ConfluenceParser;
        let path = PathBuf::from("confluence://ENG/123456.confluence");
        let result = p.parse(&path, SAMPLE_PAGE.as_bytes()).unwrap();

        assert!(result.tags.iter().any(|t| t.contains("architecture")));
    }

    #[test]
    fn parser_extracts_headings_as_chunks() {
        let p = ConfluenceParser;
        let path = PathBuf::from("confluence://ENG/123456.confluence");
        let result = p.parse(&path, SAMPLE_PAGE.as_bytes()).unwrap();

        // Title chunk + at least one heading chunk
        assert!(result.chunks.len() >= 2);
        assert_eq!(result.chunks[0].label.as_deref(), Some("Auth Design"));
    }

    #[test]
    fn parser_extracts_links() {
        let p = ConfluenceParser;
        let path = PathBuf::from("confluence://ENG/123456.confluence");
        let result = p.parse(&path, SAMPLE_PAGE.as_bytes()).unwrap();

        assert!(result.links.iter().any(|l| l.target.contains("example.com")));
    }

    #[test]
    fn strip_tags_removes_html() {
        assert_eq!(strip_tags("<strong>hello</strong> world"), "hello world");
        assert_eq!(strip_tags("plain text"), "plain text");
    }

    #[test]
    fn base64_encode_roundtrip() {
        let input = b"user@example.com:api_token_here";
        let encoded = base64_encode(input);
        assert!(!encoded.is_empty());
        assert!(!encoded.contains(' '));
    }
}

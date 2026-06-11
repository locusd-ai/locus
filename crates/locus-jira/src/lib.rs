//! Jira Cloud source and parser for Locus.
//!
//! `JiraParser` ingests bytes produced by `JiraSource::fetch_content` (Jira REST API v3 JSON).
//! `JiraSource` polls the Jira REST API and feeds items to `RemoteIngestionLoop`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use locus_core::parser::{ParseError, Parser};
use locus_core::types::{
    Chunk, ChunkKind, ChunkMetadata, DocType, LinkRef, ParseResult, Timestamp,
};
use locus_watcher::remote::{RemoteError, RemoteItem, RemoteSource};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION};
use serde::Deserialize;
use tracing::debug;

// ── Config ────────────────────────────────────────────────────────────────────

pub struct JiraConfig {
    /// Base URL, e.g. `https://your-org.atlassian.net`
    pub base_url: String,
    /// Atlassian account email
    pub username: String,
    /// Atlassian API token
    pub api_token: String,
    /// Project keys to poll, e.g. `["PROJ", "INFRA"]`
    pub projects: Vec<String>,
    pub poll_interval_secs: u64,
}

// ── Parser ────────────────────────────────────────────────────────────────────

pub struct JiraParser;

impl Parser for JiraParser {
    fn can_parse(&self, path: &Path) -> bool {
        let name = path.to_string_lossy();
        name.ends_with(".jira") || name.starts_with("jira://")
    }

    fn parse(&self, path: &Path, content: &[u8]) -> Result<ParseResult, ParseError> {
        let issue: JiraIssueResponse = serde_json::from_slice(content)
            .map_err(|e| ParseError::BadFrontmatter(format!("jira JSON: {e}")))?;
        Ok(parse_issue(path, &issue))
    }
}

fn parse_issue(path: &Path, issue: &JiraIssueResponse) -> ParseResult {
    let fields = &issue.fields;

    let auto_type = Some(match fields.issuetype.name.to_lowercase().as_str() {
        "bug" => DocType::Custom("bug".into()),
        "epic" => DocType::Custom("epic".into()),
        "story" => DocType::Custom("story".into()),
        "task" | "sub-task" | "subtask" => DocType::Task,
        _ => DocType::Custom(fields.issuetype.name.to_lowercase()),
    });

    let mut tags = vec![
        "source:jira".to_string(),
        format!("jira:project:{}", fields.project.key),
        format!("jira:status:{}", slug(&fields.status.name)),
        format!("jira:type:{}", slug(&fields.issuetype.name)),
    ];

    if let Some(ref p) = fields.priority {
        tags.push(format!("jira:priority:{}", slug(&p.name)));
    }
    for label in &fields.labels {
        tags.push(format!("tag:{}", slug(label)));
    }
    for comp in &fields.components {
        tags.push(format!("jira:component:{}", slug(&comp.name)));
    }
    if let Some(ref assignee) = fields.assignee {
        tags.push(format!("jira:assignee:{}", slug(&assignee.display_name)));
    }

    // Build chunks from summary + description
    let mut chunks = Vec::new();
    let mut byte_offset = 0usize;

    if let Some(ref summary) = fields.summary {
        let end = byte_offset + summary.len();
        chunks.push(Chunk {
            byte_range: byte_offset..end,
            kind: ChunkKind::Section,
            label: Some(summary.clone()),
            depth: 1,
            metadata: ChunkMetadata::default(),
        });
        byte_offset = end + 1;
    }

    if let Some(ref desc) = fields.description {
        let paragraphs = extract_adf_text(desc);
        for para in &paragraphs {
            if para.trim().is_empty() {
                continue;
            }
            let end = byte_offset + para.len();
            chunks.push(Chunk {
                byte_range: byte_offset..end,
                kind: ChunkKind::Body,
                label: None,
                depth: 2,
                metadata: ChunkMetadata::default(),
            });
            byte_offset = end + 1;
        }
    }

    // Comments as body chunks
    if let Some(ref comment_container) = fields.comment {
        for c in &comment_container.comments {
            let body = extract_adf_text(&c.body).join(" ");
            if body.trim().is_empty() {
                continue;
            }
            let author = c
                .author
                .as_ref()
                .map(|a| a.display_name.as_str())
                .unwrap_or("unknown");
            let text = format!("[{author}] {body}");
            let end = byte_offset + text.len();
            chunks.push(Chunk {
                byte_range: byte_offset..end,
                kind: ChunkKind::Body,
                label: Some(format!("comment by {author}")),
                depth: 2,
                metadata: ChunkMetadata::default(),
            });
            byte_offset = end + 1;
        }
    }

    // Links: issue links as LinkRef targets
    let mut links = Vec::new();
    for il in &fields.issuelinks {
        if let Some(ref inward) = il.inward_issue {
            links.push(LinkRef {
                target: inward.key.clone(),
                display: None,
                byte_offset: 0,
            });
        }
        if let Some(ref outward) = il.outward_issue {
            links.push(LinkRef {
                target: outward.key.clone(),
                display: None,
                byte_offset: 0,
            });
        }
    }

    // Store key metadata in frontmatter for consumers
    let mut frontmatter = HashMap::new();
    frontmatter.insert("key".to_string(), serde_json::Value::String(issue.key.clone()));
    frontmatter.insert(
        "project".to_string(),
        serde_json::Value::String(fields.project.key.clone()),
    );
    if let Some(ref summary) = fields.summary {
        frontmatter.insert(
            "title".to_string(),
            serde_json::Value::String(summary.clone()),
        );
    }
    frontmatter.insert(
        "status".to_string(),
        serde_json::Value::String(fields.status.name.clone()),
    );

    let _ = path;
    ParseResult {
        chunks,
        tags,
        links,
        auto_type,
        frontmatter,
    }
}

// ── ADF text extraction ───────────────────────────────────────────────────────

fn extract_adf_text(node: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_adf_text(node, &mut out);
    out
}

fn collect_adf_text(node: &serde_json::Value, out: &mut Vec<String>) {
    match node {
        serde_json::Value::Object(map) => {
            let node_type = map.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match node_type {
                "text" => {
                    if let Some(text) = map.get("text").and_then(|v| v.as_str()) {
                        if let Some(last) = out.last_mut() {
                            last.push(' ');
                            last.push_str(text);
                        } else {
                            out.push(text.to_string());
                        }
                    }
                }
                "paragraph" | "heading" | "bulletList" | "orderedList" | "listItem"
                | "blockquote" | "codeBlock" | "panel" => {
                    out.push(String::new());
                    if let Some(content) = map.get("content") {
                        collect_adf_text(content, out);
                    }
                }
                _ => {
                    if let Some(content) = map.get("content") {
                        collect_adf_text(content, out);
                    }
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                collect_adf_text(item, out);
            }
        }
        _ => {}
    }
}

// ── Deserialization types ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct JiraIssueResponse {
    key: String,
    fields: JiraFields,
}

#[derive(Debug, Deserialize)]
struct JiraFields {
    summary: Option<String>,
    description: Option<serde_json::Value>,
    status: JiraStatus,
    issuetype: JiraIssueType,
    project: JiraProject,
    priority: Option<JiraPriority>,
    assignee: Option<JiraUser>,
    labels: Vec<String>,
    components: Vec<JiraComponent>,
    #[serde(default)]
    issuelinks: Vec<JiraIssueLink>,
    comment: Option<JiraCommentContainer>,
    updated: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JiraStatus {
    name: String,
}

#[derive(Debug, Deserialize)]
struct JiraIssueType {
    name: String,
}

#[derive(Debug, Deserialize)]
struct JiraProject {
    key: String,
}

#[derive(Debug, Deserialize)]
struct JiraPriority {
    name: String,
}

#[derive(Debug, Deserialize)]
struct JiraUser {
    #[serde(rename = "displayName")]
    display_name: String,
}

#[derive(Debug, Deserialize)]
struct JiraComponent {
    name: String,
}

#[derive(Debug, Deserialize)]
struct JiraIssueLink {
    #[serde(rename = "inwardIssue")]
    inward_issue: Option<JiraLinkedIssue>,
    #[serde(rename = "outwardIssue")]
    outward_issue: Option<JiraLinkedIssue>,
}

#[derive(Debug, Deserialize)]
struct JiraLinkedIssue {
    key: String,
}

#[derive(Debug, Deserialize)]
struct JiraCommentContainer {
    comments: Vec<JiraComment>,
}

#[derive(Debug, Deserialize)]
struct JiraComment {
    body: serde_json::Value,
    author: Option<JiraUser>,
}

#[derive(Debug, Deserialize)]
struct JiraSearchResponse {
    issues: Vec<JiraSearchIssue>,
    #[serde(rename = "startAt")]
    start_at: u64,
    #[serde(rename = "maxResults")]
    max_results: u64,
    total: u64,
}

#[derive(Debug, Deserialize)]
struct JiraSearchIssue {
    key: String,
    fields: JiraSearchFields,
}

#[derive(Debug, Deserialize)]
struct JiraSearchFields {
    project: JiraProject,
    updated: Option<String>,
}

// ── Source ────────────────────────────────────────────────────────────────────

pub struct JiraSource {
    config: JiraConfig,
    client: Client,
}

impl JiraSource {
    pub fn new(config: JiraConfig) -> Self {
        Self {
            config,
            client: Client::new(),
        }
    }

    fn auth_header(&self) -> String {
        let raw = format!("{}:{}", self.config.username, self.config.api_token);
        format!("Basic {}", base64_encode(raw.as_bytes()))
    }

    fn search_issues(&self, since: Option<Timestamp>) -> Result<Vec<JiraSearchIssue>, RemoteError> {
        let projects_clause = self
            .config
            .projects
            .iter()
            .map(|p| format!("\"{}\"", p))
            .collect::<Vec<_>>()
            .join(", ");

        let mut jql = format!("project in ({projects_clause})");
        if let Some(ts) = since {
            let dt = format_jira_datetime(ts);
            jql.push_str(&format!(" AND updated > \"{dt}\""));
        }
        jql.push_str(" ORDER BY updated ASC");

        let mut all = Vec::new();
        let mut start_at: u64 = 0;
        let max_results: u64 = 50;

        loop {
            let url = format!(
                "{}/rest/api/3/search?jql={}&startAt={}&maxResults={}&fields=key,project,updated",
                self.config.base_url,
                urlencoded(&jql),
                start_at,
                max_results
            );

            debug!(%url, "jira: search_issues");

            let resp = self
                .client
                .get(&url)
                .header(AUTHORIZATION, self.auth_header())
                .header(ACCEPT, "application/json")
                .send()
                .map_err(|e| RemoteError::Remote(e.to_string()))?;

            let status = resp.status().as_u16();
            if !resp.status().is_success() {
                let body = resp.text().unwrap_or_default();
                return Err(RemoteError::Http { status, body });
            }

            let page: JiraSearchResponse = resp
                .json()
                .map_err(|e| RemoteError::Parse(e.to_string()))?;

            let count = page.issues.len() as u64;
            all.extend(page.issues);

            if start_at + count >= page.total {
                break;
            }
            start_at += count;
        }

        Ok(all)
    }
}

impl RemoteSource for JiraSource {
    fn source_id(&self) -> &str {
        "jira"
    }

    fn poll(&mut self, since: Option<Timestamp>) -> Result<Vec<RemoteItem>, RemoteError> {
        let issues = self.search_issues(since)?;
        let items = issues
            .into_iter()
            .map(|i| {
                let project = i.fields.project.key.clone();
                let updated_at = i
                    .fields
                    .updated
                    .as_deref()
                    .and_then(parse_iso8601)
                    .unwrap_or_else(|| {
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_secs() as Timestamp)
                            .unwrap_or(0)
                    });
                RemoteItem {
                    id: i.key.clone(),
                    synthetic_path: PathBuf::from(format!("jira://{}/{}.jira", project, i.key)),
                    title: i.key.clone(),
                    updated_at,
                    deleted: false,
                }
            })
            .collect();
        Ok(items)
    }

    fn fetch_content(&self, item: &RemoteItem) -> Result<Vec<u8>, RemoteError> {
        let url = format!(
            "{}/rest/api/3/issue/{}?fields=summary,description,status,issuetype,project,priority,assignee,labels,components,issuelinks,comment,updated",
            self.config.base_url, item.id
        );

        debug!(issue = %item.id, %url, "jira: fetch_content");

        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header())
            .header(ACCEPT, "application/json")
            .send()
            .map_err(|e| RemoteError::Remote(e.to_string()))?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(RemoteError::Http { status, body });
        }

        resp.bytes()
            .map(|b| b.to_vec())
            .map_err(|e| RemoteError::Remote(e.to_string()))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn slug(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn urlencoded(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(c),
            ' ' => out.push('+'),
            c => {
                let mut buf = [0u8; 4];
                let bytes = c.encode_utf8(&mut buf);
                for b in bytes.bytes() {
                    out.push('%');
                    out.push(nibble_to_hex(b >> 4));
                    out.push(nibble_to_hex(b & 0xf));
                }
            }
        }
    }
    out
}

fn nibble_to_hex(n: u8) -> char {
    if n < 10 { (b'0' + n) as char } else { (b'A' + n - 10) as char }
}

fn format_jira_datetime(ts: Timestamp) -> String {
    // Jira JQL accepts ISO 8601 — derive year from seconds for a rough cutoff
    let secs = ts.max(0) as u64;
    let days = secs / 86400;
    let year = 1970u64 + days / 365;
    format!("{year}-01-01 00:00")
}

fn parse_iso8601(s: &str) -> Option<Timestamp> {
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    let year: i64 = s[0..4].parse().ok()?;
    let month: i64 = s[5..7].parse().ok()?;
    let day: i64 = s[8..10].parse().ok()?;
    let hour: i64 = s[11..13].parse().ok()?;
    let min: i64 = s[14..16].parse().ok()?;
    let sec: i64 = s[17..19].parse().ok()?;
    let days = days_since_epoch(year, month, day);
    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

fn days_since_epoch(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let m = if month <= 2 { month + 9 } else { month - 3 };
    let c = y / 100;
    let yy = y % 100;
    (146097 * c) / 4 + (1461 * yy) / 4 + (153 * m + 2) / 5 + day + 1721119 - 2440588
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i < input.len() {
        let b0 = input[i] as u32;
        let b1 = input.get(i + 1).copied().unwrap_or(0) as u32;
        let b2 = input.get(i + 2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        if i + 1 < input.len() {
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if i + 2 < input.len() {
            out.push(TABLE[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
        i += 3;
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const ISSUE_JSON: &str = r#"{
        "key": "PROJ-42",
        "fields": {
            "summary": "Fix login timeout",
            "description": {
                "type": "doc",
                "version": 1,
                "content": [
                    {
                        "type": "paragraph",
                        "content": [
                            {"type": "text", "text": "Users are getting logged out too quickly."}
                        ]
                    },
                    {
                        "type": "paragraph",
                        "content": [
                            {"type": "text", "text": "The session timeout is set to 15 minutes."}
                        ]
                    }
                ]
            },
            "status": {"name": "In Progress"},
            "issuetype": {"name": "Bug"},
            "project": {"key": "PROJ"},
            "priority": {"name": "High"},
            "assignee": {"displayName": "Alice Smith"},
            "labels": ["auth", "backend"],
            "components": [{"name": "Auth Service"}],
            "issuelinks": [
                {
                    "inwardIssue": {"key": "PROJ-10"},
                    "outwardIssue": null
                }
            ],
            "comment": {
                "comments": [
                    {
                        "body": {
                            "type": "doc",
                            "version": 1,
                            "content": [
                                {
                                    "type": "paragraph",
                                    "content": [{"type": "text", "text": "Looking into this now."}]
                                }
                            ]
                        },
                        "author": {"displayName": "Bob Jones"}
                    }
                ]
            },
            "updated": "2024-03-15T14:32:00.000+0000"
        }
    }"#;

    #[test]
    fn test_can_parse() {
        let parser = JiraParser;
        assert!(parser.can_parse(Path::new("jira://PROJ/PROJ-42.jira")));
        assert!(parser.can_parse(Path::new("PROJ-42.jira")));
        assert!(!parser.can_parse(Path::new("note.md")));
    }

    #[test]
    fn test_parse_issue() {
        let parser = JiraParser;
        let path = Path::new("jira://PROJ/PROJ-42.jira");
        let result = parser.parse(path, ISSUE_JSON.as_bytes()).unwrap();

        assert_eq!(
            result.frontmatter["title"].as_str(),
            Some("Fix login timeout")
        );
        assert!(matches!(result.auto_type, Some(DocType::Custom(ref s)) if s == "bug"));
    }

    #[test]
    fn test_parse_tags() {
        let parser = JiraParser;
        let path = Path::new("jira://PROJ/PROJ-42.jira");
        let result = parser.parse(path, ISSUE_JSON.as_bytes()).unwrap();

        assert!(result.tags.contains(&"source:jira".to_string()));
        assert!(result.tags.contains(&"jira:project:PROJ".to_string()));
        assert!(result.tags.contains(&"jira:status:in-progress".to_string()));
        assert!(result.tags.contains(&"jira:priority:high".to_string()));
        assert!(result.tags.contains(&"tag:auth".to_string()));
        assert!(result.tags.contains(&"tag:backend".to_string()));
        assert!(result.tags.contains(&"jira:component:auth-service".to_string()));
    }

    #[test]
    fn test_parse_chunks() {
        let parser = JiraParser;
        let path = Path::new("jira://PROJ/PROJ-42.jira");
        let result = parser.parse(path, ISSUE_JSON.as_bytes()).unwrap();

        assert!(!result.chunks.is_empty());
        // First chunk is the summary section
        assert_eq!(
            result.chunks[0].label.as_deref(),
            Some("Fix login timeout")
        );
        assert!(matches!(result.chunks[0].kind, ChunkKind::Section));
    }

    #[test]
    fn test_parse_links() {
        let parser = JiraParser;
        let path = Path::new("jira://PROJ/PROJ-42.jira");
        let result = parser.parse(path, ISSUE_JSON.as_bytes()).unwrap();

        assert!(result.links.iter().any(|l| l.target == "PROJ-10"));
    }

    #[test]
    fn test_adf_text_extraction() {
        let adf: serde_json::Value = serde_json::json!({
            "type": "doc",
            "content": [
                {
                    "type": "paragraph",
                    "content": [
                        {"type": "text", "text": "Hello"},
                        {"type": "text", "text": " world"}
                    ]
                },
                {
                    "type": "paragraph",
                    "content": [{"type": "text", "text": "Second paragraph"}]
                }
            ]
        });
        let texts = extract_adf_text(&adf);
        let joined = texts.join(" ");
        assert!(joined.contains("Hello"));
        assert!(joined.contains("world"));
        assert!(joined.contains("Second paragraph"));
    }

    #[test]
    fn test_parse_invalid_json() {
        let parser = JiraParser;
        let result = parser.parse(Path::new("jira://PROJ/PROJ-1.jira"), b"not json");
        assert!(matches!(result, Err(ParseError::BadFrontmatter(_))));
    }

    #[test]
    fn test_parse_iso8601() {
        let ts = parse_iso8601("2024-03-15T14:32:00.000+0000").unwrap();
        // 2024-03-15 = 54 years * ~365 days — sanity check it's a reasonable epoch ts
        assert!(ts > 1_700_000_000);
        assert!(ts < 1_800_000_000);
    }
}

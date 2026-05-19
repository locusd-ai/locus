//! Slack source and parser for Locus.
//!
//! `SlackParser` ingests bytes produced by `SlackSource::fetch_content` (bundled JSON
//! containing channel metadata + messages for a given day).
//! `SlackSource` polls Slack's `conversations.history` API per configured channel.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use locus_core::parser::{ParseError, Parser};
use locus_core::types::{Chunk, ChunkKind, ChunkMetadata, DocType, ParseResult, Timestamp};
use locus_watcher::remote::{RemoteError, RemoteItem, RemoteSource};
use reqwest::blocking::Client;
use reqwest::header::AUTHORIZATION;
use serde::{Deserialize, Serialize};
use tracing::debug;

// ── Config ────────────────────────────────────────────────────────────────────

pub struct SlackConfig {
    /// Slack Bot User OAuth Token (starts with `xoxb-`)
    pub bot_token: String,
    /// Channel IDs to poll, e.g. `["C01234567", "C09876543"]`
    pub channel_ids: Vec<String>,
    /// Human-readable channel names (parallel to channel_ids), used for tagging
    pub channel_names: Vec<String>,
    pub poll_interval_secs: u64,
}

// ── Wire format ───────────────────────────────────────────────────────────────
// SlackSource packs one day of messages per channel into a JSON envelope.

#[derive(Debug, Deserialize, Serialize)]
pub struct SlackBundle {
    pub channel_id: String,
    pub channel_name: String,
    pub date: String,
    pub messages: Vec<SlackMessage>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SlackMessage {
    pub ts: String,
    pub user: Option<String>,
    pub username: Option<String>,
    pub text: String,
    #[serde(rename = "type")]
    pub msg_type: Option<String>,
    pub subtype: Option<String>,
    pub thread_ts: Option<String>,
    pub reply_count: Option<u32>,
    pub reactions: Option<Vec<SlackReaction>>,
    pub files: Option<Vec<SlackFile>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SlackReaction {
    pub name: String,
    pub count: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SlackFile {
    pub name: Option<String>,
    pub title: Option<String>,
}

// ── Parser ────────────────────────────────────────────────────────────────────

pub struct SlackParser;

impl Parser for SlackParser {
    fn can_parse(&self, path: &Path) -> bool {
        let name = path.to_string_lossy();
        name.ends_with(".slack") || name.starts_with("slack://")
    }

    fn parse(&self, path: &Path, content: &[u8]) -> Result<ParseResult, ParseError> {
        let bundle: SlackBundle = serde_json::from_slice(content)
            .map_err(|e| ParseError::BadFrontmatter(format!("slack JSON: {e}")))?;
        let _ = path;
        Ok(parse_bundle(&bundle))
    }
}

fn parse_bundle(bundle: &SlackBundle) -> ParseResult {
    let channel_slug = slug(&bundle.channel_name);

    let mut tags = vec![
        "source:slack".to_string(),
        format!("slack:channel:{channel_slug}"),
        format!("slack:date:{}", bundle.date),
    ];

    // Collect unique users for tags
    let users: HashSet<String> = bundle
        .messages
        .iter()
        .filter_map(|m| m.username.clone().or_else(|| m.user.clone()))
        .collect();
    let mut sorted_users: Vec<String> = users.into_iter().collect();
    sorted_users.sort();
    for user in &sorted_users {
        tags.push(format!("slack:user:{}", slug(user)));
    }

    let has_threads = bundle
        .messages
        .iter()
        .any(|m| m.reply_count.unwrap_or(0) > 0);
    if has_threads {
        tags.push("slack:has-threads".to_string());
    }

    // Group replies by thread_ts
    let mut thread_replies: HashMap<String, Vec<&SlackMessage>> = HashMap::new();
    for msg in &bundle.messages {
        if let Some(ref tt) = msg.thread_ts {
            if tt != &msg.ts {
                thread_replies.entry(tt.clone()).or_default().push(msg);
            }
        }
    }

    let mut chunks = Vec::new();
    let mut byte_offset = 0usize;

    for msg in &bundle.messages {
        // Skip non-root thread messages — they're inlined below
        if let Some(ref tt) = msg.thread_ts {
            if tt != &msg.ts {
                continue;
            }
        }
        // Skip system subtypes
        if let Some(ref sub) = msg.subtype {
            if sub != "bot_message" && sub != "file_share" {
                continue;
            }
        }

        let author = msg
            .username
            .as_deref()
            .or(msg.user.as_deref())
            .unwrap_or("unknown");

        let mut text = format!("[{}] {}", author, expand_slack_text(&msg.text));

        if let Some(ref files) = msg.files {
            for f in files {
                let name = f.title.as_deref().or(f.name.as_deref()).unwrap_or("file");
                text.push_str(&format!(" [file: {name}]"));
            }
        }

        if let Some(ref reactions) = msg.reactions {
            let rxn: Vec<String> = reactions
                .iter()
                .map(|r| format!(":{}:×{}", r.name, r.count))
                .collect();
            if !rxn.is_empty() {
                text.push_str(&format!(" ({})", rxn.join(" ")));
            }
        }

        // Inline thread replies
        if let Some(replies) = thread_replies.get(&msg.ts) {
            for reply in replies {
                let ra = reply
                    .username
                    .as_deref()
                    .or(reply.user.as_deref())
                    .unwrap_or("unknown");
                text.push_str(&format!(
                    "\n  ↳ [{}] {}",
                    ra,
                    expand_slack_text(&reply.text)
                ));
            }
        }

        let end = byte_offset + text.len();
        chunks.push(Chunk {
            byte_range: byte_offset..end,
            kind: ChunkKind::Body,
            label: Some(format!("[{author}]")),
            depth: 1,
            metadata: ChunkMetadata::default(),
        });
        byte_offset = end + 1;
    }

    let title = format!("#{} — {}", bundle.channel_name, bundle.date);
    let mut frontmatter = HashMap::new();
    frontmatter.insert(
        "title".to_string(),
        serde_json::Value::String(title.clone()),
    );
    frontmatter.insert(
        "channel".to_string(),
        serde_json::Value::String(bundle.channel_name.clone()),
    );
    frontmatter.insert(
        "date".to_string(),
        serde_json::Value::String(bundle.date.clone()),
    );

    ParseResult {
        chunks,
        tags,
        links: vec![],
        auto_type: Some(DocType::Custom("conversation".into())),
        frontmatter,
    }
}

/// Expand Slack mrkdwn markup to plain text.
fn expand_slack_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            let mut inner = String::new();
            for ic in chars.by_ref() {
                if ic == '>' {
                    break;
                }
                inner.push(ic);
            }
            if inner.starts_with('@') {
                out.push('@');
                if let Some((_, rest)) = inner.split_once('|') {
                    out.push_str(rest);
                } else {
                    out.push_str(&inner[1..]);
                }
            } else if inner.starts_with('#') {
                out.push('#');
                if let Some((_, name)) = inner.split_once('|') {
                    out.push_str(name);
                } else {
                    out.push_str(&inner[1..]);
                }
            } else if let Some((_, label)) = inner.split_once('|') {
                out.push_str(label);
            } else {
                out.push_str(&inner);
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ── Slack API responses ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ConversationsHistoryResponse {
    ok: bool,
    messages: Option<Vec<SlackMessage>>,
    has_more: Option<bool>,
    #[serde(rename = "response_metadata")]
    metadata: Option<ResponseMetadata>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponseMetadata {
    next_cursor: Option<String>,
}

// ── Source ────────────────────────────────────────────────────────────────────

pub struct SlackSource {
    config: SlackConfig,
    client: Client,
}

impl SlackSource {
    pub fn new(config: SlackConfig) -> Self {
        Self {
            config,
            client: Client::new(),
        }
    }

    fn channel_name(&self, channel_id: &str) -> String {
        self.config
            .channel_ids
            .iter()
            .position(|id| id == channel_id)
            .and_then(|i| self.config.channel_names.get(i))
            .cloned()
            .unwrap_or_else(|| channel_id.to_string())
    }

    fn fetch_history(
        &self,
        channel_id: &str,
        oldest: Option<&str>,
    ) -> Result<Vec<SlackMessage>, RemoteError> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let mut url = format!(
                "https://slack.com/api/conversations.history?channel={}&limit=200",
                channel_id
            );
            if let Some(ts) = oldest {
                url.push_str(&format!("&oldest={ts}"));
            }
            if let Some(ref c) = cursor {
                url.push_str(&format!("&cursor={c}"));
            }

            debug!(channel = %channel_id, %url, "slack: fetch_history");

            let resp = self
                .client
                .get(&url)
                .header(AUTHORIZATION, format!("Bearer {}", self.config.bot_token))
                .send()
                .map_err(|e| RemoteError::Remote(e.to_string()))?;

            let status = resp.status().as_u16();
            if !resp.status().is_success() {
                let body = resp.text().unwrap_or_default();
                return Err(RemoteError::Http { status, body });
            }

            let page: ConversationsHistoryResponse = resp
                .json()
                .map_err(|e| RemoteError::Parse(e.to_string()))?;

            if !page.ok {
                return Err(RemoteError::Remote(
                    page.error.unwrap_or_else(|| "slack api error".into()),
                ));
            }

            if let Some(msgs) = page.messages {
                all.extend(msgs);
            }

            cursor = page
                .metadata
                .and_then(|m| m.next_cursor)
                .filter(|c| !c.is_empty());

            if cursor.is_none() || page.has_more != Some(true) {
                break;
            }
        }

        Ok(all)
    }
}

impl RemoteSource for SlackSource {
    fn source_id(&self) -> &str {
        "slack"
    }

    fn poll(&mut self, since: Option<Timestamp>) -> Result<Vec<RemoteItem>, RemoteError> {
        let oldest_ts = since.map(|ts| ts.to_string());
        let mut items = Vec::new();

        for channel_id in &self.config.channel_ids {
            let messages = self.fetch_history(channel_id, oldest_ts.as_deref())?;

            let mut days: HashMap<String, ()> = HashMap::new();
            for msg in &messages {
                let ts_secs: i64 = msg
                    .ts
                    .split('.')
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let date = unix_to_date(ts_secs);
                days.insert(date, ());
            }

            let channel_name = self.channel_name(channel_id);
            for date in days.keys() {
                let item_id = format!("{}-{}", channel_id, date);
                let updated_at = date_to_unix(date).unwrap_or(0) + 43200;
                items.push(RemoteItem {
                    id: item_id,
                    synthetic_path: PathBuf::from(format!(
                        "slack://{}/{}.slack",
                        slug(&channel_name),
                        date
                    )),
                    title: format!("#{channel_name} — {date}"),
                    updated_at,
                    deleted: false,
                });
            }
        }

        Ok(items)
    }

    fn fetch_content(&self, item: &RemoteItem) -> Result<Vec<u8>, RemoteError> {
        // item.id format: "CXXXXXXXXX-2024-03-15"
        let dash = item.id.find('-').ok_or_else(|| {
            RemoteError::Parse(format!("invalid slack item id: {}", item.id))
        })?;
        let channel_id = &item.id[..dash];
        let date = &item.id[dash + 1..];

        let oldest = date_to_unix(date);
        let latest = oldest.map(|ts| ts + 86400);

        let mut url = format!(
            "https://slack.com/api/conversations.history?channel={}&limit=200",
            channel_id
        );
        if let Some(ts) = oldest {
            url.push_str(&format!("&oldest={ts}"));
        }
        if let Some(ts) = latest {
            url.push_str(&format!("&latest={ts}"));
        }

        debug!(item = %item.id, %url, "slack: fetch_content");

        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.config.bot_token))
            .send()
            .map_err(|e| RemoteError::Remote(e.to_string()))?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(RemoteError::Http { status, body });
        }

        let page: ConversationsHistoryResponse = resp
            .json()
            .map_err(|e| RemoteError::Parse(e.to_string()))?;

        if !page.ok {
            return Err(RemoteError::Remote(
                page.error.unwrap_or_else(|| "slack api error".into()),
            ));
        }

        let channel_name = self.channel_name(channel_id);
        let bundle = serde_json::json!({
            "channel_id": channel_id,
            "channel_name": channel_name,
            "date": date,
            "messages": page.messages.unwrap_or_default(),
        });

        serde_json::to_vec(&bundle).map_err(|e| RemoteError::Parse(e.to_string()))
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

fn unix_to_date(ts: i64) -> String {
    let secs = ts.max(0) as u64;
    let days = secs / 86400;
    let (y, m, d) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn date_to_unix(date: &str) -> Option<Timestamp> {
    if date.len() != 10 {
        return None;
    }
    let year: i64 = date[0..4].parse().ok()?;
    let month: i64 = date[5..7].parse().ok()?;
    let day: i64 = date[8..10].parse().ok()?;
    let y = if month <= 2 { year - 1 } else { year };
    let m = if month <= 2 { month + 9 } else { month - 3 };
    let c = y / 100;
    let yy = y % 100;
    let days =
        (146097 * c) / 4 + (1461 * yy) / 4 + (153 * m + 2) / 5 + day + 1721119 - 2440588;
    Some(days * 86400)
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bundle_json() -> &'static str {
        r#"{
            "channel_id": "C01234567",
            "channel_name": "engineering",
            "date": "2024-03-15",
            "messages": [
                {
                    "ts": "1710504000.000001",
                    "user": "U111",
                    "username": "alice",
                    "text": "Hey team, the build is broken.",
                    "type": "message"
                },
                {
                    "ts": "1710504060.000002",
                    "user": "U222",
                    "username": "bob",
                    "text": "Looking into it now.",
                    "type": "message",
                    "thread_ts": "1710504000.000001"
                },
                {
                    "ts": "1710504120.000003",
                    "user": "U333",
                    "username": "charlie",
                    "text": "Found the issue: <@U111|alice> pushed a bad config.",
                    "type": "message",
                    "reactions": [{"name": "thumbsup", "count": 3}]
                }
            ]
        }"#
    }

    #[test]
    fn test_can_parse() {
        let parser = SlackParser;
        assert!(parser.can_parse(Path::new("slack://engineering/2024-03-15.slack")));
        assert!(parser.can_parse(Path::new("2024-03-15.slack")));
        assert!(!parser.can_parse(Path::new("note.md")));
    }

    #[test]
    fn test_parse_bundle() {
        let parser = SlackParser;
        let path = Path::new("slack://engineering/2024-03-15.slack");
        let result = parser.parse(path, make_bundle_json().as_bytes()).unwrap();

        assert_eq!(
            result.frontmatter["title"].as_str(),
            Some("#engineering — 2024-03-15")
        );
        assert!(matches!(
            result.auto_type,
            Some(DocType::Custom(ref s)) if s == "conversation"
        ));
    }

    #[test]
    fn test_parse_tags() {
        let parser = SlackParser;
        let path = Path::new("slack://engineering/2024-03-15.slack");
        let result = parser.parse(path, make_bundle_json().as_bytes()).unwrap();

        assert!(result.tags.contains(&"source:slack".to_string()));
        assert!(result.tags.contains(&"slack:channel:engineering".to_string()));
        assert!(result.tags.contains(&"slack:date:2024-03-15".to_string()));
    }

    #[test]
    fn test_parse_threads_inlined() {
        let parser = SlackParser;
        let path = Path::new("slack://engineering/2024-03-15.slack");
        let result = parser.parse(path, make_bundle_json().as_bytes()).unwrap();

        // alice + charlie = 2 top-level chunks (bob's reply inlined under alice)
        assert_eq!(result.chunks.len(), 2, "expected 2 top-level chunks");

        // alice's chunk label should identify her
        let alice_chunk = &result.chunks[0];
        assert_eq!(alice_chunk.label.as_deref(), Some("[alice]"));
        assert!(matches!(alice_chunk.kind, ChunkKind::Body));
    }

    #[test]
    fn test_expand_slack_text() {
        assert_eq!(
            expand_slack_text("<@U111|alice> pushed changes to <#C01234567|engineering>"),
            "@alice pushed changes to #engineering"
        );
        assert_eq!(
            expand_slack_text("See <https://example.com|this link>"),
            "See this link"
        );
        assert_eq!(
            expand_slack_text("Visit <https://example.com>"),
            "Visit https://example.com"
        );
    }

    #[test]
    fn test_unix_to_date() {
        // 2024-03-15 00:00:00 UTC = 1710460800
        assert_eq!(unix_to_date(1710460800), "2024-03-15");
    }

    #[test]
    fn test_parse_invalid_json() {
        let parser = SlackParser;
        let result = parser.parse(Path::new("slack://x/y.slack"), b"not json");
        assert!(matches!(result, Err(ParseError::BadFrontmatter(_))));
    }
}

//! Markdown parser for Obsidian-flavour files.
//!
//! Pure function — no I/O, no state. Content provided as `&[u8]`.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;

use biem_core::{
    Chunk, ChunkKind, ChunkMetadata, LinkRef, DocType, ParseError, ParseResult, Parser,
};

/// Obsidian-flavour Markdown parser.
pub struct MarkdownParser;

impl Parser for MarkdownParser {
    fn can_parse(&self, path: &Path) -> bool {
        path.extension()
            .map(|ext| ext.eq_ignore_ascii_case("md"))
            .unwrap_or(false)
    }

    fn parse(&self, _path: &Path, content: &[u8]) -> Result<ParseResult, ParseError> {
        let text = std::str::from_utf8(content).map_err(|_| ParseError::InvalidUtf8)?;

        let (frontmatter, fm_tags, fm_range) = extract_frontmatter(text)?;
        let chunks = extract_chunks(text, fm_range.as_ref());
        let links = extract_links(text);
        let inline_tags = extract_inline_tags(text);

        // Merge and deduplicate tags
        let mut tags = fm_tags;
        for t in inline_tags {
            if !tags.contains(&t) {
                tags.push(t);
            }
        }

        // Flatten hierarchical tags: "a/b/c" → ["a", "a/b", "a/b/c"]
        let tags = flatten_hierarchical_tags(tags);

        let auto_type = detect_auto_type(text, &links, &frontmatter);

        Ok(ParseResult {
            chunks,
            tags,
            links,
            auto_type,
            frontmatter,
        })
    }
}

// ── Frontmatter ──────────────────────────────────────────────────

/// Extract YAML frontmatter from the beginning of a markdown file.
/// Returns (parsed map, tags from frontmatter, byte range of frontmatter block).
fn extract_frontmatter(
    content: &str,
) -> Result<
    (
        HashMap<String, serde_json::Value>,
        Vec<String>,
        Option<Range<usize>>,
    ),
    ParseError,
> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return Ok((HashMap::new(), vec![], None));
    }

    // Find the opening ---
    let start_offset = content.len() - trimmed.len();
    let after_opening = start_offset + 3;

    // Find closing ---
    let rest = &content[after_opening..];
    let close_pos = rest
        .find("\n---")
        .map(|p| after_opening + p + 1) // +1 for the \n
        .or_else(|| {
            // Handle \r\n
            rest.find("\r\n---").map(|p| after_opening + p + 2)
        });

    let Some(close_start) = close_pos else {
        return Ok((HashMap::new(), vec![], None));
    };

    let yaml_content = &content[after_opening..close_start];
    let fm_end = close_start + 3; // past the closing ---

    // Skip past any trailing newline after ---
    let fm_range_end = if content[fm_end..].starts_with('\n') {
        fm_end + 1
    } else if content[fm_end..].starts_with("\r\n") {
        fm_end + 2
    } else {
        fm_end
    };

    let yaml_value: serde_yaml::Value = serde_yaml::from_str(yaml_content)
        .map_err(|e| ParseError::BadFrontmatter(e.to_string()))?;

    let mut map = HashMap::new();
    let mut tags = Vec::new();

    if let serde_yaml::Value::Mapping(mapping) = yaml_value {
        for (k, v) in mapping {
            let key = match &k {
                serde_yaml::Value::String(s) => s.clone(),
                _ => continue,
            };

            // Extract tags
            if key == "tags" {
                match &v {
                    serde_yaml::Value::Sequence(seq) => {
                        for item in seq {
                            if let serde_yaml::Value::String(s) = item {
                                tags.push(s.clone());
                            }
                        }
                    }
                    serde_yaml::Value::String(s) => {
                        // Single tag as string, or comma-separated
                        for t in s.split(',') {
                            let t = t.trim();
                            if !t.is_empty() {
                                tags.push(t.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }

            let json_value = yaml_to_json(&v);
            map.insert(key, json_value);
        }
    }

    Ok((map, tags, Some(start_offset..fm_range_end)))
}

/// Convert a serde_yaml::Value to serde_json::Value.
fn yaml_to_json(v: &serde_yaml::Value) -> serde_json::Value {
    match v {
        serde_yaml::Value::Null => serde_json::Value::Null,
        serde_yaml::Value::Bool(b) => serde_json::Value::Bool(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_json::Value::Number(i.into())
            } else if let Some(f) = n.as_f64() {
                serde_json::json!(f)
            } else {
                serde_json::Value::Null
            }
        }
        serde_yaml::Value::String(s) => serde_json::Value::String(s.clone()),
        serde_yaml::Value::Sequence(seq) => {
            serde_json::Value::Array(seq.iter().map(yaml_to_json).collect())
        }
        serde_yaml::Value::Mapping(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .filter_map(|(k, v)| {
                    if let serde_yaml::Value::String(key) = k {
                        Some((key.clone(), yaml_to_json(v)))
                    } else {
                        None
                    }
                })
                .collect();
            serde_json::Value::Object(obj)
        }
        serde_yaml::Value::Tagged(tagged) => yaml_to_json(&tagged.value),
    }
}

// ── Chunks ───────────────────────────────────────────────────────

/// Extract chunks from markdown content based on ATX headings.
fn extract_chunks(content: &str, fm_range: Option<&Range<usize>>) -> Vec<Chunk> {
    let mut chunks = Vec::new();

    // Add frontmatter chunk if present
    let body_start = if let Some(range) = fm_range {
        chunks.push(Chunk {
            byte_range: range.clone(),
            kind: ChunkKind::Frontmatter,
            label: None,
            depth: 0,
            metadata: ChunkMetadata::default(),
        });
        range.end
    } else {
        0
    };

    let body = &content[body_start..];

    // Find all ATX headings with their positions
    let mut headings: Vec<(usize, u8, String)> = Vec::new(); // (byte_offset_in_body, depth, text)

    for (line_start, line) in line_positions(body) {
        if let Some((depth, text)) = parse_atx_heading(line) {
            headings.push((line_start, depth, text));
        }
    }

    if headings.is_empty() {
        // No headings → single Body chunk
        if body_start < content.len() {
            chunks.push(Chunk {
                byte_range: body_start..content.len(),
                kind: ChunkKind::Body,
                label: None,
                depth: 0,
                metadata: ChunkMetadata::default(),
            });
        }
    } else {
        // Content before first heading (if any)
        let first_heading_offset = headings[0].0;
        if first_heading_offset > 0 {
            let pre_start = body_start;
            let pre_end = body_start + first_heading_offset;
            // Only add if there's non-whitespace content
            if content[pre_start..pre_end].trim().len() > 0 {
                chunks.push(Chunk {
                    byte_range: pre_start..pre_end,
                    kind: ChunkKind::Body,
                    label: None,
                    depth: 0,
                    metadata: ChunkMetadata::default(),
                });
            }
        }

        // Each heading starts a Section chunk
        for (i, (offset, depth, text)) in headings.iter().enumerate() {
            let chunk_start = body_start + offset;
            let chunk_end = if i + 1 < headings.len() {
                body_start + headings[i + 1].0
            } else {
                content.len()
            };

            chunks.push(Chunk {
                byte_range: chunk_start..chunk_end,
                kind: ChunkKind::Section,
                label: Some(text.clone()),
                depth: *depth,
                metadata: ChunkMetadata::default(),
            });
        }
    }

    chunks
}

/// Iterate over lines in a string, yielding (byte_offset, line_str).
fn line_positions(s: &str) -> Vec<(usize, &str)> {
    let mut result = Vec::new();
    let mut pos = 0;
    for line in s.split('\n') {
        result.push((pos, line));
        pos += line.len() + 1; // +1 for \n
    }
    result
}

/// Parse an ATX heading line. Returns (depth, heading text) if it's a heading.
fn parse_atx_heading(line: &str) -> Option<(u8, String)> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }

    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }

    let rest = &trimmed[hashes..];
    // Must be followed by a space or be empty (ATX spec)
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }

    let text = rest.trim().trim_end_matches('#').trim().to_string();
    Some((hashes as u8, text))
}

// ── Links ────────────────────────────────────────────────────────

/// Extract `[[wikilinks]]` from markdown content, skipping code blocks.
fn extract_links(content: &str) -> Vec<LinkRef> {
    let mut links = Vec::new();
    let code_ranges = find_code_block_ranges(content);

    let bytes = content.as_bytes();
    let mut i = 0;

    while i < bytes.len().saturating_sub(3) {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            // Check if inside a code block
            if is_in_code_block(i, &code_ranges) {
                i += 2;
                continue;
            }

            let link_start = i;
            let inner_start = i + 2;

            // Find closing ]]
            if let Some(close) = content[inner_start..].find("]]") {
                let inner = &content[inner_start..inner_start + close];

                // Parse target|display
                let (target, display) = if let Some(pipe) = inner.find('|') {
                    (
                        inner[..pipe].to_string(),
                        Some(inner[pipe + 1..].to_string()),
                    )
                } else {
                    (inner.to_string(), None)
                };

                if !target.is_empty() {
                    links.push(LinkRef {
                        target,
                        display,
                        byte_offset: link_start,
                    });
                }

                i = inner_start + close + 2;
            } else {
                i += 2;
            }
        } else {
            i += 1;
        }
    }

    links
}

// ── Tags ─────────────────────────────────────────────────────────

/// Extract inline `#tag` patterns from markdown content, skipping code blocks and headings.
fn extract_inline_tags(content: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let code_ranges = find_code_block_ranges(content);

    for (line_start, line) in line_positions(content) {
        // Skip heading lines (# Heading is not a tag)
        if parse_atx_heading(line).is_some() {
            continue;
        }

        let bytes = line.as_bytes();
        let mut i = 0;

        while i < bytes.len() {
            if bytes[i] == b'#' {
                let abs_pos = line_start + i;

                // Check if inside code block
                if is_in_code_block(abs_pos, &code_ranges) {
                    i += 1;
                    continue;
                }

                // Must be preceded by whitespace or start of line
                if i > 0 && !bytes[i - 1].is_ascii_whitespace() {
                    i += 1;
                    continue;
                }

                // Collect tag characters: alphanumeric, -, _, /
                let tag_start = i + 1;
                let mut tag_end = tag_start;
                while tag_end < bytes.len()
                    && (bytes[tag_end].is_ascii_alphanumeric()
                        || bytes[tag_end] == b'-'
                        || bytes[tag_end] == b'_'
                        || bytes[tag_end] == b'/')
                {
                    tag_end += 1;
                }

                if tag_end > tag_start {
                    let tag = &line[tag_start..tag_end];
                    // Must start with a letter (not a number)
                    if tag.starts_with(|c: char| c.is_ascii_alphabetic()) {
                        let tag_str = tag.to_string();
                        if !tags.contains(&tag_str) {
                            tags.push(tag_str);
                        }
                    }
                }

                i = tag_end;
            } else {
                i += 1;
            }
        }
    }

    tags
}

/// Flatten hierarchical tags into all prefix segments.
/// e.g. `["work/project/alpha", "personal"]` → `["work", "work/project", "work/project/alpha", "personal"]`
fn flatten_hierarchical_tags(tags: Vec<String>) -> Vec<String> {
    let mut result = Vec::new();
    for tag in tags {
        if tag.contains('/') {
            let parts: Vec<&str> = tag.split('/').collect();
            let mut prefix = String::new();
            for (i, part) in parts.iter().enumerate() {
                if i > 0 {
                    prefix.push('/');
                }
                prefix.push_str(part);
                if !result.contains(&prefix) {
                    result.push(prefix.clone());
                }
            }
        } else if !result.contains(&tag) {
            result.push(tag);
        }
    }
    result
}

// ── Auto-type detection ──────────────────────────────────────────

/// Detect the note type based on content heuristics.
fn detect_auto_type(
    content: &str,
    links: &[LinkRef],
    frontmatter: &HashMap<String, serde_json::Value>,
) -> Option<DocType> {
    // Reference: has url, isbn, or source in frontmatter
    for key in ["url", "isbn", "source"] {
        if frontmatter.contains_key(key) {
            return Some(DocType::Reference);
        }
    }

    let line_count = content.lines().count().max(1);

    // Task: high density of checkboxes
    let checkbox_count = content.matches("- [ ]").count() + content.matches("- [x]").count()
        + content.matches("- [X]").count();
    if checkbox_count >= 3 && (checkbox_count as f64 / line_count as f64) > 0.2 {
        return Some(DocType::Task);
    }

    // Moc: high density of wikilinks
    if links.len() >= 5 && (links.len() as f64 / line_count as f64) > 0.3 {
        return Some(DocType::Moc);
    }

    None
}

// ── Code block detection ─────────────────────────────────────────

/// Find byte ranges of fenced code blocks (``` delimited).
fn find_code_block_ranges(content: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut in_fence = false;
    let mut fence_start = 0;

    for (pos, line) in line_positions(content) {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            if in_fence {
                // Closing fence
                ranges.push(fence_start..pos + line.len());
                in_fence = false;
            } else {
                // Opening fence
                fence_start = pos;
                in_fence = true;
            }
        }
    }

    // If we ended inside a fence, treat rest of file as code
    if in_fence {
        ranges.push(fence_start..content.len());
    }

    ranges
}

/// Check if a byte position falls inside any code block range.
fn is_in_code_block(pos: usize, ranges: &[Range<usize>]) -> bool {
    ranges.iter().any(|r| r.contains(&pos))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Frontmatter tests ────────────────────────────────────────

    #[test]
    fn frontmatter_with_tags() {
        let content = "---\ntitle: My Note\ntags:\n  - work\n  - rust\n---\n# Hello\n";
        let (fm, tags, range) = extract_frontmatter(content).unwrap();
        assert_eq!(fm.get("title").unwrap(), "My Note");
        assert_eq!(tags, vec!["work", "rust"]);
        assert!(range.is_some());
    }

    #[test]
    fn frontmatter_missing() {
        let content = "# Just a heading\nSome text\n";
        let (fm, tags, range) = extract_frontmatter(content).unwrap();
        assert!(fm.is_empty());
        assert!(tags.is_empty());
        assert!(range.is_none());
    }

    #[test]
    fn frontmatter_malformed_yaml() {
        let content = "---\n: invalid: yaml: [[\n---\n";
        let result = extract_frontmatter(content);
        assert!(result.is_err() || result.unwrap().0.is_empty());
    }

    #[test]
    fn frontmatter_byte_range() {
        let content = "---\ntitle: Test\n---\n# Hello\n";
        let (_, _, range) = extract_frontmatter(content).unwrap();
        let range = range.unwrap();
        assert_eq!(range.start, 0);
        assert_eq!(&content[range.clone()], "---\ntitle: Test\n---\n");
    }

    #[test]
    fn frontmatter_comma_separated_tags() {
        let content = "---\ntags: work, rust, biem\n---\n";
        let (_, tags, _) = extract_frontmatter(content).unwrap();
        assert_eq!(tags, vec!["work", "rust", "biem"]);
    }

    // ── Chunk tests ──────────────────────────────────────────────

    #[test]
    fn chunks_multiple_headings() {
        let content = "# Intro\nSome text\n## Details\nMore text\n### Deep\nDeep text\n";
        let chunks = extract_chunks(content, None);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].kind, ChunkKind::Section);
        assert_eq!(chunks[0].label.as_deref(), Some("Intro"));
        assert_eq!(chunks[0].depth, 1);
        assert_eq!(chunks[1].depth, 2);
        assert_eq!(chunks[2].depth, 3);
    }

    #[test]
    fn chunks_no_headings() {
        let content = "Just some text\nwith no headings\n";
        let chunks = extract_chunks(content, None);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].kind, ChunkKind::Body);
        assert_eq!(chunks[0].byte_range, 0..content.len());
    }

    #[test]
    fn chunks_with_frontmatter() {
        let fm_range = 0..20usize;
        let content = "---\ntitle: Test\n---\n# Hello\nText\n";
        let chunks = extract_chunks(content, Some(&fm_range));
        assert_eq!(chunks[0].kind, ChunkKind::Frontmatter);
        assert_eq!(chunks[1].kind, ChunkKind::Section);
        assert_eq!(chunks[1].label.as_deref(), Some("Hello"));
    }

    #[test]
    fn chunks_byte_ranges_contiguous() {
        let content = "# A\ntext\n## B\nmore\n## C\nend\n";
        let chunks = extract_chunks(content, None);
        // Verify contiguous coverage
        let mut expected_start = 0;
        for chunk in &chunks {
            assert_eq!(chunk.byte_range.start, expected_start);
            expected_start = chunk.byte_range.end;
        }
        assert_eq!(expected_start, content.len());
    }

    // ── Link tests ───────────────────────────────────────────────

    #[test]
    fn link_simple() {
        let content = "See [[ProjectAlpha]] for details.";
        let links = extract_links(content);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "ProjectAlpha");
        assert_eq!(links[0].display, None);
        assert_eq!(links[0].byte_offset, 4);
    }

    #[test]
    fn link_with_display() {
        let content = "Check [[ProjectAlpha|my project]] out.";
        let links = extract_links(content);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "ProjectAlpha");
        assert_eq!(links[0].display.as_deref(), Some("my project"));
    }

    #[test]
    fn link_multiple() {
        let content = "See [[A]] and [[B]] and [[C]].";
        let links = extract_links(content);
        assert_eq!(links.len(), 3);
    }

    #[test]
    fn link_inside_code_block_ignored() {
        let content = "text\n```\n[[NotALink]]\n```\n[[RealLink]]\n";
        let links = extract_links(content);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "RealLink");
    }

    // ── Tag tests ────────────────────────────────────────────────

    #[test]
    fn tag_simple() {
        let content = "Some text #work and more";
        let tags = extract_inline_tags(content);
        assert_eq!(tags, vec!["work"]);
    }

    #[test]
    fn tag_nested() {
        let content = "Tagged #parent/child here";
        let tags = extract_inline_tags(content);
        assert_eq!(tags, vec!["parent/child"]);
    }

    #[test]
    fn tag_heading_not_a_tag() {
        let content = "# Heading\n#realtag\n";
        let tags = extract_inline_tags(content);
        assert_eq!(tags, vec!["realtag"]);
    }

    #[test]
    fn tag_in_code_block_ignored() {
        let content = "```\n#notag\n```\n#realtag\n";
        let tags = extract_inline_tags(content);
        assert_eq!(tags, vec!["realtag"]);
    }

    #[test]
    fn tag_deduplication() {
        let content = "#work some text #work again";
        let tags = extract_inline_tags(content);
        assert_eq!(tags, vec!["work"]);
    }

    // ── Auto-type tests ──────────────────────────────────────────

    #[test]
    fn auto_type_task() {
        let content = "# Tasks\n- [ ] Do thing\n- [x] Done thing\n- [ ] Another\n- [ ] More\n";
        let links = extract_links(content);
        let fm = HashMap::new();
        assert_eq!(detect_auto_type(content, &links, &fm), Some(DocType::Task));
    }

    #[test]
    fn auto_type_moc() {
        let content = "# Index\n[[A]]\n[[B]]\n[[C]]\n[[D]]\n[[E]]\n[[F]]\n";
        let links = extract_links(content);
        let fm = HashMap::new();
        assert_eq!(detect_auto_type(content, &links, &fm), Some(DocType::Moc));
    }

    #[test]
    fn auto_type_reference() {
        let content = "# Book\nSome notes";
        let links = vec![];
        let mut fm = HashMap::new();
        fm.insert("isbn".to_string(), serde_json::json!("978-0-123456-78-9"));
        assert_eq!(
            detect_auto_type(content, &links, &fm),
            Some(DocType::Reference)
        );
    }

    #[test]
    fn auto_type_none_for_normal() {
        let content = "# Regular Note\nJust some text with no special patterns.\n";
        let links = extract_links(content);
        let fm = HashMap::new();
        assert_eq!(detect_auto_type(content, &links, &fm), None);
    }

    // ── Full parser integration ──────────────────────────────────

    #[test]
    fn parser_can_parse() {
        let p = MarkdownParser;
        assert!(p.can_parse(Path::new("notes/test.md")));
        assert!(p.can_parse(Path::new("test.MD")));
        assert!(!p.can_parse(Path::new("test.txt")));
        assert!(!p.can_parse(Path::new("test.rs")));
    }

    #[test]
    fn parser_full_document() {
        let p = MarkdownParser;
        let content = b"---\ntags:\n  - work\n---\n# Introduction\nSee [[ProjectAlpha]] for details.\n#rust\n## Details\nMore text\n";
        let result = p.parse(Path::new("test.md"), content).unwrap();

        assert_eq!(result.tags, vec!["work", "rust"]);
        assert_eq!(result.links.len(), 1);
        assert_eq!(result.links[0].target, "ProjectAlpha");
        assert!(result.chunks.len() >= 3); // frontmatter + 2 sections
        assert_eq!(result.chunks[0].kind, ChunkKind::Frontmatter);
    }

    #[test]
    fn parser_invalid_utf8() {
        let p = MarkdownParser;
        let content = &[0xFF, 0xFE, 0x00];
        let result = p.parse(Path::new("test.md"), content);
        assert!(matches!(result, Err(ParseError::InvalidUtf8)));
    }

    // ── Hierarchical tag flattening ──────────────────────────────

    #[test]
    fn flatten_hierarchical_simple() {
        let tags = vec!["work/project/alpha".to_string()];
        let flat = flatten_hierarchical_tags(tags);
        assert_eq!(flat, vec!["work", "work/project", "work/project/alpha"]);
    }

    #[test]
    fn flatten_hierarchical_mixed() {
        let tags = vec!["work/project".to_string(), "personal".to_string()];
        let flat = flatten_hierarchical_tags(tags);
        assert_eq!(flat, vec!["work", "work/project", "personal"]);
    }

    #[test]
    fn flatten_hierarchical_no_slash() {
        let tags = vec!["simple".to_string()];
        let flat = flatten_hierarchical_tags(tags);
        assert_eq!(flat, vec!["simple"]);
    }

    #[test]
    fn flatten_hierarchical_dedup() {
        let tags = vec!["a/b".to_string(), "a/c".to_string()];
        let flat = flatten_hierarchical_tags(tags);
        assert_eq!(flat, vec!["a", "a/b", "a/c"]);
    }

    #[test]
    fn parser_flattens_hierarchical_tags() {
        let p = MarkdownParser;
        let content = b"---\ntags:\n  - work/project/alpha\n---\n# Note\n#personal/finance\n";
        let result = p.parse(Path::new("test.md"), content).unwrap();
        assert!(result.tags.contains(&"work".to_string()));
        assert!(result.tags.contains(&"work/project".to_string()));
        assert!(result.tags.contains(&"work/project/alpha".to_string()));
        assert!(result.tags.contains(&"personal".to_string()));
        assert!(result.tags.contains(&"personal/finance".to_string()));
    }
}

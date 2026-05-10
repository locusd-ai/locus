//! Integration tests using fixture files.

use std::path::Path;

use biem_core::{ChunkKind, NoteType, Parser};
use biem_parser::markdown::MarkdownParser;

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("Failed to read fixture {name}: {e}"))
}

#[test]
fn simple_note() {
    let p = MarkdownParser;
    let content = fixture("simple.md");
    let result = p.parse(Path::new("simple.md"), &content).unwrap();

    // Frontmatter tags
    assert!(result.tags.contains(&"work".to_string()));
    assert!(result.tags.contains(&"rust".to_string()));
    // Inline tag
    assert!(result.tags.contains(&"inline-tag".to_string()));

    // Links
    assert_eq!(result.links.len(), 2);
    assert_eq!(result.links[0].target, "ProjectAlpha");
    assert_eq!(result.links[1].target, "ProjectBeta");
    assert_eq!(result.links[1].display.as_deref(), Some("the beta project"));

    // Chunks: frontmatter + 3 sections (Intro, Details, Deep Section)
    assert_eq!(result.chunks[0].kind, ChunkKind::Frontmatter);
    let sections: Vec<_> = result
        .chunks
        .iter()
        .filter(|c| c.kind == ChunkKind::Section)
        .collect();
    assert_eq!(sections.len(), 3);
    assert_eq!(sections[0].label.as_deref(), Some("Introduction"));
    assert_eq!(sections[0].depth, 1);
    assert_eq!(sections[1].depth, 2);
    assert_eq!(sections[2].depth, 3);
}

#[test]
fn task_note_detected() {
    let p = MarkdownParser;
    let content = fixture("task-note.md");
    let result = p.parse(Path::new("task-note.md"), &content).unwrap();
    assert_eq!(result.auto_type, Some(NoteType::Task));
}

#[test]
fn moc_detected() {
    let p = MarkdownParser;
    let content = fixture("moc.md");
    let result = p.parse(Path::new("moc.md"), &content).unwrap();
    assert_eq!(result.auto_type, Some(NoteType::Moc));
    assert!(result.links.len() >= 5);
}

#[test]
fn no_frontmatter_file() {
    let p = MarkdownParser;
    let content = fixture("no-frontmatter.md");
    let result = p.parse(Path::new("no-frontmatter.md"), &content).unwrap();

    assert!(result.frontmatter.is_empty());
    assert!(result.tags.is_empty());
    assert_eq!(result.links.len(), 1);
    assert_eq!(result.links[0].target, "link");

    // Should have sections, no frontmatter chunk
    assert!(result
        .chunks
        .iter()
        .all(|c| c.kind != ChunkKind::Frontmatter));
}

#[test]
fn code_blocks_ignored() {
    let p = MarkdownParser;
    let content = fixture("code-blocks.md");
    let result = p.parse(Path::new("code-blocks.md"), &content).unwrap();

    // Only real links, not ones inside code blocks
    let targets: Vec<_> = result.links.iter().map(|l| l.target.as_str()).collect();
    assert!(targets.contains(&"RealLink"));
    assert!(!targets.contains(&"NotALink"));
    assert!(!targets.contains(&"AlsoNotALink"));

    // Only real tags
    assert!(result.tags.contains(&"real-tag".to_string()));
    assert!(result.tags.contains(&"another-tag".to_string()));
    assert!(result.tags.contains(&"code".to_string())); // from frontmatter
    assert!(!result.tags.contains(&"not-a-tag-either".to_string()));
}

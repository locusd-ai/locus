//! Synthetic Obsidian vault generator.
//!
//! Shared module used by both criterion benchmarks and the scale-test binary.
//! Generates realistic markdown files with frontmatter, headings, tags, links,
//! task checkboxes, and varying content length.

use rand::prelude::*;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::{Path, PathBuf};

const TAGS: &[&str] = &[
    "project", "work", "personal", "research", "meeting", "idea",
    "todo", "done", "review", "brainstorm", "reference", "daily",
    "weekly", "planning", "learning", "rust", "scala", "python",
    "architecture", "design", "bug", "feature", "question", "answer",
    "journal", "health", "finance", "book", "article", "video",
];

const LINK_TARGETS: &[&str] = &[
    "Home", "Projects", "People", "Meetings", "Resources",
    "Architecture Overview", "API Design", "Database Schema",
    "Sprint Planning", "Retrospective", "Daily Standup",
    "Code Review Guidelines", "Testing Strategy", "Deployment",
    "Monitoring", "Incident Response", "Team Handbook",
    "Onboarding", "Tech Radar", "Decision Log",
];

const FOLDERS: &[&str] = &[
    "daily", "projects", "meetings", "references", "ideas",
    "projects/locus", "projects/client-a", "projects/infra",
    "people", "areas/health", "areas/finance", "areas/learning",
    "templates", "archive", "inbox",
];

const LOREM: &[&str] = &[
    "Lorem ipsum dolor sit amet, consectetur adipiscing elit.",
    "Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.",
    "Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris.",
    "Duis aute irure dolor in reprehenderit in voluptate velit esse cillum.",
    "Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia.",
    "Curabitur pretium tincidunt lacus sed gravida nulla facilisi.",
    "Vestibulum ante ipsum primis in faucibus orci luctus et ultrices posuere.",
    "Maecenas sed diam eget risus varius blandit sit amet non magna.",
    "Praesent commodo cursus magna, vel scelerisque nisl consectetur et.",
    "Donec ullamcorper nulla non metus auctor fringilla sed posuere velit.",
    "Integer posuere erat a ante venenatis dapibus posuere velit aliquet.",
    "Aenean lacinia bibendum nulla sed consectetur praesent commodo.",
    "Fusce dapibus tellus ac cursus commodo tortor mauris condimentum nibh.",
    "Nullam quis risus eget urna mollis ornare vel eu leo morbi leo risus.",
    "Vivamus sagittis lacus vel augue laoreet rutrum faucibus dolor auctor.",
];

const HEADINGS: &[&str] = &[
    "Overview", "Background", "Goals", "Requirements", "Design",
    "Implementation", "Testing", "Deployment", "Monitoring",
    "Next Steps", "Open Questions", "Decisions", "Notes",
    "Action Items", "Summary", "Analysis", "Observations",
    "Key Takeaways", "References", "Related",
];

pub struct VaultConfig {
    pub num_files: usize,
    /// Approximate paragraphs per file (randomised ±50%)
    pub avg_paragraphs: usize,
    /// Chance a note is a task-style note (with checkboxes)
    pub task_ratio: f64,
    /// Chance a note is a MOC-style note (mostly links)
    pub moc_ratio: f64,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            num_files: 1000,
            avg_paragraphs: 8,
            task_ratio: 0.15,
            moc_ratio: 0.05,
        }
    }
}

/// Generate a synthetic vault and return the paths of created files.
pub fn generate_vault(root: &Path, config: &VaultConfig) -> Vec<PathBuf> {
    let mut rng = StdRng::seed_from_u64(42);
    let mut paths = Vec::with_capacity(config.num_files);

    for folder in FOLDERS {
        fs::create_dir_all(root.join(folder)).unwrap();
    }

    for i in 0..config.num_files {
        let folder = FOLDERS[rng.gen_range(0..FOLDERS.len())];
        let filename = format!("note-{:07}.md", i);
        let path = root.join(folder).join(&filename);

        let roll: f64 = rng.gen();
        let content = if roll < config.moc_ratio {
            generate_moc(&mut rng)
        } else if roll < config.moc_ratio + config.task_ratio {
            generate_task_note(&mut rng, config.avg_paragraphs)
        } else {
            generate_regular_note(&mut rng, config.avg_paragraphs)
        };

        fs::write(&path, content).unwrap();
        paths.push(path);
    }

    paths
}

/// Return the total size in bytes of all files under `root`.
pub fn vault_size_bytes(root: &Path) -> u64 {
    let mut total = 0u64;
    walk_size(root, &mut total);
    total
}

fn walk_size(dir: &Path, total: &mut u64) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk_size(&path, total);
            } else if let Ok(meta) = path.metadata() {
                *total += meta.len();
            }
        }
    }
}

fn generate_frontmatter(rng: &mut StdRng) -> String {
    let mut fm = String::from("---\n");
    let n_tags = rng.gen_range(1..=4);
    let tags: Vec<&str> = TAGS.choose_multiple(rng, n_tags).copied().collect();
    writeln!(fm, "tags: [{}]", tags.join(", ")).unwrap();
    if rng.gen_bool(0.7) {
        let day = rng.gen_range(1..=28);
        let month = rng.gen_range(1..=12);
        writeln!(fm, "date: 2025-{month:02}-{day:02}").unwrap();
    }
    if rng.gen_bool(0.2) {
        writeln!(fm, "aliases: [\"alias-{}\"]", rng.gen_range(0..1000)).unwrap();
    }
    fm.push_str("---\n\n");
    fm
}

fn generate_regular_note(rng: &mut StdRng, avg_paragraphs: usize) -> String {
    let mut content = generate_frontmatter(rng);
    let title_word = HEADINGS[rng.gen_range(0..HEADINGS.len())];
    writeln!(content, "# {title_word}\n").unwrap();

    let n_paragraphs = rng.gen_range(avg_paragraphs / 2..=avg_paragraphs * 3 / 2);
    for p in 0..n_paragraphs {
        if p > 0 && rng.gen_bool(0.3) {
            let depth = rng.gen_range(2..=4);
            let heading = HEADINGS[rng.gen_range(0..HEADINGS.len())];
            writeln!(content, "{} {heading}\n", "#".repeat(depth)).unwrap();
        }
        let n_sentences = rng.gen_range(2..=5);
        for s in 0..n_sentences {
            let sentence = LOREM[rng.gen_range(0..LOREM.len())];
            content.push_str(sentence);
            if rng.gen_bool(0.15) {
                let target = LINK_TARGETS[rng.gen_range(0..LINK_TARGETS.len())];
                write!(content, " See [[{target}]].").unwrap();
            }
            if s < n_sentences - 1 {
                content.push(' ');
            }
        }
        content.push_str("\n\n");
    }
    content
}

fn generate_task_note(rng: &mut StdRng, avg_paragraphs: usize) -> String {
    let mut content = generate_frontmatter(rng);
    writeln!(content, "# Tasks\n").unwrap();

    let n_intro = rng.gen_range(1..=3);
    for _ in 0..n_intro {
        let sentence = LOREM[rng.gen_range(0..LOREM.len())];
        writeln!(content, "{sentence}\n").unwrap();
    }

    let n_tasks = rng.gen_range(3..=15);
    for _ in 0..n_tasks {
        let done = if rng.gen_bool(0.4) { "x" } else { " " };
        let task_text = LOREM[rng.gen_range(0..LOREM.len())];
        let short = &task_text[..task_text.len().min(60)];
        writeln!(content, "- [{done}] {short}").unwrap();
    }
    content.push('\n');

    if avg_paragraphs > 4 {
        writeln!(content, "## Notes\n").unwrap();
        let sentence = LOREM[rng.gen_range(0..LOREM.len())];
        writeln!(content, "{sentence}\n").unwrap();
    }
    content
}

fn generate_moc(rng: &mut StdRng) -> String {
    let mut content = generate_frontmatter(rng);
    writeln!(content, "# Map of Content\n").unwrap();
    writeln!(content, "An index of related notes.\n").unwrap();

    let n_links = rng.gen_range(10..=30);
    let targets: Vec<&str> = LINK_TARGETS
        .choose_multiple(rng, n_links.min(LINK_TARGETS.len()))
        .copied()
        .collect();
    for target in &targets {
        writeln!(content, "- [[{target}]]").unwrap();
    }
    for i in 0..(n_links.saturating_sub(targets.len())) {
        writeln!(content, "- [[Generated Note {i}]]").unwrap();
    }
    content.push('\n');
    content
}

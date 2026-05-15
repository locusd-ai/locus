//! Step 8: Integration tests — index multi-language code, verify queries and chunk accuracy.

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use biem_bitmap::memory::InMemoryBitmapStore;
    use biem_code::CodeParser;
    use biem_core::bitmap::BitmapStore;
    use biem_core::query::{Filter, QueryEngine, QueryRequest};
    use biem_core::registry::Registry;
    use biem_ingest::IngestionPipeline;
    use biem_parser::markdown::MarkdownParser;
    use biem_query::BitmapQueryEngine;
    use biem_registry::memory::InMemoryRegistry;

    /// Build a pipeline with both markdown and code parsers.
    fn make_pipeline() -> IngestionPipeline {
        IngestionPipeline::new(
            vec![Box::new(MarkdownParser), Box::new(CodeParser::new())],
            Box::new(InMemoryRegistry::new()),
            Box::new(InMemoryBitmapStore::new()),
        )
    }

    /// Build a pipeline, bulk-index a dir, return query engine.
    fn index_and_query(dir: &Path) -> BitmapQueryEngine {
        let mut pipeline = make_pipeline();
        let result = pipeline.bulk_index(dir).unwrap();
        assert!(result.docs_indexed > 0, "should index at least one doc");
        let (_parsers, registry, bitmap_store) = pipeline.into_parts();
        BitmapQueryEngine::new(bitmap_store, registry)
    }

    // ── Rust integration ─────────────────────────────────────────

    fn write_rust_project(dir: &Path) {
        // lib.rs — main source
        fs::write(
            dir.join("lib.rs"),
            r#"use serde::Serialize;
use std::collections::HashMap;
use roaring::RoaringBitmap;

pub struct BitmapIndex {
    store: HashMap<String, RoaringBitmap>,
}

impl BitmapIndex {
    pub fn new() -> Self {
        Self { store: HashMap::new() }
    }

    pub fn insert(&mut self, key: &str, id: u32) {
        self.store.entry(key.to_string())
            .or_insert_with(RoaringBitmap::new)
            .insert(id);
    }

    pub fn intersect(&self, keys: &[&str]) -> RoaringBitmap {
        let mut result = RoaringBitmap::new();
        for (i, key) in keys.iter().enumerate() {
            if let Some(bm) = self.store.get(*key) {
                if i == 0 { result = bm.clone(); } else { result &= bm; }
            }
        }
        result
    }

    fn compact_internal(&mut self) {
        self.store.retain(|_, v| !v.is_empty());
    }
}

pub async fn process_batch(items: Vec<u32>) -> Vec<u32> {
    items.into_iter().map(|x| x * 2).collect()
}

pub const MAX_BATCH_SIZE: usize = 1000;
"#,
        )
        .unwrap();

        // tests.rs — test file
        let test_dir = dir.join("tests");
        fs::create_dir_all(&test_dir).unwrap();
        fs::write(
            test_dir.join("bitmap_tests.rs"),
            r#"use super::BitmapIndex;

#[test]
fn test_insert_and_intersect() {
    let mut idx = BitmapIndex::new();
    idx.insert("a", 1);
    idx.insert("b", 1);
    let result = idx.intersect(&["a", "b"]);
    assert!(result.contains(1));
}
"#,
        )
        .unwrap();
    }

    #[test]
    fn test_rust_lang_and_kind_filter() {
        let dir = tempfile::tempdir().unwrap();
        write_rust_project(dir.path());
        let engine = index_and_query(dir.path());

        // lang:rust AND kind:function → should find public functions
        let result = engine
            .query(QueryRequest {
                filter: Filter::And(vec![
                    Filter::Key("lang:rust".into()),
                    Filter::Key("kind:function".into()),
                ]),
                limit: Some(50),
                offset: None,
            })
            .unwrap();
        assert!(
            result.total_matching > 0,
            "should find docs with lang:rust AND kind:function"
        );
    }

    #[test]
    fn test_rust_visibility_filter() {
        let dir = tempfile::tempdir().unwrap();
        write_rust_project(dir.path());
        let engine = index_and_query(dir.path());

        // visibility:public AND kind:method
        let result = engine
            .query(QueryRequest {
                filter: Filter::And(vec![
                    Filter::Key("visibility:public".into()),
                    Filter::Key("kind:method".into()),
                ]),
                limit: Some(50),
                offset: None,
            })
            .unwrap();
        assert!(result.total_matching > 0, "should find public methods");

        // visibility:private should also have results (compact_internal)
        let priv_result = engine
            .query(QueryRequest {
                filter: Filter::Key("visibility:private".into()),
                limit: Some(50),
                offset: None,
            })
            .unwrap();
        assert!(priv_result.total_matching > 0, "should find private items");
    }

    #[test]
    fn test_rust_import_filter() {
        let dir = tempfile::tempdir().unwrap();
        write_rust_project(dir.path());
        let engine = index_and_query(dir.path());

        // import:roaring AND kind:method
        let result = engine
            .query(QueryRequest {
                filter: Filter::And(vec![
                    Filter::Key("import:roaring".into()),
                    Filter::Key("kind:method".into()),
                ]),
                limit: Some(50),
                offset: None,
            })
            .unwrap();
        assert!(
            result.total_matching > 0,
            "should find methods in files importing roaring"
        );
    }

    #[test]
    fn test_rust_async_filter() {
        let dir = tempfile::tempdir().unwrap();
        write_rust_project(dir.path());
        let engine = index_and_query(dir.path());

        let result = engine
            .query(QueryRequest {
                filter: Filter::Key("async:true".into()),
                limit: Some(50),
                offset: None,
            })
            .unwrap();
        assert!(result.total_matching > 0, "should find async functions");
    }

    #[test]
    fn test_rust_test_file_type() {
        let dir = tempfile::tempdir().unwrap();
        write_rust_project(dir.path());
        let engine = index_and_query(dir.path());

        let result = engine
            .query(QueryRequest {
                filter: Filter::Key("type:test_file".into()),
                limit: Some(50),
                offset: None,
            })
            .unwrap();
        assert!(result.total_matching > 0, "should find test files");
    }

    #[test]
    fn test_chunk_labels_match_source() {
        let dir = tempfile::tempdir().unwrap();
        write_rust_project(dir.path());

        let mut pipeline = make_pipeline();
        pipeline.bulk_index(dir.path()).unwrap();
        let (_parsers, registry, _bitmap_store) = pipeline.into_parts();

        let docs = registry.list_all_docs().unwrap();
        let lib_doc = docs.iter().find(|d| d.file_path.ends_with("lib.rs")).unwrap();
        let chunks = registry.get_chunks(lib_doc.doc_id).unwrap();

        let labels: Vec<_> = chunks.iter().filter_map(|c| c.label.as_deref()).collect();
        assert!(labels.contains(&"BitmapIndex"), "should have struct label");
        assert!(labels.iter().any(|l| l.contains("new")), "should have new method");
        assert!(labels.iter().any(|l| l.contains("intersect")), "should have intersect method");
        assert!(labels.iter().any(|l| l.contains("process_batch")), "should have process_batch fn");
        assert!(labels.contains(&"MAX_BATCH_SIZE"), "should have constant label");
    }

    #[test]
    fn test_chunk_byte_ranges_accurate() {
        let dir = tempfile::tempdir().unwrap();
        let content = r#"pub fn hello() -> String {
    "hello".to_string()
}

pub fn world() -> &'static str {
    "world"
}
"#;
        fs::write(dir.path().join("funcs.rs"), content).unwrap();

        let mut pipeline = make_pipeline();
        pipeline.bulk_index(dir.path()).unwrap();
        let (_parsers, registry, _bitmap_store) = pipeline.into_parts();

        let docs = registry.list_all_docs().unwrap();
        let doc = &docs[0];
        let chunks = registry.get_chunks(doc.doc_id).unwrap();

        for chunk in &chunks {
            let start = chunk.byte_start as usize;
            let end = chunk.byte_end as usize;
            assert!(end <= content.len(), "byte range should be within content");
            let extracted = &content[start..end];
            if let Some(ref label) = chunk.label {
                assert!(
                    extracted.contains(label),
                    "extracted source should contain label '{}', got: {}",
                    label,
                    &extracted[..extracted.len().min(80)]
                );
            }
        }
    }

    // ── Multi-language integration ───────────────────────────────

    #[test]
    fn test_mixed_language_project() {
        let dir = tempfile::tempdir().unwrap();

        // Rust file
        fs::write(
            dir.path().join("lib.rs"),
            "pub fn rust_fn() -> u32 { 42 }\n",
        )
        .unwrap();

        // TypeScript file
        fs::write(
            dir.path().join("index.ts"),
            "export function tsFn(): number { return 42; }\n",
        )
        .unwrap();

        // Python file
        fs::write(
            dir.path().join("main.py"),
            "def py_fn():\n    return 42\n",
        )
        .unwrap();

        // Markdown file
        fs::write(
            dir.path().join("README.md"),
            "# Project\nSome docs\n",
        )
        .unwrap();

        let mut pipeline = make_pipeline();
        let result = pipeline.bulk_index(dir.path()).unwrap();
        assert_eq!(result.docs_indexed, 4, "should index all 4 files");

        let (_parsers, _registry, bitmap_store) = pipeline.into_parts();

        // Each language should have its bitmap key
        assert!(!bitmap_store.get("lang:rust").unwrap().is_empty());
        assert!(!bitmap_store.get("lang:typescript").unwrap().is_empty());
        assert!(!bitmap_store.get("lang:python").unwrap().is_empty());
        // Markdown gets source:obsidian, not lang:*
        assert!(!bitmap_store.get("source:obsidian").unwrap().is_empty());
    }

    // ── Ignore integration ───────────────────────────────────────

    #[test]
    fn test_code_project_ignores_target_dir() {
        let dir = tempfile::tempdir().unwrap();

        fs::write(dir.path().join("lib.rs"), "pub fn good() {}\n").unwrap();

        let target = dir.path().join("target").join("debug");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("generated.rs"), "fn bad() {}\n").unwrap();

        let mut pipeline = make_pipeline();
        let result = pipeline.bulk_index(dir.path()).unwrap();
        assert_eq!(result.docs_indexed, 1, "should only index lib.rs, not target/");
    }

    // ── Performance ──────────────────────────────────────────────

    #[test]
    fn test_parsing_throughput() {
        let dir = tempfile::tempdir().unwrap();

        // Generate 100 small Rust files
        for i in 0..100 {
            let content = format!(
                "pub fn func_{i}(x: u32) -> u32 {{ x + {i} }}\n\
                 pub struct Type{i} {{ value: u32 }}\n\
                 impl Type{i} {{ pub fn new() -> Self {{ Self {{ value: {i} }} }} }}\n"
            );
            fs::write(dir.path().join(format!("mod_{i}.rs")), content).unwrap();
        }

        let mut pipeline = make_pipeline();
        let result = pipeline.bulk_index(dir.path()).unwrap();

        assert_eq!(result.docs_indexed, 100);
        let files_per_sec = if result.duration_ms > 0 {
            100_000 / result.duration_ms // 100 files / duration_ms * 1000
        } else {
            99999
        };
        eprintln!(
            "Throughput: {} files in {}ms ({} files/s)",
            result.docs_indexed, result.duration_ms, files_per_sec
        );
        // Should be well above 5K files/s for small files
        assert!(
            files_per_sec > 100,
            "parsing throughput should be reasonable ({} files/s)",
            files_per_sec
        );
    }
}

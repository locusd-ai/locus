//! Integration test: index Rust source files with CodeParser → verify bitmap keys.

#[cfg(test)]
mod tests {
    use std::fs;

    use locus_bitmap::memory::InMemoryBitmapStore;
    use locus_code::CodeParser;
    use locus_core::bitmap::BitmapStore;
    use locus_ingest::IngestionPipeline;
    use locus_registry::memory::InMemoryRegistry;

    fn make_code_pipeline() -> IngestionPipeline {
        IngestionPipeline::new(
            vec![Box::new(CodeParser::new())],
            Box::new(InMemoryRegistry::new()),
            Box::new(InMemoryBitmapStore::new()),
        )
    }

    #[test]
    fn test_index_rust_file_generates_code_bitmap_keys() {
        let dir = tempfile::tempdir().unwrap();
        let rs_file = dir.path().join("lib.rs");
        fs::write(
            &rs_file,
            r#"use serde::Serialize;
use std::collections::HashMap;

pub struct Config {
    pub name: String,
}

impl Config {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string() }
    }
}

pub fn process(data: &[u8]) -> Vec<u8> {
    data.to_vec()
}

fn internal_helper() {}
"#,
        )
        .unwrap();

        let mut pipeline = make_code_pipeline();

        let event = locus_core::types::ChangeEvent {
            path: rs_file.clone(),
            kind: locus_core::types::ChangeKind::Created,
        };
        pipeline.process_event(&event).unwrap();

        let (_, _, bitmap_store) = pipeline.into_parts();

        // Helper: assert a bitmap key exists and is non-empty
        let assert_key = |key: &str| {
            let bm = bitmap_store.get(key).unwrap();
            assert!(!bm.is_empty(), "bitmap '{key}' should be non-empty");
        };

        assert_key("lang:rust");
        assert_key("kind:function");
        assert_key("kind:method");
        assert_key("kind:struct");
        assert_key("visibility:public");
        assert_key("visibility:private");
        assert_key("import:serde");
        assert_key("import:std");
        assert_key("source:code");
        assert_key("type:source_file");
    }
}

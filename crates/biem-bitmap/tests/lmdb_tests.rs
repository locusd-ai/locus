//! Integration tests for LmdbBitmapStore.

use biem_bitmap::lmdb::LmdbBitmapStore;
use biem_core::BitmapStore;
use roaring::RoaringBitmap;
use tempfile::TempDir;

fn open_store() -> (LmdbBitmapStore, TempDir) {
    let dir = TempDir::new().unwrap();
    let store = LmdbBitmapStore::new(dir.path()).unwrap();
    (store, dir)
}

#[test]
fn put_get_round_trip() {
    let (mut s, _dir) = open_store();
    let mut bm = RoaringBitmap::new();
    bm.insert(1);
    bm.insert(42);
    s.put("tag:work", &bm).unwrap();
    assert_eq!(s.get("tag:work").unwrap(), bm);
}

#[test]
fn get_missing_returns_empty() {
    let (s, _dir) = open_store();
    assert!(s.get("nope").unwrap().is_empty());
}

#[test]
fn insert_id_and_remove_id() {
    let (mut s, _dir) = open_store();
    s.insert_id("tag:a", 1).unwrap();
    s.insert_id("tag:a", 2).unwrap();
    assert!(s.get("tag:a").unwrap().contains(1));
    assert!(s.get("tag:a").unwrap().contains(2));

    s.remove_id("tag:a", 1).unwrap();
    assert!(!s.get("tag:a").unwrap().contains(1));
    assert!(s.get("tag:a").unwrap().contains(2));
}

#[test]
fn delete_removes_key() {
    let (mut s, _dir) = open_store();
    s.insert_id("tag:a", 1).unwrap();
    assert!(s.exists("tag:a"));
    s.delete("tag:a").unwrap();
    assert!(!s.exists("tag:a"));
}

#[test]
fn bulk_put_writes_all() {
    let (mut s, _dir) = open_store();
    let mut bm1 = RoaringBitmap::new();
    bm1.insert(1);
    let mut bm2 = RoaringBitmap::new();
    bm2.insert(2);

    s.bulk_put(vec![
        ("tag:a".into(), bm1.clone()),
        ("tag:b".into(), bm2.clone()),
    ])
    .unwrap();

    assert_eq!(s.get("tag:a").unwrap(), bm1);
    assert_eq!(s.get("tag:b").unwrap(), bm2);
}

#[test]
fn tombstone_round_trip() {
    let (mut s, _dir) = open_store();
    s.tombstone(10).unwrap();
    s.tombstone(20).unwrap();
    let ts = s.get_tombstone().unwrap();
    assert!(ts.contains(10));
    assert!(ts.contains(20));
    assert_eq!(ts.len(), 2);
}

#[test]
fn list_keys_with_prefix() {
    let (mut s, _dir) = open_store();
    s.insert_id("tag:a", 1).unwrap();
    s.insert_id("tag:b", 2).unwrap();
    s.insert_id("folder:/x", 3).unwrap();
    s.tombstone(1).unwrap();

    let mut keys = s.list_keys(Some("tag:")).unwrap();
    keys.sort();
    assert_eq!(keys, vec!["tag:a", "tag:b"]);

    // tombstone excluded from unfiltered list
    let all = s.list_keys(None).unwrap();
    assert!(!all.iter().any(|k| k == "__tombstone__"));
    assert_eq!(all.len(), 3);
}

#[test]
fn cardinality() {
    let (mut s, _dir) = open_store();
    assert_eq!(s.cardinality("missing").unwrap(), 0);
    s.insert_id("tag:a", 1).unwrap();
    s.insert_id("tag:a", 2).unwrap();
    assert_eq!(s.cardinality("tag:a").unwrap(), 2);
}

#[test]
fn data_persists_across_reopen() {
    let dir = TempDir::new().unwrap();
    {
        let mut s = LmdbBitmapStore::new(dir.path()).unwrap();
        s.insert_id("tag:persist", 42).unwrap();
    }
    // Reopen
    let s = LmdbBitmapStore::new(dir.path()).unwrap();
    assert!(s.get("tag:persist").unwrap().contains(42));
}

#[test]
fn large_bitmap_round_trip() {
    let (mut s, _dir) = open_store();
    let mut bm = RoaringBitmap::new();
    for i in 0..10_000u32 {
        bm.insert(i);
    }
    s.put("large", &bm).unwrap();
    let got = s.get("large").unwrap();
    assert_eq!(got.len(), 10_000);
    assert_eq!(got, bm);
}

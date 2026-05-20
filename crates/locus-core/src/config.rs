//! Configuration and source registration for BIEM.
//!
//! Global config lives at `~/.locus/config.toml`. Each registered source
//! gets its own state directory containing registry (DuckDB) and bitmaps (LMDB).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::types::SourceType;

// ── Types ────────────────────────────────────────────────────────

/// Top-level BIEM configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct BiemConfig {
    /// Registered filesystem sources, keyed by a human-friendly name.
    #[serde(default)]
    pub sources: BTreeMap<String, SourceEntry>,

    /// Registered remote (API-polled) sources, keyed by a human-friendly name.
    #[serde(default)]
    pub remote_sources: BTreeMap<String, RemoteSourceEntry>,

    // Legacy alias: deserialize old configs that use "vaults"
    #[serde(default, rename = "vaults")]
    #[serde(skip_serializing)]
    vaults_compat: BTreeMap<String, SourceEntry>,
}

// ── Remote source config ─────────────────────────────────────────

fn default_poll_interval_secs() -> u64 { 300 }

/// A registered remote source (Confluence, Jira, Slack, etc.).
/// API credentials are NOT stored here — read from env vars at poll time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoteSourceEntry {
    /// Source kind: `"confluence"`, `"jira"`, `"slack"`.
    pub kind: String,
    /// State directory (DuckDB + LMDB live here).
    pub data_dir: PathBuf,
    /// Unix timestamp of the last successful poll. `None` = never polled (full sync).
    #[serde(default)]
    pub last_poll: Option<i64>,
    /// How often to poll in background mode (seconds).
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,

    // ── Confluence / Jira shared ──────────────────────────────────
    /// Base URL of the Atlassian instance, e.g. `https://org.atlassian.net`
    pub base_url: Option<String>,
    /// Atlassian account email used for Basic auth.
    pub username: Option<String>,

    // ── Confluence-specific ───────────────────────────────────────
    /// Confluence space keys to poll, e.g. `["ENG", "OPS"]`.
    pub spaces: Option<Vec<String>>,

    // ── Jira-specific ─────────────────────────────────────────────
    /// Jira project keys to poll, e.g. `["PROJ", "INFRA"]`.
    pub projects: Option<Vec<String>>,

    // ── Slack-specific ────────────────────────────────────────────
    /// Slack channel IDs to poll, e.g. `["C01234567"]`.
    pub channel_ids: Option<Vec<String>>,
    /// Human-readable channel names (parallel to channel_ids), used for tagging.
    pub channel_names: Option<Vec<String>>,
}

impl RemoteSourceEntry {
    pub fn kind(&self) -> &str {
        &self.kind
    }
}

impl BiemConfig {
    /// Merge legacy `vaults` entries into `sources` (for config migration).
    pub fn migrate_legacy(&mut self) {
        for (k, v) in std::mem::take(&mut self.vaults_compat) {
            self.sources.entry(k).or_insert(v);
        }
    }
}

/// A registered source entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceEntry {
    /// Canonical absolute path to the source root.
    pub path: PathBuf,
    /// The type of content at this path.
    #[serde(default)]
    pub source_type: SourceTypeConfig,
    /// Where state is stored.
    #[serde(default)]
    pub storage: StorageMode,
    /// Resolved state directory (registry + bitmaps live here).
    pub data_dir: PathBuf,
}

/// Source type for config serialization. Serializes as a plain string in TOML.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SourceTypeConfig {
    #[default]
    Obsidian,
    Code,
    Custom(String),
}

impl Serialize for SourceTypeConfig {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::Obsidian => "obsidian",
            Self::Code => "code",
            Self::Custom(s) => s.as_str(),
        })
    }
}

impl<'de> Deserialize<'de> for SourceTypeConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "obsidian" => Self::Obsidian,
            "code" => Self::Code,
            other => Self::Custom(other.to_string()),
        })
    }
}

impl From<SourceTypeConfig> for SourceType {
    fn from(c: SourceTypeConfig) -> Self {
        match c {
            SourceTypeConfig::Obsidian => SourceType::Obsidian,
            SourceTypeConfig::Code => SourceType::Code,
            SourceTypeConfig::Custom(s) => SourceType::Custom(s),
        }
    }
}

impl From<SourceType> for SourceTypeConfig {
    fn from(s: SourceType) -> Self {
        match s {
            SourceType::Obsidian => SourceTypeConfig::Obsidian,
            SourceType::Code => SourceTypeConfig::Code,
            SourceType::Custom(s) => SourceTypeConfig::Custom(s),
        }
    }
}

/// Where to store BIEM state for a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StorageMode {
    /// State in `~/.locus/sources/<name>/` (default).
    #[default]
    Global,
    /// State in `<source>/.locus/`.
    Local,
}

// ── Error ────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    TomlRead(#[from] toml::de::Error),
    #[error("TOML serialization error: {0}")]
    TomlWrite(#[from] toml::ser::Error),
    #[error("home directory not found")]
    NoHomeDir,
    #[error("source not registered: {0}")]
    SourceNotFound(String),
    #[error("source already registered as '{0}'")]
    SourceAlreadyRegistered(String),
}

// ── Functions ────────────────────────────────────────────────────

/// Returns the default global BIEM config directory (`~/.locus/`).
pub fn default_config_dir() -> Result<PathBuf, ConfigError> {
    let home = dirs::home_dir().ok_or(ConfigError::NoHomeDir)?;
    Ok(home.join(".locus"))
}

/// Load config from a specific directory. Returns default if file doesn't exist.
pub fn load_config_from(config_dir: &Path) -> Result<BiemConfig, ConfigError> {
    let path = config_dir.join("config.toml");
    if !path.exists() {
        return Ok(BiemConfig::default());
    }
    let contents = std::fs::read_to_string(&path)?;
    let mut config: BiemConfig = toml::from_str(&contents)?;
    config.migrate_legacy();
    Ok(config)
}

/// Load config from the default location (`~/.locus/config.toml`).
pub fn load_config() -> Result<BiemConfig, ConfigError> {
    load_config_from(&default_config_dir()?)
}

/// Save config to a specific directory. Creates the directory if needed.
pub fn save_config_to(config: &BiemConfig, config_dir: &Path) -> Result<(), ConfigError> {
    std::fs::create_dir_all(config_dir)?;
    let path = config_dir.join("config.toml");
    let contents = toml::to_string_pretty(config)?;
    std::fs::write(&path, contents)?;
    Ok(())
}

/// Save config to the default location (`~/.locus/config.toml`).
pub fn save_config(config: &BiemConfig) -> Result<(), ConfigError> {
    save_config_to(config, &default_config_dir()?)
}

/// Compute a deterministic short hash of a source path for use as a directory name.
/// Uses blake3, truncated to 12 hex chars.
pub fn source_hash(source_path: &Path) -> String {
    let hash = blake3::hash(source_path.to_string_lossy().as_bytes());
    hash.to_hex()[..12].to_string()
}

/// Derive a human-friendly source name from the directory name.
fn derive_source_name(source_path: &Path) -> String {
    source_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| source_hash(source_path))
}

/// Ensure the source name is unique in the config, appending a suffix if needed.
fn unique_source_name(base: &str, config: &BiemConfig) -> String {
    if !config.sources.contains_key(base) {
        return base.to_string();
    }
    for i in 2.. {
        let candidate = format!("{base}-{i}");
        if !config.sources.contains_key(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

/// Register a source in the config. Creates the state directory.
/// `locus_dir` is the root Locus config dir (e.g. `~/.locus/`), used for global storage.
pub fn register_source(
    source_path: &Path,
    source_type: SourceType,
    storage: StorageMode,
    locus_dir: &Path,
    config: &mut BiemConfig,
) -> Result<(String, SourceEntry), ConfigError> {
    let canonical = source_path.canonicalize().map_err(ConfigError::Io)?;

    // Check if already registered
    for (name, entry) in &config.sources {
        if entry.path == canonical {
            return Err(ConfigError::SourceAlreadyRegistered(name.clone()));
        }
    }

    let name = unique_source_name(&derive_source_name(&canonical), config);

    let data_dir = match storage {
        StorageMode::Global => locus_dir.join("sources").join(&name),
        StorageMode::Local => canonical.join(".locus"),
    };

    std::fs::create_dir_all(&data_dir)?;

    let entry = SourceEntry {
        path: canonical,
        source_type: source_type.into(),
        storage,
        data_dir,
    };

    config.sources.insert(name.clone(), entry.clone());
    Ok((name, entry))
}

/// Find a source entry by its path (canonicalized).
pub fn resolve_source(source_path: &Path, config: &BiemConfig) -> Result<SourceEntry, ConfigError> {
    let canonical = source_path.canonicalize().map_err(ConfigError::Io)?;

    for entry in config.sources.values() {
        if entry.path == canonical {
            return Ok(entry.clone());
        }
    }

    Err(ConfigError::SourceNotFound(
        canonical.to_string_lossy().into_owned(),
    ))
}

/// Remove a source from the config by path. Does NOT delete the state directory.
pub fn remove_source(source_path: &Path, config: &mut BiemConfig) -> Result<String, ConfigError> {
    let canonical = source_path.canonicalize().map_err(ConfigError::Io)?;

    let name = config
        .sources
        .iter()
        .find(|(_, entry)| entry.path == canonical)
        .map(|(name, _)| name.clone());

    match name {
        Some(n) => {
            config.sources.remove(&n);
            Ok(n)
        }
        None => Err(ConfigError::SourceNotFound(
            canonical.to_string_lossy().into_owned(),
        )),
    }
}

// ── Remote source helpers ────────────────────────────────────────

/// Register a remote source in the config and create its state directory.
pub fn register_remote_source(
    name: &str,
    mut entry: RemoteSourceEntry,
    locus_dir: &Path,
    config: &mut BiemConfig,
) -> Result<(), ConfigError> {
    if config.remote_sources.contains_key(name) {
        return Err(ConfigError::SourceAlreadyRegistered(name.to_string()));
    }

    if entry.data_dir.as_os_str().is_empty() {
        entry.data_dir = locus_dir.join("remote").join(name);
    }
    std::fs::create_dir_all(&entry.data_dir)?;
    config.remote_sources.insert(name.to_string(), entry);
    Ok(())
}

/// Remove a remote source from the config by name. Does NOT delete the state directory.
pub fn remove_remote_source(name: &str, config: &mut BiemConfig) -> Result<(), ConfigError> {
    if config.remote_sources.remove(name).is_none() {
        return Err(ConfigError::SourceNotFound(name.to_string()));
    }
    Ok(())
}

/// Update the `last_poll` timestamp for a remote source.
pub fn update_remote_last_poll(
    name: &str,
    ts: i64,
    config: &mut BiemConfig,
) -> Result<(), ConfigError> {
    config
        .remote_sources
        .get_mut(name)
        .ok_or_else(|| ConfigError::SourceNotFound(name.to_string()))
        .map(|e| e.last_poll = Some(ts))
}

// ── Backwards compatibility ──────────────────────────────────────

/// Legacy type alias.
pub type VaultEntry = SourceEntry;

/// Legacy alias — delegates to `register_source` with `SourceType::Obsidian`.
#[deprecated(note = "Use register_source instead")]
pub fn register_vault(
    vault_path: &Path,
    storage: StorageMode,
    locus_dir: &Path,
    config: &mut BiemConfig,
) -> Result<(String, SourceEntry), ConfigError> {
    register_source(vault_path, SourceType::Obsidian, storage, locus_dir, config)
}

/// Legacy alias — delegates to `resolve_source`.
#[deprecated(note = "Use resolve_source instead")]
pub fn resolve_vault(vault_path: &Path, config: &BiemConfig) -> Result<SourceEntry, ConfigError> {
    resolve_source(vault_path, config)
}

/// Legacy alias — delegates to `remove_source`.
#[deprecated(note = "Use remove_source instead")]
pub fn remove_vault(vault_path: &Path, config: &mut BiemConfig) -> Result<String, ConfigError> {
    remove_source(vault_path, config)
}

/// Legacy alias for source_hash.
#[deprecated(note = "Use source_hash instead")]
pub fn vault_hash(vault_path: &Path) -> String {
    source_hash(vault_path)
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_hash_deterministic() {
        let h1 = source_hash(Path::new("/foo/bar"));
        let h2 = source_hash(Path::new("/foo/bar"));
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 12);
    }

    #[test]
    fn test_source_hash_different_paths() {
        let h1 = source_hash(Path::new("/foo/bar"));
        let h2 = source_hash(Path::new("/foo/baz"));
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_load_missing_config_returns_default() {
        let tmp = tempfile::tempdir().unwrap();
        let config = load_config_from(tmp.path()).unwrap();
        assert!(config.sources.is_empty());
    }

    #[test]
    fn test_save_and_load_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = BiemConfig::default();
        config.sources.insert(
            "test".into(),
            SourceEntry {
                path: PathBuf::from("/tmp/vault"),
                source_type: SourceTypeConfig::Obsidian,
                storage: StorageMode::Global,
                data_dir: PathBuf::from("/tmp/state"),
            },
        );
        save_config_to(&config, tmp.path()).unwrap();
        let loaded = load_config_from(tmp.path()).unwrap();
        assert_eq!(config, loaded);
    }

    #[test]
    fn test_register_source_global() {
        let locus_dir = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let mut config = BiemConfig::default();

        let (name, entry) =
            register_source(source.path(), SourceType::Obsidian, StorageMode::Global, locus_dir.path(), &mut config)
                .unwrap();

        assert!(!name.is_empty());
        assert_eq!(entry.storage, StorageMode::Global);
        assert!(entry.data_dir.starts_with(locus_dir.path().join("sources")));
        assert!(entry.data_dir.exists());
    }

    #[test]
    fn test_register_source_local() {
        let locus_dir = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let mut config = BiemConfig::default();

        let (_name, entry) =
            register_source(source.path(), SourceType::Obsidian, StorageMode::Local, locus_dir.path(), &mut config)
                .unwrap();

        assert_eq!(entry.storage, StorageMode::Local);
        let canonical = source.path().canonicalize().unwrap();
        assert_eq!(entry.data_dir, canonical.join(".locus"));
        assert!(entry.data_dir.exists());
    }

    #[test]
    fn test_register_duplicate_errors() {
        let locus_dir = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let mut config = BiemConfig::default();

        register_source(source.path(), SourceType::Obsidian, StorageMode::Global, locus_dir.path(), &mut config).unwrap();
        let err =
            register_source(source.path(), SourceType::Obsidian, StorageMode::Global, locus_dir.path(), &mut config)
                .unwrap_err();
        assert!(matches!(err, ConfigError::SourceAlreadyRegistered(_)));
    }

    #[test]
    fn test_resolve_source() {
        let locus_dir = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let mut config = BiemConfig::default();

        let (_name, expected) =
            register_source(source.path(), SourceType::Obsidian, StorageMode::Global, locus_dir.path(), &mut config)
                .unwrap();
        let resolved = resolve_source(source.path(), &config).unwrap();
        assert_eq!(resolved.data_dir, expected.data_dir);
    }

    #[test]
    fn test_resolve_unknown_source_errors() {
        let source = tempfile::tempdir().unwrap();
        let config = BiemConfig::default();
        let err = resolve_source(source.path(), &config).unwrap_err();
        assert!(matches!(err, ConfigError::SourceNotFound(_)));
    }

    #[test]
    fn test_remove_source() {
        let locus_dir = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let mut config = BiemConfig::default();

        register_source(source.path(), SourceType::Obsidian, StorageMode::Global, locus_dir.path(), &mut config).unwrap();
        assert_eq!(config.sources.len(), 1);
        remove_source(source.path(), &mut config).unwrap();
        assert!(config.sources.is_empty());
    }

    #[test]
    fn test_full_round_trip() {
        let locus_dir = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let mut config = BiemConfig::default();

        register_source(source.path(), SourceType::Obsidian, StorageMode::Global, locus_dir.path(), &mut config).unwrap();
        save_config_to(&config, locus_dir.path()).unwrap();

        let loaded = load_config_from(locus_dir.path()).unwrap();
        let resolved = resolve_source(source.path(), &loaded).unwrap();
        assert!(resolved.data_dir.exists());
    }

    #[test]
    fn test_unique_name_dedup() {
        let locus_dir = tempfile::tempdir().unwrap();
        let s1 = tempfile::Builder::new().prefix("my-source").tempdir().unwrap();
        let s2 = tempfile::Builder::new().prefix("my-source").tempdir().unwrap();
        let mut config = BiemConfig::default();

        let (n1, _) = register_source(s1.path(), SourceType::Obsidian, StorageMode::Global, locus_dir.path(), &mut config).unwrap();
        let (n2, _) = register_source(s2.path(), SourceType::Obsidian, StorageMode::Global, locus_dir.path(), &mut config).unwrap();

        assert_ne!(n1, n2);
        assert_eq!(config.sources.len(), 2);
    }

    #[test]
    fn test_register_code_source() {
        let locus_dir = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let mut config = BiemConfig::default();

        let (_name, entry) =
            register_source(source.path(), SourceType::Code, StorageMode::Global, locus_dir.path(), &mut config)
                .unwrap();

        assert_eq!(entry.source_type, SourceTypeConfig::Code);
    }

    #[test]
    fn test_legacy_vaults_config_migration() {
        let tmp = tempfile::tempdir().unwrap();
        let toml_content = r#"
[vaults.old-vault]
path = "/tmp/old-vault"
storage = "global"
data_dir = "/tmp/state"
"#;
        std::fs::write(tmp.path().join("config.toml"), toml_content).unwrap();
        let config = load_config_from(tmp.path()).unwrap();
        assert!(config.sources.contains_key("old-vault"));
    }
}

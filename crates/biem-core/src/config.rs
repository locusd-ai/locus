//! Configuration and vault registration for BIEM.
//!
//! Global config lives at `~/.biem/config.toml`. Each registered vault
//! gets its own state directory containing registry (DuckDB) and bitmaps (LMDB).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ── Types ────────────────────────────────────────────────────────

/// Top-level BIEM configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct BiemConfig {
    /// Registered vaults, keyed by a human-friendly name.
    #[serde(default)]
    pub vaults: BTreeMap<String, VaultEntry>,
}

/// A registered vault entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VaultEntry {
    /// Canonical absolute path to the vault root.
    pub path: PathBuf,
    /// Where state is stored.
    #[serde(default)]
    pub storage: StorageMode,
    /// Resolved state directory (registry + bitmaps live here).
    pub data_dir: PathBuf,
}

/// Where to store BIEM state for a vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StorageMode {
    /// State in `~/.biem/vaults/<hash>/` (default).
    #[default]
    Global,
    /// State in `<vault>/.biem/`.
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
    #[error("vault not registered: {0}")]
    VaultNotFound(String),
    #[error("vault already registered as '{0}'")]
    VaultAlreadyRegistered(String),
}

// ── Functions ────────────────────────────────────────────────────

/// Returns the default global BIEM config directory (`~/.biem/`).
pub fn default_config_dir() -> Result<PathBuf, ConfigError> {
    let home = dirs::home_dir().ok_or(ConfigError::NoHomeDir)?;
    Ok(home.join(".biem"))
}

/// Load config from a specific directory. Returns default if file doesn't exist.
pub fn load_config_from(config_dir: &Path) -> Result<BiemConfig, ConfigError> {
    let path = config_dir.join("config.toml");
    if !path.exists() {
        return Ok(BiemConfig::default());
    }
    let contents = std::fs::read_to_string(&path)?;
    let config: BiemConfig = toml::from_str(&contents)?;
    Ok(config)
}

/// Load config from the default location (`~/.biem/config.toml`).
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

/// Save config to the default location (`~/.biem/config.toml`).
pub fn save_config(config: &BiemConfig) -> Result<(), ConfigError> {
    save_config_to(config, &default_config_dir()?)
}

/// Compute a deterministic short hash of a vault path for use as a directory name.
/// Uses blake3, truncated to 12 hex chars.
pub fn vault_hash(vault_path: &Path) -> String {
    let hash = blake3::hash(vault_path.to_string_lossy().as_bytes());
    hash.to_hex()[..12].to_string()
}

/// Derive a human-friendly vault name from the directory name.
fn derive_vault_name(vault_path: &Path) -> String {
    vault_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| vault_hash(vault_path))
}

/// Ensure the vault name is unique in the config, appending a suffix if needed.
fn unique_vault_name(base: &str, config: &BiemConfig) -> String {
    if !config.vaults.contains_key(base) {
        return base.to_string();
    }
    for i in 2.. {
        let candidate = format!("{base}-{i}");
        if !config.vaults.contains_key(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

/// Register a vault in the config. Creates the state directory.
/// `biem_dir` is the root BIEM config dir (e.g. `~/.biem/`), used for global storage.
pub fn register_vault(
    vault_path: &Path,
    storage: StorageMode,
    biem_dir: &Path,
    config: &mut BiemConfig,
) -> Result<(String, VaultEntry), ConfigError> {
    let canonical = vault_path.canonicalize().map_err(ConfigError::Io)?;

    // Check if already registered
    for (name, entry) in &config.vaults {
        if entry.path == canonical {
            return Err(ConfigError::VaultAlreadyRegistered(name.clone()));
        }
    }

    let data_dir = match storage {
        StorageMode::Global => {
            let hash = vault_hash(&canonical);
            biem_dir.join("vaults").join(hash)
        }
        StorageMode::Local => canonical.join(".biem"),
    };

    std::fs::create_dir_all(&data_dir)?;

    let name = unique_vault_name(&derive_vault_name(&canonical), config);
    let entry = VaultEntry {
        path: canonical,
        storage,
        data_dir,
    };

    config.vaults.insert(name.clone(), entry.clone());
    Ok((name, entry))
}

/// Find a vault entry by its path (canonicalized).
pub fn resolve_vault(vault_path: &Path, config: &BiemConfig) -> Result<VaultEntry, ConfigError> {
    let canonical = vault_path.canonicalize().map_err(ConfigError::Io)?;

    for entry in config.vaults.values() {
        if entry.path == canonical {
            return Ok(entry.clone());
        }
    }

    Err(ConfigError::VaultNotFound(
        canonical.to_string_lossy().into_owned(),
    ))
}

/// Remove a vault from the config by path. Does NOT delete the state directory.
pub fn remove_vault(vault_path: &Path, config: &mut BiemConfig) -> Result<String, ConfigError> {
    let canonical = vault_path.canonicalize().map_err(ConfigError::Io)?;

    let name = config
        .vaults
        .iter()
        .find(|(_, entry)| entry.path == canonical)
        .map(|(name, _)| name.clone());

    match name {
        Some(n) => {
            config.vaults.remove(&n);
            Ok(n)
        }
        None => Err(ConfigError::VaultNotFound(
            canonical.to_string_lossy().into_owned(),
        )),
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vault_hash_deterministic() {
        let h1 = vault_hash(Path::new("/foo/bar"));
        let h2 = vault_hash(Path::new("/foo/bar"));
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 12);
    }

    #[test]
    fn test_vault_hash_different_paths() {
        let h1 = vault_hash(Path::new("/foo/bar"));
        let h2 = vault_hash(Path::new("/foo/baz"));
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_load_missing_config_returns_default() {
        let tmp = tempfile::tempdir().unwrap();
        let config = load_config_from(tmp.path()).unwrap();
        assert!(config.vaults.is_empty());
    }

    #[test]
    fn test_save_and_load_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = BiemConfig::default();
        config.vaults.insert(
            "test".into(),
            VaultEntry {
                path: PathBuf::from("/tmp/vault"),
                storage: StorageMode::Global,
                data_dir: PathBuf::from("/tmp/state"),
            },
        );
        save_config_to(&config, tmp.path()).unwrap();
        let loaded = load_config_from(tmp.path()).unwrap();
        assert_eq!(config, loaded);
    }

    #[test]
    fn test_register_vault_global() {
        let biem_dir = tempfile::tempdir().unwrap();
        let vault = tempfile::tempdir().unwrap();
        let mut config = BiemConfig::default();

        let (name, entry) =
            register_vault(vault.path(), StorageMode::Global, biem_dir.path(), &mut config)
                .unwrap();

        assert!(!name.is_empty());
        assert_eq!(entry.storage, StorageMode::Global);
        assert!(entry.data_dir.starts_with(biem_dir.path().join("vaults")));
        assert!(entry.data_dir.exists());
    }

    #[test]
    fn test_register_vault_local() {
        let biem_dir = tempfile::tempdir().unwrap();
        let vault = tempfile::tempdir().unwrap();
        let mut config = BiemConfig::default();

        let (_name, entry) =
            register_vault(vault.path(), StorageMode::Local, biem_dir.path(), &mut config)
                .unwrap();

        assert_eq!(entry.storage, StorageMode::Local);
        let canonical = vault.path().canonicalize().unwrap();
        assert_eq!(entry.data_dir, canonical.join(".biem"));
        assert!(entry.data_dir.exists());
    }

    #[test]
    fn test_register_duplicate_errors() {
        let biem_dir = tempfile::tempdir().unwrap();
        let vault = tempfile::tempdir().unwrap();
        let mut config = BiemConfig::default();

        register_vault(vault.path(), StorageMode::Global, biem_dir.path(), &mut config).unwrap();
        let err =
            register_vault(vault.path(), StorageMode::Global, biem_dir.path(), &mut config)
                .unwrap_err();
        assert!(matches!(err, ConfigError::VaultAlreadyRegistered(_)));
    }

    #[test]
    fn test_resolve_vault() {
        let biem_dir = tempfile::tempdir().unwrap();
        let vault = tempfile::tempdir().unwrap();
        let mut config = BiemConfig::default();

        let (_name, expected) =
            register_vault(vault.path(), StorageMode::Global, biem_dir.path(), &mut config)
                .unwrap();
        let resolved = resolve_vault(vault.path(), &config).unwrap();
        assert_eq!(resolved.data_dir, expected.data_dir);
    }

    #[test]
    fn test_resolve_unknown_vault_errors() {
        let vault = tempfile::tempdir().unwrap();
        let config = BiemConfig::default();
        let err = resolve_vault(vault.path(), &config).unwrap_err();
        assert!(matches!(err, ConfigError::VaultNotFound(_)));
    }

    #[test]
    fn test_remove_vault() {
        let biem_dir = tempfile::tempdir().unwrap();
        let vault = tempfile::tempdir().unwrap();
        let mut config = BiemConfig::default();

        register_vault(vault.path(), StorageMode::Global, biem_dir.path(), &mut config).unwrap();
        assert_eq!(config.vaults.len(), 1);
        remove_vault(vault.path(), &mut config).unwrap();
        assert!(config.vaults.is_empty());
    }

    #[test]
    fn test_full_round_trip() {
        let biem_dir = tempfile::tempdir().unwrap();
        let vault = tempfile::tempdir().unwrap();
        let mut config = BiemConfig::default();

        register_vault(vault.path(), StorageMode::Global, biem_dir.path(), &mut config).unwrap();
        save_config_to(&config, biem_dir.path()).unwrap();

        let loaded = load_config_from(biem_dir.path()).unwrap();
        let resolved = resolve_vault(vault.path(), &loaded).unwrap();
        assert!(resolved.data_dir.exists());
    }

    #[test]
    fn test_unique_name_dedup() {
        let biem_dir = tempfile::tempdir().unwrap();
        let vault1 = tempfile::Builder::new().prefix("my-vault").tempdir().unwrap();
        let vault2 = tempfile::Builder::new().prefix("my-vault").tempdir().unwrap();
        let mut config = BiemConfig::default();

        let (n1, _) = register_vault(vault1.path(), StorageMode::Global, biem_dir.path(), &mut config).unwrap();
        let (n2, _) = register_vault(vault2.path(), StorageMode::Global, biem_dir.path(), &mut config).unwrap();

        assert_ne!(n1, n2);
        assert_eq!(config.vaults.len(), 2);
    }
}

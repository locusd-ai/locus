# Task: Implement configuration and vault registration ✅

## Goal
Implement the two-tier config model from §8.1 of the system overview: a global `~/.locus/config.toml` that tracks registered vaults, with per-vault state directories (global by default, local opt-in). Add `locus init` and `locus config` CLI commands. Both CLI and daemon should resolve vault state directories from the config.

## Context
Currently, `--data-dir` is passed explicitly or defaults to `~/.locus/` as a flat directory. This doesn't support multiple vaults — all vaults share the same DuckDB/LMDB files. The config system fixes this by giving each vault its own state directory (keyed by a hash of the vault path).

## Steps

### Step 1: Add `toml` dependency
- [x] Add `toml` to workspace dependencies
- [x] Add to `locus-core` (config types live there) and `locus-cli`
- **Validate**: `cargo check`

### Step 2: Config types in locus-core
- [x] Create `crates/locus-core/src/config.rs`
- [x] Define `BiemConfig` struct: map of vault name → `VaultEntry`
- [x] Define `VaultEntry`: path, storage mode (global/local), source_type
- [x] Define `StorageMode` enum: `Global`, `Local`
- [x] `fn config_path() -> PathBuf` — returns `~/.locus/config.toml`
- [x] `fn load_config() -> Result<BiemConfig>` — reads or creates default
- [x] `fn save_config(config: &BiemConfig) -> Result<()>` — writes TOML
- [x] `fn resolve_state_dir(vault_path: &Path, config: &BiemConfig) -> PathBuf` — returns the state directory for a vault
- [x] `fn vault_hash(vault_path: &Path) -> String` — deterministic hash of canonical vault path
- [x] Wire into `locus-core/src/lib.rs`
- **Validate**: `cargo check -p locus-core`

### Step 3: `locus init` command
- [x] Add `Init` variant to CLI `Commands` enum
- [x] `locus init <vault-path> [--local] [--name <name>]`
- [x] Canonicalize vault path, compute hash
- [x] Create state dir (`~/.locus/vaults/<hash>/` or `<vault>/.locus/`)
- [x] Register vault in config.toml
- [x] Run initial bulk index
- [x] Report result
- **Validate**: `locus init tests/fixtures/ --name test-vault`

### Step 4: `locus config` command
- [x] Add `Config` variant to CLI `Commands` enum
- [x] `locus config` — show current config (list registered vaults)
- [x] `locus config --storage local|global` — change storage mode for current vault
- **Validate**: `locus config`

### Step 5: Refactor CLI to resolve state from config
- [x] When no `--data-dir` given, look up vault in config.toml
- [x] `cmd_index` uses resolved state dir
- [x] `cmd_search`, `cmd_inspect`, `cmd_status`, `cmd_filters` use resolved state dir
- [x] Keep `--data-dir` as explicit override
- **Validate**: `locus init tests/fixtures/ && locus search tag:work`

### Step 6: Refactor daemon to resolve state from config
- [x] `biemd <vault>` resolves state dir from config if no `--data-dir`
- [x] Falls back to `~/.locus/` if vault not registered (with warning)
- **Validate**: `biemd tests/fixtures/ --http`

### Step 7: Tests
- [x] Unit tests for config load/save round-trip
- [x] Unit tests for vault_hash determinism
- [x] Unit tests for resolve_state_dir (global + local modes)
- [x] Integration test: init → search flow
- **Validate**: `cargo test`

### Step 8: Commits
- [x] `feat(core): add config types and vault registration`
- [x] `feat(cli): add init and config commands`
- [x] `refactor(cli,daemon): resolve state dir from config`

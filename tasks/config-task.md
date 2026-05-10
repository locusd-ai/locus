# Task: Implement configuration and vault registration

## Goal
Implement the two-tier config model from §8.1 of the system overview: a global `~/.biem/config.toml` that tracks registered vaults, with per-vault state directories (global by default, local opt-in). Add `biem init` and `biem config` CLI commands. Both CLI and daemon should resolve vault state directories from the config.

## Context
Currently, `--data-dir` is passed explicitly or defaults to `~/.biem/` as a flat directory. This doesn't support multiple vaults — all vaults share the same DuckDB/LMDB files. The config system fixes this by giving each vault its own state directory (keyed by a hash of the vault path).

## Steps

### Step 1: Add `toml` dependency
- [ ] Add `toml` to workspace dependencies
- [ ] Add to `biem-core` (config types live there) and `biem-cli`
- **Validate**: `cargo check`

### Step 2: Config types in biem-core
- [ ] Create `crates/biem-core/src/config.rs`
- [ ] Define `BiemConfig` struct: map of vault name → `VaultEntry`
- [ ] Define `VaultEntry`: path, storage mode (global/local), source_type
- [ ] Define `StorageMode` enum: `Global`, `Local`
- [ ] `fn config_path() -> PathBuf` — returns `~/.biem/config.toml`
- [ ] `fn load_config() -> Result<BiemConfig>` — reads or creates default
- [ ] `fn save_config(config: &BiemConfig) -> Result<()>` — writes TOML
- [ ] `fn resolve_state_dir(vault_path: &Path, config: &BiemConfig) -> PathBuf` — returns the state directory for a vault
- [ ] `fn vault_hash(vault_path: &Path) -> String` — deterministic hash of canonical vault path
- [ ] Wire into `biem-core/src/lib.rs`
- **Validate**: `cargo check -p biem-core`

### Step 3: `biem init` command
- [ ] Add `Init` variant to CLI `Commands` enum
- [ ] `biem init <vault-path> [--local] [--name <name>]`
- [ ] Canonicalize vault path, compute hash
- [ ] Create state dir (`~/.biem/vaults/<hash>/` or `<vault>/.biem/`)
- [ ] Register vault in config.toml
- [ ] Run initial bulk index
- [ ] Report result
- **Validate**: `biem init tests/fixtures/ --name test-vault`

### Step 4: `biem config` command
- [ ] Add `Config` variant to CLI `Commands` enum
- [ ] `biem config` — show current config (list registered vaults)
- [ ] `biem config --storage local|global` — change storage mode for current vault
- **Validate**: `biem config`

### Step 5: Refactor CLI to resolve state from config
- [ ] When no `--data-dir` given, look up vault in config.toml
- [ ] `cmd_index` uses resolved state dir
- [ ] `cmd_search`, `cmd_inspect`, `cmd_status`, `cmd_filters` use resolved state dir
- [ ] Keep `--data-dir` as explicit override
- **Validate**: `biem init tests/fixtures/ && biem search tag:work`

### Step 6: Refactor daemon to resolve state from config
- [ ] `biemd <vault>` resolves state dir from config if no `--data-dir`
- [ ] Falls back to `~/.biem/` if vault not registered (with warning)
- **Validate**: `biemd tests/fixtures/ --http`

### Step 7: Tests
- [ ] Unit tests for config load/save round-trip
- [ ] Unit tests for vault_hash determinism
- [ ] Unit tests for resolve_state_dir (global + local modes)
- [ ] Integration test: init → search flow
- **Validate**: `cargo test`

### Step 8: Commits
- [ ] `feat(core): add config types and vault registration`
- [ ] `feat(cli): add init and config commands`
- [ ] `refactor(cli,daemon): resolve state dir from config`

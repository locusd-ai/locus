# Naming Exploration

> Status: **Decided** — renamed from BIEM to Locus/locusd
> Context: "BIEM" (Bit-Indexed External Memory) described the implementation, not the purpose

## What it does (user perspective)

- Indexes everything in your local world (notes, code, future sources)
- Returns pointers, not content — a structural map for AI and humans
- Bitmap pre-filtering eliminates noise before anything reaches an LLM
- Local-first — data never leaves the machine
- Core value: *"I know where everything is, so the AI doesn't have to guess"*

## Candidates

| Name | Rationale | CLI feel | Concerns |
|------|-----------|----------|----------|
| **Locus** | Latin for "place" — returns *where* things are | `locus search --tag work` | Could sound like a bug tracker |
| **Sieve** | Filters noise, lets signal through | `sieve query --tag work` | Might undersell — sounds like just a filter |
| **Topo** | Topology/topograph — structural map of knowledge | `topo search` | Some npm packages exist |
| **Cartograph** | Maps your knowledge terrain | `carto index`, `carto search` | Long, needs alias |
| **Pano** | Panorama — complete view | `pano search` | Might feel lightweight |
| **Karta** | Swedish/Russian for "map" | `karta search` | Pronunciation ambiguity |
| **Omnidex** | Omni + index — says what it is | `omnidex search` | Corporate-sounding |
| **Strix** | Latin owl — sees everything, precise | `strix search` | Obscure |
| **Lens** | Look through it to find things | `lens search` | Very common word |
| **Apex** | All-Purpose EXternal index | `apex search` | Generic, likely taken |

## Decision

**locusd** — domain `locusd.ai` is affordable (vs $500k for `locus.ai`). Keeps the Latin root, follows established precedent (snapd/snap, etcd/etcdctl, containerd/ctr).

- **Daemon / brand**: `locusd` (binary name, repo name, domain)
- **Client CLI**: `locus` (clean root word, user-facing)
- **Config dir**: `~/.locus/`
- **Crate prefixes**: `locus-core`, `locus-parser`, etc.

Precedent: `snapd`/`snap` is the closest parallel — daemon gets the `d`, client is the clean root.

## Rename scope ✅

- [x] Repo renamed from `biem` → `locusd`
- [x] Workspace `Cargo.toml` — crate paths updated
- [x] All 12 crates renamed: `biem-*` → `locus-*` (dirs + `Cargo.toml` + internal deps)
- [x] Binary names: `biem` → `locus`, `biemd` → `locusd`
- [x] Config dir references: `~/.locus/` → `~/.locus/`
- [x] All `use biem_*` imports → `use locus_*`
- [x] Docs updated (`001-system-overview.md`, `002-roadmap.md`, `003-contracts.md`, this file)
- [x] CLAUDE.md updated
- [x] `.github/copilot-instructions.md` updated
- [x] Task files in `tasks/` updated

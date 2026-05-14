# Naming Exploration

> Status: **Open** — parked for follow-up before any public release
> Context: "BIEM" (Bit-Indexed External Memory) describes the implementation, not the purpose

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

## Current recommendation

**Locus** — short, meaningful (returns locations/pointers), unclaimed in Rust/dev-tools, daemon name `locusd` works, scales from personal vault to multi-repo.

Runner-up: **Sieve** — leans into the noise-elimination value prop.

## Rename scope (when ready)

- Repo name
- Crate prefixes (`locus-core`, `locus-parser`, etc.)
- Binary names (`locus`, `locusd`)
- Config dir (`~/.locus/`)
- All docs and README
- Copilot instructions

## Decision

TBD — revisit before first public release.

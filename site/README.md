# site/

The Locus landing page.

| File | What it is |
|---|---|
| `page.html` | The page. Edit this one. |
| `build.py` | Inlines the webfonts and wraps `page.html` into a full document. |
| `index.html` | Generated output — self-contained, servable as-is. Don't hand-edit. |
| `launch-copy.md` | Announcement copy for the places this gets posted. |

```sh
python3 site/build.py     # page.html -> index.html
```

`index.html` is a single file that makes **zero external requests** — fonts are
embedded as base64 `data:` URIs, there is no analytics and no third-party
anything. Drop it on GitHub Pages, or open it straight off disk. The build step
needs network access only to fetch the two typefaces (Source Serif 4 and IBM
Plex Mono, both OFL).

## Keeping it honest

Every figure on the page comes from this repository and is reproducible:

| Claim on the page | Source |
|---|---|
| 568,502 / 587,629 / 79,587 tokens | `docs/benchmarks/token-bench.md` — `scripts/token-bench.py` |
| 80,824 → 20,940 for the database question | same, first row |
| 16µs single key, 19µs AND at 100K docs | `docs/benchmarks/REPORT.md` — in-memory store |
| 4,758µs SQL / 4,075µs graph / 111µs HashSet | same |
| ~25–55ms semantic end-to-end | same, Phase 2/3 §3 |
| ~9,500 files/s parsing, ~66K/s enrichment | same, Phase 2/3 §1–2 |
| The five pointers in the hero demo | real byte ranges into `crates/` at the commit that added this page |

The bitmap timings are in-memory numbers and the page says so, next to the
number, because the on-disk cold read is ~9ms until the daemon warms the page
cache. If a benchmark is re-run and a figure moves, update it here too — the
page is only worth anything while it's true.

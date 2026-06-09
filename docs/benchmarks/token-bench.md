# Token-cost benchmark — locating code: Locus vs grep

Corpus: this repo's `crates/` (72 documents, 1,099 chunks). Reproduce with `python3 scripts/token-bench.py` after indexing. Date: 2026-06-09.

| Question | grep hits → file-dump tokens | grep -C20 tokens | Locus docs → pointer tokens |
|---|---|---|---|
| Where is database access implemented? | 19 files → 80,824 | 63,581 | 20 docs → 20,940 |
| Where is error handling implemented? | 52 files → 152,540 | 192,246 | 6 docs → 8,085 |
| Which modules talk to the network? | 11 files → 46,120 | 28,544 | 11 docs → 11,911 |
| Where is concurrency used? | 8 files → 32,964 | 21,757 | 7 docs → 7,962 |
| Which files implement parsing? | 37 files → 116,164 | 163,912 | 16 docs → 17,821 |
| Where is configuration loaded? | 28 files → 98,907 | 96,057 | 10 docs → 9,926 |
| Which code is async? | 5 files → 25,706 | 14,264 | 3 docs → 2,204 |
| What depends on the vector store? | 3 files → 15,274 | 7,266 | 1 docs → 735 |
| **Total** | **568,502** | **587,629** | **79,587** |

Locus vs file-dump: 7.1x fewer tokens
Locus vs grep -C20: 7.4x fewer tokens

Notes: tokens approximated as bytes/4. Locus column is the full JSON
pointer digest (file paths + function/class labels + byte ranges) —
often enough to answer 'where is X?' outright; when content is needed
the agent reads only the byte ranges it picks. Filters like
complexity:high or visibility:public have no grep equivalent at all.

#!/usr/bin/env python3
"""Token-cost benchmark: locating code with Locus vs grep-and-dump.

Scenario: an AI agent needs to find where something lives in a codebase.
Three strategies, each measured by bytes that would enter the agent's
context window (tokens ~= bytes / 4):

  1. file-dump  — grep -ril <terms>, read every matching file in full
                  (what coding agents do by default)
  2. grep -C20  — grep with 20 lines of context (a disciplined agent)
  3. locus      — bitmap query returning labeled chunk pointers
                  (file + function/class labels + byte ranges)

Run from the repo root after:  locus init crates --type code && locus index crates

  python3 scripts/token-bench.py
"""
from __future__ import annotations

import json
import subprocess
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CORPUS = REPO / "crates"
LOCUS = REPO / "target" / "debug" / "locus"
DATA_DIR = Path.home() / ".locus" / "sources" / "crates"

# (question, grep terms ORed, locus filter expression)
QUESTIONS = [
    ("Where is database access implemented?", ["duckdb", "database"], ["topic:database"]),
    ("Where is error handling implemented?", ["thiserror", "Error"], ["topic:error-handling"]),
    ("Which modules talk to the network?", ["reqwest", "http"], ["topic:networking"]),
    ("Where is concurrency used?", ["tokio", "spawn"], ["topic:concurrency"]),
    ("Which files implement parsing?", ["parse"], ["topic:parsing"]),
    ("Where is configuration loaded?", ["config"], ["topic:config"]),
    ("Which code is async?", ["async fn"], ["async:true"]),
    ("What depends on the vector store?", ["usearch"], ["link:usearch"]),
]


def sh(args: list[str], **kw) -> subprocess.CompletedProcess:
    return subprocess.run(args, capture_output=True, text=True, **kw)


def grep_files(terms: list[str]) -> list[Path]:
    args = ["grep", "-rli", "--include=*.rs"]
    for t in terms:
        args += ["-e", t]
    args.append(str(CORPUS))
    out = sh(args)
    return [Path(p) for p in out.stdout.splitlines() if p]


def grep_context_bytes(terms: list[str]) -> int:
    args = ["grep", "-ri", "-C20", "--include=*.rs"]
    for t in terms:
        args += ["-e", t]
    args.append(str(CORPUS))
    return len(sh(args).stdout.encode())


def locus_digest_bytes(filters: list[str]) -> tuple[int, int, float]:
    t0 = time.perf_counter()
    out = sh([str(LOCUS), "--json", "--data-dir", str(DATA_DIR), "search", *filters])
    elapsed_ms = (time.perf_counter() - t0) * 1000
    data = json.loads(out.stdout)
    docs = len(data.get("matches", []))
    return len(out.stdout.encode()), docs, elapsed_ms


def fmt_tokens(b: int) -> str:
    return f"{b // 4:,}"


def main() -> None:
    rows = []
    tot_dump = tot_ctx = tot_locus = 0
    for question, terms, filters in QUESTIONS:
        t0 = time.perf_counter()
        files = grep_files(terms)
        dump_bytes = sum(f.stat().st_size for f in files)
        grep_ms = (time.perf_counter() - t0) * 1000

        ctx_bytes = grep_context_bytes(terms)
        locus_bytes, docs, locus_ms = locus_digest_bytes(filters)

        tot_dump += dump_bytes
        tot_ctx += ctx_bytes
        tot_locus += locus_bytes
        rows.append((question, len(files), dump_bytes, ctx_bytes, docs, locus_bytes, grep_ms, locus_ms))

    print("| Question | grep hits → file-dump tokens | grep -C20 tokens | Locus docs → pointer tokens |")
    print("|---|---|---|---|")
    for q, nfiles, dump, ctx, docs, locus, gms, lms in rows:
        print(f"| {q} | {nfiles} files → {fmt_tokens(dump)} | {fmt_tokens(ctx)} | {docs} docs → {fmt_tokens(locus)} |")
    print(f"| **Total** | **{fmt_tokens(tot_dump)}** | **{fmt_tokens(tot_ctx)}** | **{fmt_tokens(tot_locus)}** |")
    print()
    print(f"Locus vs file-dump: {tot_dump / tot_locus:.1f}x fewer tokens")
    print(f"Locus vs grep -C20: {tot_ctx / tot_locus:.1f}x fewer tokens")
    print()
    print("Notes: tokens approximated as bytes/4. Locus column is the full JSON")
    print("pointer digest (file paths + function/class labels + byte ranges) —")
    print("often enough to answer 'where is X?' outright; when content is needed")
    print("the agent reads only the byte ranges it picks. Filters like")
    print("complexity:high or visibility:public have no grep equivalent at all.")


if __name__ == "__main__":
    main()

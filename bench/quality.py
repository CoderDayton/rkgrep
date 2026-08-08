#!/usr/bin/env python3
"""Retrieval quality of the rkgrep binary against AST-derived ground truth.

Ground truth comes from Python's own parser, not from any of the systems
being measured, so nothing here scores itself. For each query symbol the
truth is the file that declares it plus the files that reference it.

    python3 bench/quality.py /path/to/repo --prototype /path/to/prototype

`--prototype` is the directory holding `bench_rg.py`, which supplies the
corpus, the ground truth, and the ripgrep baseline.
"""
from __future__ import annotations

import argparse
import json
import os
import statistics
import subprocess
import sys
import time
from collections import defaultdict

DEFAULT_BINARY = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "target", "release", "rkgrep",
)


def run_rkgrep(binary, root, name, max_tokens):
    """Spans rkgrep returns, as (path, tokens) in rank order."""
    proc = subprocess.run(
        [binary, name, root, "--json", "-w", "-g", "*.py",
         "-t", str(max_tokens)],
        capture_output=True, text=True, check=False,
    )
    if not proc.stdout.strip():
        return []
    return [(h["path"], h["tokens"]) for h in json.loads(proc.stdout)]


def evaluate(systems, queries, max_tokens, bench):
    stats = {k: defaultdict(list) for k in systems}
    for q in queries:
        for label, fn in systems.items():
            start = time.perf_counter()
            ranked = [(p, c) for p, c in fn(q["query"]) if c is not None]
            elapsed = time.perf_counter() - start
            kept = bench.under_budget(ranked, max_tokens)
            s = stats[label]
            s["lat"].append(elapsed)
            s["mrr"].append(bench.mrr([p for p, _ in ranked], q["definition"]))
            s["def"].append(1.0 if q["definition"] in kept else 0.0)
            s["cov"].append(
                len(set(kept) & q["neighborhood"]) / len(q["neighborhood"])
            )
    return stats


def report(stats):
    header = (f"{'system':<12}{'MRR(def)':>10}{'def@budget':>12}"
              f"{'cov@budget':>12}{'latency':>11}")
    print(header)
    print("-" * len(header))
    for label, s in stats.items():
        mean = statistics.mean
        print(f"{label:<12}{mean(s['mrr']):>10.3f}{mean(s['def']):>12.1%}"
              f"{mean(s['cov']):>12.1%}{mean(s['lat']) * 1e3:>9.2f}ms")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("root")
    ap.add_argument("--prototype", default="/tmp/test_1786182792",
                    help="directory containing bench_rg.py")
    ap.add_argument("--binary", default=DEFAULT_BINARY)
    ap.add_argument("--queries", type=int, default=120)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--max-tokens", type=int, default=2000)
    ap.add_argument("--context", type=int, default=10)
    args = ap.parse_args()

    sys.path.insert(0, args.prototype)
    import bench_rg as bench

    root = os.path.abspath(args.root)
    files = bench.collect_files(root)
    definitions, references, _ = bench.parse_truth(files)
    queries = bench.build_queries(
        definitions, references, files, args.queries, args.seed
    )
    print(f"repo:    {root}")
    print(f"corpus:  {len(files)} .py files")
    print(f"queries: {len(queries)}\n")

    systems = {
        "rg-span": lambda n: bench.run_rg_span(root, n, files, args.context),
        "rkgrep": lambda n: run_rkgrep(args.binary, root, n, args.max_tokens),
    }
    report(evaluate(systems, queries, args.max_tokens, bench))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

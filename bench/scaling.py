#!/usr/bin/env python3
"""Thread-scaling harness: rkgrep vs ripgrep on a warm page cache.

Reports best-of-N wall time per thread count so scaling efficiency
(t1 / tN) is comparable between the two tools on identical input.
"""
from __future__ import annotations

import argparse
import subprocess
import sys
import time

WARM_PASSES = 2
DEFAULT_REPEATS = 5
DEFAULT_THREADS = (1, 2, 4, 8, 16, 32)


def run(cmd: list[str]) -> float:
    start = time.perf_counter()
    subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return time.perf_counter() - start


def best_of(cmd: list[str], repeats: int) -> float:
    return min(run(cmd) for _ in range(repeats))


def rkgrep_cmd(binary: str, pattern: str, tree: str, threads: int) -> list[str]:
    return [binary, pattern, tree, "--threads", str(threads),
            "--no-ignore", "--hidden"]


def rg_cmd(pattern: str, tree: str, threads: int) -> list[str]:
    return ["rg", "-j", str(threads), "--no-ignore", "--hidden", "-c",
            pattern, tree]


def warm(tree: str) -> None:
    for _ in range(WARM_PASSES):
        subprocess.run(["rg", "-j", "32", "--no-ignore", "--hidden", "-c",
                        "zzz_no_such_token_zzz", tree],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("tree")
    ap.add_argument("--binary", default="target/release/rkgrep")
    ap.add_argument("--patterns", nargs="+", default=["function", "Result", "config"])
    ap.add_argument("--threads", nargs="+", type=int, default=list(DEFAULT_THREADS))
    ap.add_argument("--repeats", type=int, default=DEFAULT_REPEATS)
    args = ap.parse_args()

    print(f"warming page cache on {args.tree} ...", file=sys.stderr)
    warm(args.tree)

    for pattern in args.patterns:
        rk = {t: best_of(rkgrep_cmd(args.binary, pattern, args.tree, t), args.repeats)
              for t in args.threads}
        rg = {t: best_of(rg_cmd(pattern, args.tree, t), args.repeats)
              for t in args.threads}
        base_rk, base_rg = rk[args.threads[0]], rg[args.threads[0]]
        print(f"\npattern={pattern!r}")
        print(f"{'threads':>7} {'rkgrep':>9} {'speedup':>8} {'rg':>9} {'speedup':>8} {'ratio':>7}")
        for t in args.threads:
            print(f"{t:>7} {rk[t]*1e3:>8.1f}ms {base_rk/rk[t]:>7.2f}x "
                  f"{rg[t]*1e3:>8.1f}ms {base_rg/rg[t]:>7.2f}x {rk[t]/rg[t]:>6.2f}x")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

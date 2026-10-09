#!/usr/bin/env python3
"""Rank a `scripts/profile -t` call tree's guest functions by self samples per run.

Only frames under `array_to_wasm_trampoline` count: the rest of the tree is
Wasmtime's runtime and idle threads. Samples are divided by the loop driver's
run count (read from the matching `.prof.stderr`, or given with --runs), since
the two allocators complete different numbers of runs in the same window.

Also prints COVERAGE: the sum of visible self samples over the trampoline's
total. Well below 100% means the tree was pruned (samply-text's -s threshold)
and the ranking is missing functions; re-profile with a lower -s. In analysis
005, sqlite3 showed 25% coverage at the old default.

Usage:
    guest_profile.py PROFILE.txt [PROFILE.txt ...] [--top N] [--runs N]

Given two files (baseline first), also prints a side-by-side delta table.
"""

import argparse
import os
import re
import sys

ROW = re.compile(r"\s*(\d+)\s+[\d.]+%\s+(?:(\d+)\s+[\d.]+%)?\s+(\S.*)$")


def read(path, runs):
    if runs is None:
        err = re.sub(r"\.txt$", "", path) + ".prof.stderr"
        if os.path.exists(err):
            m = re.search(r"loop-driver: (\d+) run", open(err).read())
            runs = int(m.group(1)) if m else None
    if runs is None:
        sys.exit(f"{path}: no run count; pass --runs or keep its .prof.stderr")
    self_samples, trampoline, depth = {}, 0, None
    for line in open(path):
        m = ROW.match(line)
        if not m:
            continue
        name = m.group(3).strip()
        indent = line.index(name)
        if "array_to_wasm_trampoline" in name:
            depth = indent
            trampoline += int(m.group(1))
            continue
        if depth is not None and indent <= depth:
            depth = None
        if depth is not None and m.group(2):
            self_samples[name] = self_samples.get(name, 0) + int(m.group(2))
    return runs, trampoline, self_samples


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("profiles", nargs="+")
    ap.add_argument("--top", type=int, default=12)
    ap.add_argument("--runs", type=int)
    args = ap.parse_args()

    tables = []
    for path in args.profiles:
        runs, tramp, selfs = read(path, args.runs)
        if not tramp:
            sys.exit(f"{path}: no array_to_wasm_trampoline frame; was it run with -t?")
        cover = 100 * sum(selfs.values()) / tramp
        print(f"{os.path.basename(path)}: {runs} runs, guest {tramp / runs:.3f}/run, "
              f"coverage {cover:.0f}%" + ("  <-- pruned tree, lower -s" if cover < 75 else ""))
        for name, s in sorted(selfs.items(), key=lambda x: -x[1])[: args.top]:
            print(f"  {s / runs:7.3f}/run {100 * s / tramp:5.1f}%  {name[:100]}")
        tables.append({n: s / runs for n, s in selfs.items()})

    if len(tables) == 2:
        base, cand = tables
        names = sorted(set(base) | set(cand), key=lambda n: -(base.get(n, 0) + cand.get(n, 0)))
        print(f"\n{'function':60} {'base':>7} {'cand':>7} {'delta':>7}")
        for n in names[: args.top]:
            b, c = base.get(n, 0), cand.get(n, 0)
            print(f"{n[:60]:60} {b:7.3f} {c:7.3f} {c - b:+7.3f}")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Check whether a sightglass cycles result survives robust estimators.

A cycles mean can be dragged by one outlier sample (blake3 in analysis 005: mean
+4.20%, median -0.43%, one regicide sample 16% off). Before believing a result,
compare the mean against the median, a 20%-trimmed mean, and minimum-to-minimum,
and look at each engine's own spread.

Reads the RAW per-sample CSV that `scripts/sightglass` writes (not effect-size
output). Deltas follow the house sign convention: (candidate - baseline) /
baseline * 100, NEGATIVE = candidate faster.

Usage:
    cycles_robustness.py RAW.csv [RAW.csv ...] [--baseline backtracking]
                                 [--candidate regicide]

Prints one row per benchmark and a verdict column:
    robust   every estimator agrees in sign with the mean
    MIXED    some estimator disagrees in sign; do not report as a finding
    NOISY    either engine's CV exceeds 5%; re-run with more samples
"""

import argparse
import csv
import statistics as st
import sys
from collections import defaultdict


def delta(a, c):
    return 100.0 * (c - a) / a


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("csv", nargs="+")
    ap.add_argument("--baseline", default="backtracking")
    ap.add_argument("--candidate", default="regicide")
    args = ap.parse_args()

    samples = defaultdict(lambda: defaultdict(list))
    for path in args.csv:
        with open(path, newline="") as f:
            for row in csv.DictReader(f):
                bench = row["wasm"].split("benchmarks/")[-1]
                samples[bench][row["engine"]].append(int(row["count"]))

    print(f"{'benchmark':45} {'n':>3} {'mean':>7} {'median':>7} {'trim20':>7} "
          f"{'min':>7}  {'CV% base/cand':>13}  verdict")
    for bench, engines in sorted(samples.items()):
        base = sorted(engines.get(args.baseline, []))
        cand = sorted(engines.get(args.candidate, []))
        if len(base) < 3 or len(cand) < 3:
            print(f"{bench:45} too few samples", file=sys.stderr)
            continue
        k = len(base) // 5
        trim = lambda v: st.mean(v[k:len(v) - k]) if k else st.mean(v)
        ests = [
            delta(st.mean(base), st.mean(cand)),
            delta(st.median(base), st.median(cand)),
            delta(trim(base), trim(cand)),
            delta(base[0], cand[0]),
        ]
        cvs = [100 * st.stdev(v) / st.mean(v) for v in (base, cand)]
        if max(cvs) > 5:
            verdict = "NOISY"
        elif all((e > 0) == (ests[0] > 0) for e in ests):
            verdict = "robust"
        else:
            verdict = "MIXED"
        print(f"{bench:45} {len(base):3} " + " ".join(f"{e:+7.2f}" for e in ests)
              + f"  {cvs[0]:6.1f}/{cvs[1]:<6.1f}  {verdict}")


if __name__ == "__main__":
    main()

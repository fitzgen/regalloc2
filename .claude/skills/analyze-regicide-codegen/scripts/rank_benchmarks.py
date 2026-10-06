#!/usr/bin/env python3
"""Rank sightglass benchmarks by how much regicide beat or lost to backtracking.

Picking what to analyze is the first real decision in a codegen analysis, and it
should come from the data rather than from which benchmark name looks
interesting. This reads the machine-readable output of

    sightglass-cli effect-size -f sightglass-data.csv -i csv -o csv > es.csv

ranks every benchmark from worst regression to largest win, and prints the top N
each way as `.wasm` paths ready to paste into a confirmation run.

No statistics are re-derived here. Sightglass computes a Behrens-Fisher
confidence interval on the difference of means and calls a result significant
when

    |a_mean - b_mean| > |half_width_confidence_interval|

which is exactly what this applies, per (benchmark, phase, event) group.

Input columns (the `effect-size -o csv` schema):

    arch,wasm,phase,event,a_engine,a_engine_flags,a_mean,b_engine,b_engine_flags,
    b_mean,significance_level,half_width_confidence_interval

Counts are cycles / instructions, so LOWER IS FASTER. Deltas are reported as
(candidate - baseline) / baseline * 100, i.e. the change in cost the candidate
(regicide, by default) brings: NEGATIVE means FASTER (-12% shaved 12% off the
baseline), POSITIVE means SLOWER (+5% ran 5% more). This is the house
convention for every report file; see SKILL.md, "Sign convention".

Usage:
    rank_benchmarks.py ES.csv [--top N] [--baseline LABEL] [--candidate LABEL]
                              [--phase PHASE] [--event EVENT] [--all]

`scripts/sightglass` names its engines `regicide` and `backtracking`, which are
the defaults for --candidate and --baseline, so usually no labels are needed.
"""

import argparse
import csv
import sys
from collections import defaultdict

REQUIRED_COLUMNS = {
    "wasm",
    "phase",
    "event",
    "a_engine",
    "a_mean",
    "b_engine",
    "b_mean",
    "half_width_confidence_interval",
}

# The suite-wide synthetic benchmark sightglass appends. Reported separately: it
# reaches significance far more readily than any individual benchmark, so it is
# never a candidate for analysis.
SUM_TOTAL = "Sum Total"

# Instantiation is fast and noisy; sightglass itself filters it from comparisons.
SKIP_PHASES = {"instantiation"}

# What `scripts/sightglass` calls the two engines, via its `-n` flags.
DEFAULT_BASELINE = "backtracking"
DEFAULT_CANDIDATE = "regicide"


def strip_to_benchmarks(wasm):
    """Reduce a sightglass module path to the part under `benchmarks/`.

    Sightglass records either a relative path (`./benchmarks/bz2/x.wasm`) or an
    absolute one (`/home/me/sightglass/benchmarks/bz2/x.wasm`), depending on
    how it was invoked.
    """
    name = wasm
    marker = "benchmarks/"
    i = name.rfind("/" + marker)
    if i >= 0:
        return name[i + 1 + len(marker) :]
    for prefix in ("./" + marker, marker):
        if name.startswith(prefix):
            return name[len(prefix) :]
    return name


def bench_label(wasm):
    """Reduce '.../benchmarks/bz2/benchmark.wasm' to 'bz2/benchmark'."""
    name = strip_to_benchmarks(wasm)
    return name[: -len(".wasm")] if name.endswith(".wasm") else name


def wasm_path(wasm):
    """The path to pass back to `scripts/sightglass`, relative to sightglass."""
    if wasm == SUM_TOTAL:
        return wasm
    return "benchmarks/" + strip_to_benchmarks(wasm)


def load(path):
    with open(path, newline="") as f:
        reader = csv.DictReader(f)
        missing = REQUIRED_COLUMNS - set(reader.fieldnames or ())
        if missing:
            sys.exit(
                f"error: {path} is missing the column(s) {sorted(missing)}; expected "
                f"the output of `sightglass-cli effect-size -i csv -o csv`, not the "
                f"raw per-sample CSV that `scripts/sightglass` writes. Convert it "
                f"first:\n"
                f"  cd ~/sightglass && cargo run --release -q -- effect-size \\\n"
                f"      -f {path} -i csv -o csv > es.csv"
            )
        rows = list(reader)
    if not rows:
        sys.exit(f"error: no rows in {path}")
    return rows


def label_of(row, side):
    """Full engine label for one side of a comparison, including its flags."""
    label = row[f"{side}_engine"]
    flags = row.get(f"{side}_engine_flags")
    return f"{label} [{flags}]" if flags else label


def resolve(want, available, what):
    hits = [l for l in available if want in l]
    if not hits:
        sys.exit(
            f"error: no {what} engine matching {want!r} (found: {sorted(available)}). "
            f"Pass --{what} to name one."
        )
    if len(hits) > 1:
        sys.exit(f"error: {what} {want!r} is ambiguous: {hits}")
    return hits[0]


def pick_pair(rows, want_base, want_cand):
    """Choose the (baseline, candidate) engine pair, and whether it is flipped.

    `effect-size` emits each pair once, in one order, which need not be the
    order asked for, so the flip is tracked and the means swapped on read.
    """
    pairs = {(label_of(r, "a"), label_of(r, "b")) for r in rows}
    engines = sorted({e for pair in pairs for e in pair})

    base = resolve(want_base, engines, "baseline") if want_base else None
    cand = resolve(want_cand, engines, "candidate") if want_cand else None

    for a, b in pairs:
        if (base in (None, a)) and (cand in (None, b)):
            return a, b, False
    for a, b in pairs:
        if (base in (None, b)) and (cand in (None, a)):
            return b, a, True

    sys.exit(
        f"error: no comparison of baseline={want_base!r} against "
        f"candidate={want_cand!r}; available pairs: {sorted(pairs)}"
    )


def collect(rows, base, cand, flipped):
    """Group the selected pair's rows by (phase, event) -> [entry, ...]."""
    groups = defaultdict(list)
    for row in rows:
        a, b = label_of(row, "a"), label_of(row, "b")
        if flipped:
            a, b = b, a
        if (a, b) != (base, cand):
            continue
        if row["phase"].lower() in SKIP_PHASES:
            continue

        base_mean = float(row["a_mean" if not flipped else "b_mean"])
        cand_mean = float(row["b_mean" if not flipped else "a_mean"])
        half_width = abs(float(row["half_width_confidence_interval"]))
        diff = cand_mean - base_mean

        groups[(row["phase"], row["event"])].append(
            {
                "bench": bench_label(row["wasm"]),
                "path": wasm_path(row["wasm"]),
                "base": base_mean,
                "cand": cand_mean,
                # Negative => candidate is faster (fewer cycles/instructions).
                "delta_pct": (diff / base_mean * 100.0) if base_mean else 0.0,
                "significant": abs(diff) > half_width,
            }
        )
    return groups


def geomean_ratio(entries):
    """Geometric mean of candidate/baseline, as a percentage change in cost.

    A geometric mean is the right average for ratios, and the suite spans
    benchmarks whose counts differ by four orders of magnitude, so an arithmetic
    mean of the deltas would be dominated by whichever benchmark happens to be
    largest.
    """
    import math

    ratios = [e["cand"] / e["base"] for e in entries if e["base"] > 0 and e["cand"] > 0]
    if not ratios:
        return None
    gm = math.exp(sum(math.log(r) for r in ratios) / len(ratios))
    # Same sign as the per-benchmark delta: negative => candidate faster.
    return (gm - 1.0) * 100.0


def report(group, entries, top, show_all):
    phase, event = group
    print(f"\n=== {phase} :: {event} ===\n")

    total = next((e for e in entries if e["bench"] == SUM_TOTAL), None)
    benches = [e for e in entries if e["bench"] != SUM_TOTAL]
    if not benches:
        print("  (no per-benchmark rows)")
        return

    # Worst regression (most positive) first: that is the end of the list the
    # analysis is for.
    benches.sort(key=lambda e: -e["delta_pct"])

    sig = [e for e in benches if e["significant"]]
    regressed = [e for e in sig if e["delta_pct"] > 0]
    improved = [e for e in sig if e["delta_pct"] < 0]

    gm = geomean_ratio(benches)
    print(f"  benchmarks           {len(benches)}")
    if gm is not None:
        print(f"  geomean              {gm:+.2f}%  (negative => candidate faster)")
    print(f"  significantly faster {len(improved)}")
    print(f"  significantly slower {len(regressed)}")
    print(f"  no difference        {len(benches) - len(sig)}")
    if total:
        mark = "significant" if total["significant"] else "not significant"
        print(f"  Sum Total            {total['delta_pct']:+.2f}%  ({mark})")

    shown = benches if show_all else [e for e in benches if e["significant"]]
    if not shown:
        print(
            "\n  No benchmark showed a significant difference. The run was probably "
            "underpowered\n  (raise `-i`), or this metric cannot resolve the "
            "difference. Pass --all to see\n  every benchmark anyway."
        )
    else:
        heading = "every benchmark" if show_all else "significant differences only"
        print(f"\n  {heading}, worst regression first:\n")
        print(
            f"  {'benchmark':<48} {'baseline':>16} {'candidate':>16} "
            f"{'delta %':>9}  sig"
        )
        for e in shown:
            print(
                f"  {e['bench']:<48} {e['base']:>16,.0f} {e['cand']:>16,.0f} "
                f"{e['delta_pct']:>+9.2f}  {'yes' if e['significant'] else 'no'}"
            )

    # The actionable output: what to analyze, and what to confirm with cycles.
    # Taken from the significant rows when there are any, so that noise does not
    # pick the work; otherwise from the ranking, clearly marked as unconfirmed.
    pool = sig if sig else benches
    qualifier = "" if sig else "  (NOT significant -- ranking only, treat as a guess)"

    worst = [e for e in pool if e["delta_pct"] > 0][:top]
    best = [e for e in reversed(pool) if e["delta_pct"] < 0][:top]

    print(f"\n  top {top} regressed by the candidate{qualifier}:")
    for e in worst or [None]:
        print(f"    {e['bench']:<48} {e['delta_pct']:+.2f}%" if e else "    (none)")
    print(f"\n  top {top} improved by the candidate{qualifier}:")
    for e in best or [None]:
        print(f"    {e['bench']:<48} {e['delta_pct']:+.2f}%" if e else "    (none)")

    picks = [e["path"] for e in worst + best]
    if picks:
        print("\n  confirm these with cycles, at a high sample count:\n")
        print("    ./scripts/sightglass -m cycles -i 30 \\")
        for i, p in enumerate(picks):
            tail = " \\" if i < len(picks) - 1 else ""
            print(f"        {p}{tail}")


def main():
    parser = argparse.ArgumentParser(
        description="Rank sightglass benchmarks by regicide vs backtracking delta.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    parser.add_argument("es_csv", help="output of `effect-size -i csv -o csv`")
    parser.add_argument(
        "--top", type=int, default=5, help="how many each way to pick (default 5)"
    )
    parser.add_argument(
        "--baseline",
        default=DEFAULT_BASELINE,
        help=f"engine to treat as the baseline (default {DEFAULT_BASELINE!r})",
    )
    parser.add_argument(
        "--candidate",
        default=DEFAULT_CANDIDATE,
        help=f"engine to treat as the candidate (default {DEFAULT_CANDIDATE!r})",
    )
    parser.add_argument("--phase", help="only this phase, e.g. Execution")
    parser.add_argument("--event", help="only this event, e.g. cycles")
    parser.add_argument(
        "--all",
        action="store_true",
        help="list every benchmark, not only the significant ones",
    )
    args = parser.parse_args()

    if args.top < 1:
        sys.exit("error: --top wants a positive integer")

    rows = load(args.es_csv)
    base, cand, flipped = pick_pair(rows, args.baseline, args.candidate)
    groups = collect(rows, base, cand, flipped)

    if args.phase:
        groups = {g: e for g, e in groups.items() if g[0].lower() == args.phase.lower()}
    if args.event:
        groups = {g: e for g, e in groups.items() if g[1].lower() == args.event.lower()}
    if not groups:
        sys.exit("error: no rows left after --phase/--event filtering")

    print(f"baseline  = {base}")
    print(f"candidate = {cand}")
    print(
        "\nCounts are cycles/instructions, so LOWER IS FASTER; delta % is\n"
        "(candidate - baseline) / baseline * 100, so NEGATIVE => candidate is FASTER\n"
        "(-12% = 12% less than the baseline) and POSITIVE => SLOWER.\n"
        "'sig' is sightglass's own test: |a_mean - b_mean| > |half-width CI|."
    )

    for group in sorted(groups):
        report(group, groups[group], args.top, args.all)
    print()


if __name__ == "__main__":
    main()

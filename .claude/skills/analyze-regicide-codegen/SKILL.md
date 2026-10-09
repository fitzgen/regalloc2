---
name: analyze-regicide-codegen
description: >
  Analyze the quality of the register allocation and machine code that
  `regicide` produces, compared against regalloc2's backtracking (ion)
  allocator. Use when asked to benchmark regicide against backtracking, to find
  out why regicide is faster or slower on a Wasm benchmark, to investigate
  regicide's spills, reloads, moves, stack frame sizes or register assignments,
  or to produce a codegen-quality report for regicide — driving the work
  empirically with sightglass, `scripts/profile`, `scripts/disassemble`,
  `scripts/log` and `scripts/visualize`. Produces analysis only; it does not
  change regicide.
---

# Analyze regicide's register allocation and codegen quality

Your job is to find out **where regicide's generated code is better or worse
than regalloc2's backtracking allocator's, and why**, and to write that up so
someone else can act on it.

This skill is steps 1 and 2 of a four-step loop:

1. **Run sightglass**, comparing the wall time of Wasm compiled by Wasmtime
   with regicide against Wasmtime with regalloc2's backtracking (ion)
   allocator. ← you do this
2. **Analyze** the results: why is regicide faster or slower on each? ← you do this

Then, after the code-quality report is written, **measure and tabulate compile
time** against ion (§7). That part is a measurement only, with no analysis.
3. Implement optimizations based on that analysis. ← **you do not do this**
4. Repeat.

**Start by reading `references/known-findings.md`** — what previous runs
established, with the evidence and status of each, plus the open questions in
priority order. It will save you rediscovering the main finding, and it records
two wrong turns a previous run actually took. Verify anything load-bearing still
holds; regicide changes. Also read the user's step-3 notes,
`regicide/.claude/skills/optimize-regicide-codegen/references/known-findings.md`
(read-only): it records which of this skill's findings were attempted, which
landed under which commit, and what was dropped and why, so a recommendation
here does not re-propose a probe that already failed.

**You are not responsible for changing regicide.** The user implements step 3
themselves, elsewhere, in other branches and worktrees. Your deliverable is a
report directory. Do not edit anything under `regicide/src/`, `src/`, or
`wasmtime/`; if you believe a change is needed, that is a finding to write down,
not work to do.

## Invariants

- **Analysis only.** No changes to regicide, regalloc2, or Wasmtime source. The
  only files you create or modify live under `reports/codegen-analysis-NNN/`.
- **Write as you go.** Write into the report directory *continuously*, not at
  the end. Others monitor your progress through those files, and an unexpected
  shutdown must not cost the analysis. Do not accumulate findings in temporary
  files or in your context and write them out once at the end.
- **Code quality, not compile speed.** You are analyzing the code that comes
  *out* of the allocator, not how fast the allocator runs. A finding of the form
  "regicide allocates faster" is out of scope; so is "this analysis would make
  regicide slower to run". Compile time is *measured and tabulated* at the end
  (§7), and never analyzed.
- **Wall time decides execution performance.** Rank, file and cost every
  benchmark by sightglass `cycles`. Instructions retired is a diagnostic that
  explains a trade (more moves, fewer stack accesses), never the verdict.
- **Measure, don't intuit.** Every claim that regicide is better or worse on a
  benchmark traces back to a sightglass number or a concrete instruction
  difference you can quote. "This looks like it would be slower" is not a
  finding.
- **Both directions matter.** Wins are findings too — we need to know what we
  are doing well so that future changes preserve it. Do not report only the
  regressions.
- **A top-line win can hide local bad codegen.** A benchmark regicide wins
  overall can still contain a block where it does something dumb, masked by a
  win elsewhere. Look at the code, not only the totals.
- **Each report is standalone.** It cites only what this run measured, plus
  regicide commits and builds it made itself (e.g. `$R/bisect/`). Do not link
  or quote other report directories, and do not point a report at
  `references/known-findings.md`: those are for *you*, to know the history.
  When a comparison with an earlier revision matters, rebuild that revision
  (`scripts/build-at`, §3) and measure it inside this report. Old report
  directories may have been deleted (001-005 were, on 2026-10-08).
- **Git hygiene:** never commit, never touch the index, never push or fetch.
  `reports/` is gitignored anyway (see "Environment notes").

## Inputs

- `N` — how deep the lists go. **Defaults to 5.** `N` controls the
  *benchmark-level* and *summary-level* lists:
  - the top `N` benchmarks regicide most improves,
  - the top `N` benchmarks regicide most regresses,
  - the top `N` things to fix, and the top `N` things to preserve.

  Inside a single benchmark the fan-out is capped lower, independently of `N`,
  or the work explodes combinatorially: **the 3 hottest functions** per
  benchmark, and **the 3 hottest basic blocks** per function. If a benchmark has
  fewer hot functions than that, say so and stop; do not pad with cold code.
- The benchmarks to run — **defaults to `~/sightglass/benchmarks/pca.suite`**,
  which is a one-per-cluster sample of the full suite and is what
  `scripts/sightglass` uses when given nothing.

## What we are optimizing

In roughly most to least important order. The first two come from sightglass,
the rest from reading one region of code.

| Metric | Where from | Notes |
| --- | --- | --- |
| **Cycles** (≈ wall time) | sightglass `-m cycles` | **What we care about, and what ranks the benchmarks** (§2). Noisy, so it needs many samples (`-i 60`, `-i 90` to confirm). |
| **Native instructions executed** | sightglass `-m insts-retired` | A secondary diagnostic for execution, never the ranking or the verdict. Quiet, but blind to the difference between a register-register move (cheap) and a spill or reload that touches memory (expensive). Regicide routinely trades reloads for moves, so it routinely runs **more instructions in less wall time**. |
| **Estimated cycles** | `run` per function; `scripts/estimated_cycles.py` per block | Regicide's own static cost model: register operand, move or remat 1, stack access 5, times the block weight. Definition in `references/analyzing-allocation.md`. Lower is better. Explains a wall-time result; never replaces one. |
| **Stack frame size** | `run.rs` `slots`, disassembly | How many spill slots we created. All else equal, smaller is better. |
| **Moves dynamically executed** | hot block × static moves | A move in the innermost loop costs far more than a move in a cold block. |
| **Moves statically generated** | `run.rs` counts | `moves`, `spills`, `reloads`, `remats`. Lower is better — but moves in `COLD` blocks are often deliberate (known-findings §4). |

**Compile time is reported too, separately and last** (§7): instructions
retired in sightglass's compilation phase, one table, no analysis. It is not
a code-quality metric and never feeds the rankings above.

## Sign convention: negative is faster

Every percentage in every report file is the **change in cost from the
baseline to the variant**:

```
delta % = (variant - baseline) / baseline * 100
```

So **`-12%` means the variant shaved 12% off** the baseline's cycles or
instructions, and **`+5%` means it ran 5% longer** or retired 5% more. The
same holds whatever the pair is — ion vs regicide (baseline ion), regicide
`HEAD^` vs `HEAD` (baseline `HEAD^`), or 001's numbers vs this run's. Say
which side is the baseline whenever it is not ion → regicide.

Static counts follow the same rule: "moves +40%" means more moves.

Sightglass's own human-readable output, the raw `effect-size` CSV, the sibling
skill's `check_no_pessimization.py` and analysis 001's reports do **not** use
this convention (001 and that script use positive = faster).
`scripts/rank_benchmarks.py` does. **Never copy a sign from another tool or an
old report without checking which convention it is in.**

## How to sequence the work

Builds and benchmark runs dominate the wall clock. `scripts/sightglass`,
`scripts/disassemble`/`log`/`make-corpus`/`visualize` all use the `release`
profile; only `scripts/profile` differs, using `profiling` (optimized plus line
tables). So there are **two** profiles to pay for, and the first build of each
takes a few minutes with warm dependencies. **Start the long jobs in the
background and write up the previous step while they run** — never sit idle
watching a build.

**Compile time no longer gates the benchmark runs** (006, regicide
`cf70ddd`): a 10-benchmark `-i 30` cycles run took 1 minute, a 14-benchmark
`-i 5` instruction run ~2.5 minutes including the bench-api build, and every
module analyzed compiled in 3-9 s under regicide, so a 14-benchmark
`-i 60` cycles run is a few minutes. In 002 a single module
(`tinygo-regex`, then 40 minutes per compile) turned a 10-minute run into 2
hours, because sightglass compiles inside every measured process. If a
regicide change brings a slow compile back, it will show up in §1's
disassembly time; then run that module in its own invocation.
Sightglass buffers its CSV to the end, so a half-finished run gives you
nothing.

A sequence that keeps the critical path short:

1. §0 report skeleton (seconds).
2. **Check the host is quiet**: `uptime`, `ps -Ao %cpu,comm -r`. Do this
   *first*, not when a result fails to resolve. `cycles` is the metric that
   decides, and on a loaded machine it will not resolve; see §2. Note what
   you cannot stop (the user's fuzzers and experiments, endpoint security)
   in `misc/measurement-notes.md`.
3. §1 preflight, which pays for the first `release` Wasmtime build (~2-7 min
   after regalloc2/regicide changes). Write §0 while it builds.
4. §2 the **`cycles` ranking** of the whole suite minus its host-bound modules,
   `-i 60` (~3-4 min for 14 benchmarks), then `-i 90` on whatever
   `cycles_robustness.py` calls `NOISY` or `MIXED` and on anything near
   significance. This ranks the benchmarks. Nothing else may run on the host
   while it does.
5. The `insts-retired` run (`-i 5`, ~2 min) on the same benchmarks, as a
   diagnostic: wherever its sign disagrees with cycles, there is a
   moves-for-memory trade to explain in that benchmark's report. It is
   insensitive to load, so it can run while you set up §3.
6. §3 **the whole-suite corpus** (required, ~11 minutes) and §4
   disassemblies for every benchmark you will analyze, in the background.
   Start them once the cycles runs are done. **Do not run them during a
   cycles run**; your own builds are the one load you control. Replay the
   corpus for the suite-wide static counts as soon as it lands.
7. **`scripts/build-at` at the last analyzed regicide revision and at HEAD**
   on every module you will analyze, then `asm_tools.py same` (§5, "Start
   from what changed"). Two builds, ~4 minutes. In 006 this showed in one
   pass that keccak's and siphashx24's hot code was untouched and that
   hashset's driver and tinygo's two hot functions had moved.
8. §4-§5 profile and analyze, writing each report as you finish it. **Once
   cycles, disassemblies, corpora, profiles and the `build-at` comparison are
   all in, the per-benchmark analyses are independent: run them as parallel
   forked subagents**, one per benchmark or pair of related benchmarks (006:
   five forks, ~25-30 minutes each, eight reports). Give each the measured
   numbers, its hot-function list from `guest_profile.py`, the import offset,
   and the standalone rule. Their replays and visualizer builds load the host
   heavily (load ~40 in 006), so **all timing must be finished first**. Use
   the waiting time for §6's skeleton and its static-counts section. Point
   each subagent at `$R/corpus/` and its module's `<module>-` prefix.
9. §6 aggregate report, from the subagents' reports.
10. §7 compile time: one `insts-retired` run of the compilation phase
    (`-i 5`), tabulated at the end of `report.md`. A few minutes, no
    analysis.

### Wall time decides; instruction counts only explain

**The single most expensive mistake available in this workflow is judging
execution performance by instructions retired.** For a register allocator the
difference is usually register-register moves, which is exactly
`insts-retired`'s blind spot: a `mov` and a cache-missing load both count as
1. Regicide's strategy is to do more register moves to avoid spills and
reloads, so **it often runs more instructions in less time**. In 006, three of
the five worst benchmarks by instructions were wall-time wins or parity:

| benchmark (006) | insts-retired | cycles |
| --- | ---: | ---: |
| `sqlite3` | +5.96% | **-3.8% to -4.6%** (win) |
| `shootout-keccak` | +8.75% | **-1.2% to -1.3%** (win) |
| `blake3-scalar` | +5.23% | -0.2% to -0.3% (parity) |
| `hashset` (005) | -3.93% | **+4.2% to +6.2%** (loss; the reverse trade) |

That is why §2 ranks by cycles from the start, rather than screening by
instructions and confirming a subset with cycles, as this skill used to.
Earlier sessions show the same thing:

| benchmark | insts-retired | cycles |
| --- | ---: | ---: |
| `shootout-heapsort` | +15.06% | **+1.15%** (not significant) |
| `shootout-keccak` | +11.00% | **+1.15%** (not significant) |
| `shootout-sieve` | +2.23% | **-7.38% — a significant *win*** (did not reproduce in 002) |
| `shootout-switch` | +237.23% | **+53.39%** (significant) |

Three lessons, in order of how much they cost to learn:

- A benchmark filed as a regression on instructions was a **win** on cycles
  — and five days later, after an unrelated change moved its (identical) hot
  loop across a cache line, it was not. A cycles result is a measurement, not
  an attribution.
- Instruction deltas overstated runtime cost by ~10x on the small benchmarks
  (and on `sqlite3` in 002) — but only ~4–5x on `shootout-switch`, where
  jump-table fan-out multiplied the moves and the regression stayed severe.
- So **"excess moves are cheap" is a conclusion you can reach from an
  unrepresentative sample.** The honest finding is "cheap per occurrence,
  severe where the code shape multiplies them", and you cannot tell which case
  you are in without cycles.

Separate **mechanism** from **magnitude** in every report. The magnitude is
the cycles number; instructions, estimated cycles and move counts are
evidence for the mechanism. Derive each mechanism from the event log rather
than from the code shape: in 001 every magnitude moved, and one mechanism —
`shootout-switch`'s, attributed to region boundaries by analogy with
heapsort — turned out to be wrong (002 found `MAX_MERGE_SPANS`).

## 0. Set up the report directory

Pick the next unused number and create the directory tree **before** you start
measuring, so the first results have somewhere to land:

```bash
cd ~/regalloc2
ls reports/                              # find the highest codegen-analysis-NNN

# `$R` is used throughout this skill for the report directory. Set it in every
# shell you use, since shell state does not persist between commands.
R=reports/codegen-analysis-NNN   # the directory from §0
mkdir -p $R/{misc,sightglass-data,profiles,corpus,viz,improvements,regressions}
```

`NNN` is a unique, incrementing number, so that historical reports are never
clobbered: take the highest existing one and add one.

| Path | Holds |
| --- | --- |
| `report.md` | The aggregate report (§6). Create it early as a skeleton and fill it in. |
| `sightglass-data/` | Raw sightglass CSVs, so others can re-analyze without re-measuring. |
| `profiles/` | `scripts/profile` output you worked from. |
| `corpus/` | The whole-suite `scripts/make-corpus` output, all 17 modules (§3, required), so allocations can be replayed cheaply. |
| `viz/<benchmark>/` | `scripts/visualize` HTML pages, one per allocation, which the per-benchmark reports link to (§5). |
| `improvements/` | One `.md` per most-improved benchmark (§5). |
| `regressions/` | One `.md` per most-regressed benchmark (§5). |
| `misc/` | Everything else worth keeping: environment facts, tool gotchas, hard-won insights, things that should be added to this skill. Use it freely. |

Write the date and time (`YYYY-MM-DD, HH:MM`) at the top of every report file.
Get it from `date '+%Y-%m-%d, %H:%M'` rather than guessing.

## 1. Preflight: prove the two allocators actually differ

**Do this before a long benchmark run.** If the Wasmtime build silently ignores
`-O regalloc-algorithm=regicide`, every benchmark comes back "no difference in
performance" and the whole analysis is a measurement of nothing against itself.
That failure is indistinguishable from "the two allocators perform identically",
so rule it out up front by confirming the flag changes the generated code.

Use `splay/splay.wasm` for this: at 3.4KB it is by far the smallest module in
`pca.suite`, and the check is about whether the flag works at all, not about
which module it runs on. A bigger module turns a 10-second check into a
multi-minute one for no extra information.

```bash
cd ~/regalloc2
# Copied to /tmp first, one per allocator: `disassemble` writes its `.cwasm`
# *beside the module*, so running both in place would litter the sightglass
# checkout and have the two runs overwrite each other's artifact.
cp ~/sightglass/benchmarks/splay/splay.wasm /tmp/pf-regicide.wasm
cp ~/sightglass/benchmarks/splay/splay.wasm /tmp/pf-backtracking.wasm

./scripts/disassemble -O regalloc-algorithm=regicide \
    /tmp/pf-regicide.wasm -- --funcs all > /tmp/pf-regicide.asm
./scripts/disassemble -O regalloc-algorithm=backtracking \
    /tmp/pf-backtracking.wasm -- --funcs all > /tmp/pf-backtracking.asm

diff /tmp/pf-backtracking.asm /tmp/pf-regicide.asm | head -40
```

A non-empty diff means you are comparing two allocators. An empty one means stop
and work out why before measuring anything. Record the result in
`$R/misc/preflight.md`.

For reference, on 2026-09-30 this produced 2,398 lines for backtracking against
2,444 for regicide, differing in register choices from the first block onwards —
so a diff of a few thousand lines here is normal and expected.

**This is also where you pay for the first Wasmtime `release` build** — a few
minutes when dependencies are warm, much longer from cold. Start it, then set up
the report skeleton while it runs.

## 2. Run the benchmarks

`scripts/sightglass` does nearly all of this: it builds `wasmtime-bench-api`
once and hands the *same* dylib to sightglass twice, once per allocator, so a
run compares allocators rather than builds. Full flag reference in
`references/tools.md`.

**Rank by `cycles` (wall time), on every benchmark, with enough samples to
resolve it.** Since every module compiles in seconds, there is no reason to
screen with a cheaper metric first: the whole 14-benchmark suite at `-i 60` is
a few minutes. `insts-retired` runs too, but only as a diagnostic. Full policy
and the reasoning in `references/measurement.md`.

```bash
cd ~/regalloc2
R=reports/codegen-analysis-NNN   # the directory from §0
S=.claude/skills/analyze-regicide-codegen/scripts

# The suite minus its host-bound modules. `splay`, `shootout-memmove` and
# `cm-online-stats` are ~98% of pca.suite's cost and have no
# register-allocated hot code (§4).
grep -v '^[[:space:]]*#' ~/sightglass/benchmarks/pca.suite | grep -v '^[[:space:]]*$' \
    | grep -vE 'splay|memmove|cm-online' > $R/sightglass-data/screen.suite
BENCHES=$(sed 's|^|benchmarks/|' $R/sightglass-data/screen.suite)

# 1. The ranking: cycles, every benchmark, -i 60 (20 processes x 3).
#    `--pin` binds each process to one core (advisory on Apple Silicon).
#    Run on a quiet host, with none of your own jobs alongside.
./scripts/sightglass -m cycles -i 60 $BENCHES -- --pin \
    > $R/sightglass-data/cycles.csv
python3 $S/cycles_robustness.py $R/sightglass-data/cycles.csv \
    > $R/sightglass-data/cycles-robustness.txt

# 2. -i 90 on whatever cycles_robustness.py calls NOISY or MIXED, on anything
#    not significant whose estimators all agree in sign (it may resolve with
#    more samples), and on anything you will build a finding on.
./scripts/sightglass -m cycles -i 90 <those> -- --pin \
    > $R/sightglass-data/cycles-confirm.csv
python3 $S/cycles_robustness.py $R/sightglass-data/cycles-confirm.csv \
    > $R/sightglass-data/cycles-confirm-robustness.txt

# 3. The diagnostic: instructions retired on the same benchmarks. Quiet, so
#    -i 5 is plenty, and insensitive to host load.
./scripts/sightglass -i 5 $BENCHES > $R/sightglass-data/insts-retired.csv
```

`cycles_robustness.py` prints mean, median, 20%-trimmed mean and
minimum-to-minimum per benchmark with a verdict: `robust` when all four
agree in sign, `MIXED` when they do not, `NOISY` when either engine's CV
exceeds 5%. In 005 it flagged blake3's +4.20% "regression" (one 16%
outlier; median -0.43%) on sight.

These are double the sample counts this skill used to use (`-i 30` and
`-i 45` for cycles). At `-i 30` in 006, rust-protobuf, blake3 and hash3
stayed unresolved or flipped between runs, and hashset's size was
uncertain by a factor of three. If a benchmark is still `NOISY` at `-i 90`,
report its sign with the spread of the estimators, and do not quote one
number.

If a compile-time regression ever makes a module slow to compile again, give
that module its own sightglass invocation so it does not hold up the rest
(sightglass compiles inside every measured process).

Sightglass buffers its CSV until the end, so a long run gives you nothing
until it finishes; there is no partial output to poll.

Then rank the benchmarks **by cycles** and pick the extremes. Pool the `-i 60`
and `-i 90` samples for a benchmark that has both: `effect-size` takes
several `-f` files.

```bash
cd ~/sightglass
cargo run --release -q -- effect-size \
    -f ~/regalloc2/$R/sightglass-data/cycles.csv \
    -f ~/regalloc2/$R/sightglass-data/cycles-confirm.csv -i csv -o csv \
    > ~/regalloc2/$R/sightglass-data/cycles-effect-size.csv
cargo run --release -q -- effect-size \
    -f ~/regalloc2/$R/sightglass-data/insts-retired.csv -i csv -o csv \
    > ~/regalloc2/$R/sightglass-data/insts-retired-effect-size.csv

python3 ~/regalloc2/$S/rank_benchmarks.py --all --top 5 \
    ~/regalloc2/$R/sightglass-data/cycles-effect-size.csv \
    > ~/regalloc2/$R/sightglass-data/ranking.txt
python3 ~/regalloc2/$S/rank_benchmarks.py --all \
    ~/regalloc2/$R/sightglass-data/insts-retired-effect-size.csv \
    > ~/regalloc2/$R/sightglass-data/insts-retired-ranking.txt
```

`rank_benchmarks.py` prints every benchmark ranked worst-first with its delta
and whether sightglass calls it significant, then the top-`N` each way. **The
top `N` improved and regressed come from the cycles ranking.** A benchmark
significant only on instructions is not a pick; it may still be worth a
line in the report if its instruction delta is large, as a moves-for-memory
trade that came out even.

Check the pooled samples with `cycles_robustness.py` too (it takes several
CSVs). A benchmark is a regression or a win when sightglass calls it
significant **and** the verdict is `robust`.

Read `references/measurement.md` before interpreting any of this. The four
things that most often go wrong:

- **Direction.** These are counts, so **lower is faster**, and a report's
  deltas are **negative when regicide is faster** (see "Sign convention").
  Check which engine is `a_engine` in raw `effect-size` output before calling
  anything a win.
- **Significance.** Sightglass prints `Δ` and a comparison sentence *only* when
  the difference is significant; otherwise "No difference in performance". The
  synthetic `Sum Total` row reaches significance far more readily than any
  individual benchmark.
- **Cycles may not resolve at `-i 60`.** Then run `-i 90`. If it still does
  not, say plainly that the benchmark's wall time is inconclusive and give
  the estimators' spread. **Do not fall back to ranking it by instructions.**
  Check robustness (median, trimmed mean, minimum-to-minimum, within-engine
  spread) before believing a result that *is* significant.
- **Severity written from instruction counts.** For a register allocator the
  difference is usually register-register moves, which `insts-retired`
  cannot distinguish from a cache miss. Every severity sentence in every
  report comes from cycles.

When `insts-retired` and `cycles` disagree in direction, regicide traded
moves for memory traffic, or the reverse. **This is the normal case, not an
anomaly**: in 006, sqlite3, keccak and blake3 ran more instructions in less
or equal time (fewer stack accesses, more moves), and in 005 hashset ran
fewer instructions in more time (reloads after calls). Explain the trade in
that benchmark's report, with the stack-access and move counts behind it.

**Rule out layout before attributing a cycles delta to the allocator.** In
002, sieve's disagreement sat on an *identical* loop and was probably code
layout. The cheap control is to re-run cycles with
`-C cranelift-log2_min_function_alignment=6` on both engines, which moves every
hot loop within its cache line without changing allocation:

```bash
cd ~/sightglass
E=~/regalloc2/wasmtime/target/release/libwasmtime_bench_api.dylib
cargo run --release -q -- benchmark \
  -e $E --engine-flags "-O regalloc-algorithm=regicide -C cranelift-log2_min_function_alignment=6" -n regicide \
  -e $E --engine-flags "-O regalloc-algorithm=backtracking -C cranelift-log2_min_function_alignment=6" -n backtracking \
  --benchmark-phase execution -m cycles --processes 20 --iterations-per-process 3 --pin \
  --raw -f csv benchmarks/hashset/benchmark.wasm > ~/regalloc2/$R/sightglass-data/align6-cycles.csv
```

If the delta survives (hashset's did: +6.19%), it is not layout.

## 3. Build the whole-suite corpus — required

Capture every regicide allocation input in the suite into **`$R/corpus/`**,
so that replaying an allocation costs one allocation (about a second) instead
of a Wasmtime run, and so the report has suite-wide static counts. **Every
run builds it**, all 17 modules of `pca.suite`: the three left out of the
execution runs too. It is the only corpus; there are no per-module corpus
directories.

Start it as soon as the cycles runs (§2) are finished. It does not depend on
which benchmarks you will analyze, and it must not overlap a timing run. Run
it in the background and get on with §4 while it works:

```bash
cd ~/regalloc2
SUITE=~/sightglass/benchmarks/pca.suite

# A `.suite` file is one benchmark path per line, relative to `benchmarks/`,
# with `#` comments and blank lines. Read with a `while` loop rather than
# `mapfile`, which this host's bash 3.2 does not have.
WASMS=()
while IFS= read -r line; do
    WASMS+=("$HOME/sightglass/benchmarks/$line")
done < <(grep -v '^[[:space:]]*#' "$SUITE" | grep -v '^[[:space:]]*$')
echo "${#WASMS[@]} modules"          # pca.suite: 17

./scripts/make-corpus -c $R/corpus "${WASMS[@]}" 2> $R/misc/make-corpus.log
echo "exit $?" >> $R/misc/make-corpus.log
```

One invocation, so a single `regicide-dump` Wasmtime build serves every
module. On 2026-10-09 (regicide `cf70ddd`) it took **11 minutes** and wrote
**6,416 inputs**, sqlite3 alone being 3,472 of them. That is faster than the
eight separate per-module builds 006 made (~13 minutes in total), because the
Wasmtime build is paid once. `make-corpus` exits nonzero if any module
captured nothing; check `make-corpus.log` for that rather than assuming it
finished cleanly.

Each capture lands as `$R/corpus/<module>-<counter>[-<function>].postcard`
(postcard-encoded, not JSON). `<module>` is the `.wasm`'s base name, except for
sightglass's many identically-named `benchmark.wasm` modules, where it is the
*directory* name instead: `blake3-scalar/benchmark.wasm` captures as
`blake3-scalar-0000-...` and does not collide with `hashset`'s. A capture is
byte-identical to what a single-module `make-corpus` of the same module
writes (checked on four hot inputs), so nothing is lost by building them all
together.

The input for a function you care about is found by module and index, since
every `pca.suite` capture is named `u0-N` (§4, "Corpus `u0-N` is not
disassembly `function[N]`"), or by name when the capture carries one:

```bash
ls $R/corpus | grep '^sqlite3-.*-u0-181\.'
ls $R/corpus | grep -i <function-name>
```

Replay one, with regicide's own event log, to see the decisions behind an
allocation:

```bash
cd ~/regalloc2/regicide
RUST_LOG=trace cargo run --all-features --example run -- \
    ../$R/corpus/sqlite3-0226-u0-181.postcard
```

`run` takes files as well as directories, so one module's inputs are a glob:
`run -q $R/corpus/sqlite3-[0-9]*.postcard`. The `[0-9]` keeps
`shootout-switch` from matching `shootout-sieve`'s inputs and so on.

### Suite-wide static counts — also required

Replay the whole corpus once, quietly, for the per-function counts of every
function in every benchmark, and per module for the report's table:

```bash
cd ~/regalloc2/regicide
cargo build --release --all-features --example run
RUN=target/release/examples/run
$RUN -q ../$R/corpus > ../$R/misc/corpus-counts.txt
grep -A4 '=== totals' ../$R/misc/corpus-counts.txt
for m in $(ls ../$R/corpus | sed -E 's/-[0-9]{4}-.*//' | sort -u); do
    echo "== $m"; $RUN -q ../$R/corpus/$m-[0-9]*.postcard | grep -A4 '=== totals'
done > ../$R/misc/corpus-counts-by-module.txt
```

A few minutes. Those totals (`moves`, `spills`, `reloads`, `remats`,
`critical-edges`, `slots`, `estimated-cycles`, `coalesce-out-of-fuel`) are
suite-wide static allocation quality, and they are the numbers step 3 will
diff against after a change. Quote them in the aggregate report's "Static
allocation quality" section, suite total plus one row per module. 2026-10-09,
`cf70ddd`: moves 210,177 (8,054 cold), spills 23,656, reloads 71,911, remats
21,862, slots 8,658, `coalesce-out-of-fuel` 0. **Caveat:** since Cranelift reports `COLD` blocks, some
moves are *deliberately* placed in cold blocks, so a rise in total `moves` can
be a correct decision; check where the moves landed before calling it a
regression (`references/known-findings.md` §4).

### Before/after a regicide change: replay an old corpus through two builds

**The cleanest attribution available.** A previous analysis's corpus is a
fixed set of allocator inputs. Replaying it through regicide built at two
revisions isolates exactly what the commits between them did, with no
Wasmtime or Cranelift change in the loop. Build the old revision from a
`git archive` so no repository state is touched:

```bash
cd ~/regalloc2/regicide
rm -rf /tmp/regicide-OLD && mkdir /tmp/regicide-OLD
git archive <old-rev> | tar -x -C /tmp/regicide-OLD
(cd /tmp/regicide-OLD && CARGO_TARGET_DIR=/tmp/regicide-OLD/target \
    cargo build --release --all-features --example run)
cargo build --release --all-features --example run

OLD=/tmp/regicide-OLD/target/release/examples/run
NEW=~/regalloc2/regicide/target/release/examples/run
# The totals block is several lines; `tail -1` alone shows only its last,
# `coalesce-out-of-fuel`.
$OLD -q ../reports/codegen-analysis-NNN/corpus | grep -A4 '=== totals'
$NEW -q ../reports/codegen-analysis-NNN/corpus | grep -A4 '=== totals'
```

Revisions `7d1217e`..`f12e432` (at least) do not build with the `run`
example's features: delete the line `self.split_span_vals.clear();` from
`src/region_allocation.rs` in the scratch copy first.

**The corpus format is not stable across revisions.** A `run` built at
`288da68` rejects corpora captured at `cf70ddd` ("not a valid postcard input:
Serde Deserialization Error"); `1db4308` changed the input. So an *old* `run`
generally cannot replay a *new* corpus. An old corpus with a new `run` may or
may not work. When the replay fails, fall back to the runtime comparison
below (`scripts/build-at`), which needs no corpus at all.

**Validate first:** the OLD build on the old corpus should reproduce the old
report's published counts exactly. In 002 it did, to the move, which is what
made every delta trustworthy.

The same trick answers "what would this change do?" for a one-line experiment
— patch the scratch copy, never `regicide/src/`. In 002, lifting
`MAX_MERGE_SPANS` and changing a split tie-break this way located two
regressions in under an hour. Run `--check` on anything you patch. Such a
probe bounds how much of a regression a mechanism explains; it is not a fix
proposal.

The converse — **same regicide, old vs new corpus** — isolates what changed in
Cranelift's inputs (e.g. new remat or block-frequency information).

**Bisect one function across several revisions** the same way: build `run`
at each revision into `/tmp/regicide-<rev>` and replay one input through
all of them. In 005 that located, in minutes, the commit that tripled
tinygo `countMatches`' stack-to-stack copies (`7d1217e`: 710 → 2,176) and
cut sqlite3's VDBE reloads 37%.

### Before/after a regicide change, at runtime: `scripts/build-at`

A corpus replay cannot tell you what a change cost in cycles, and it cannot
help when **the hot functions did not change** but the benchmark's time did.
In 005, hashset's four hot functions were byte-identical to 003's and its
cycles still moved from -2% to +5%; the cause was a 1.5%-self-time driver
function whose extra reloads sat on the path of 11,000 calls. Only a
runtime bisect found it.

```bash
cd ~/regalloc2
# Each call builds a scratch Wasmtime under /tmp/build-at with regicide taken
# from that revision (everything else is this checkout's HEAD plus Wasmtime's
# uncommitted diff), then compiles the modules. ~3.5 min first, ~15 s after.
for rev in e9b676b 36e91a7 7d1217e HEAD; do
    ./scripts/build-at $rev $R/bisect ~/sightglass/benchmarks/hashset/benchmark.wasm
done

# Interleaved timing of the bench.start..bench.end region, baseline first.
./scripts/time-cwasm -n 80 -d ~/sightglass/benchmarks/hashset \
    $R/profiles/asm/hashset-backtracking.cwasm $R/bisect/hashset-*.cwasm
```

**Validate first**, as with replays: build at the revision an old report
measured and confirm its `.cwasm` disassembles identically to that report's
(005: `objdump | md5` matched 003's exactly). `asm_tools.py same OLD NEW`
then lists which functions each step changed, which is what localizes the
cause. `time-cwasm` needs ~80 rounds to resolve ~2% steps on a loaded host,
and like any timing it must not share the machine with your own builds.

**An old report's `.cwasm`s are a free baseline, if the report still
exists.** `disassemble` leaves them in `$R/profiles/asm/`, and they still
load in today's Wasmtime; timing 003's beside today's reproduced both
reports' cycles results in one session. If the old report is gone, `build-at`
at its regicide revision regenerates them (validated in 006: `build-at HEAD`
reproduced the measured build function-for-function).

## 4. Find the hot code

For each benchmark you are writing up, profile it and work down from the hottest
code. Cold code is not worth analyzing: a perfect fix to a block that never
executes is worth nothing, and a block you never even sampled is worth less than
that.

### First: check the hot code is actually Wasm

**Most of `pca.suite`'s execution cost is not register-allocated code at all.**
Check this per benchmark before analyzing it, because a benchmark whose time is
spent in Wasmtime's host runtime cannot tell you anything about either
allocator. Measured on `pca.suite`:

| benchmark | share of suite cost | where its guest time goes |
| --- | ---: | --- |
| `splay` | **79.8%** | ~96% inside `wasmtime_builtin_gc_alloc_raw` (host GC) |
| `shootout-memmove` | 10.1% | ~99% inside `wasmtime_builtin_memory_copy` |
| `cm-online-stats` | 7.9% | a multi-component composition; `scripts/profile` cannot run it |

Those three are **97.8% of the suite's instruction count**, and the 001 screen
confirmed the prediction: they came back at +0.00%, +0.26% and +0.37%, i.e.
no difference, because there is nothing there for an allocator to affect.

Two consequences:

- **Never quote `Sum Total`.** On this suite it read +0.84% against a
  per-benchmark geomean of +20.10% — understating the regression ~24x, because
  it is dominated by benchmarks that cannot move. Rank and report
  per-benchmark, treating each as one observation regardless of absolute size.
- **If a top-`N` pick turns out to be host-bound, say so and move down the
  list.** Producing a block analysis of Wasmtime's GC is wasted work.

The check is one look at the call tree under `array_call_native`: if the
inclusive time collapses into a `wasmtime_builtin_*` frame, the benchmark is
host-bound.

**Components are profiled through their core module.** `scripts/profile`
detects a component and extracts its main core module with
`wasm-tools component unbundle`. That works when the component wraps a single
WASIp1 module and the `wasi_snapshot_preview1` adapter, as `kotlin-richards`
does (005 profiled it this way for the first time; its stdout matches the
benchmark's expected output, and its code is identical to the component's).
`cm-online-stats` is a composition of several components and is refused, with
a message saying so. `scripts/disassemble` handles components directly; their
functions appear as `wasm[0]::`, `wasm[1]::`, … per core module.

```bash
cd ~/regalloc2
./scripts/profile -t --loop 10 ~/sightglass/benchmarks/bz2/benchmark.wasm \
    > $R/profiles/bz2-regicide.txt 2> $R/profiles/bz2-regicide.prof.stderr
./scripts/profile -t --loop 10 --regalloc backtracking \
    ~/sightglass/benchmarks/bz2/benchmark.wasm \
    > $R/profiles/bz2-backtracking.txt 2> $R/profiles/bz2-backtracking.prof.stderr

python3 .claude/skills/analyze-regicide-codegen/scripts/guest_profile.py \
    $R/profiles/bz2-backtracking.txt $R/profiles/bz2-regicide.txt
```

Keep the `.prof.stderr`: it holds the loop driver's run count, which
`guest_profile.py` divides by. It ranks the frames under
`array_to_wasm_trampoline` by **self samples per run** and prints a
side-by-side delta.

Read the **call tree** (`-t`), not the flat view: the flat view is topped by
Wasmtime's idle threads parked in the async runtime, which are asleep rather
than working. The guest's frames sit under `array_call_native`.

**Check the coverage line.** `guest_profile.py` reports what fraction of the
guest's samples the tree's visible self samples account for. `samply-text`
prunes subtrees under its `-s` threshold (default 0.1%), and for sqlite3,
whose time is spread over many small functions, a pruned tree showed **24%**
coverage and a ranking missing its second-hottest function. `scripts/profile`
now passes `-s 0.005 -n 100000` unless told otherwise; if coverage is still
below ~75%, lower `-s` further.

**`scripts/profile` recompiles the module every time**, with the `profiling`
profile. That costs seconds today; if a module ever takes regicide minutes to
compile again, profile the `.cwasm` that `scripts/disassemble` already wrote
instead (`references/tools.md`, "Profiling an existing `.cwasm`").

**A small self-time share does not make a function cold.** hashset's
`benchmark()` driver had 1.2-1.5% of self time, and its extra reloads cost the
benchmark ~5 pp of cycles, because each sat right after one of its 11,190
calls, on the critical path of the next. When a benchmark's cycles moved but
its hot functions did not, look at the functions that *did* change (§3,
`asm_tools.py same`), however cold, and bisect with `scripts/build-at`.

**There is no block-level heat for JIT code on macOS.** `wasmtime hot-blocks`
is Linux-only, and the per-address frames samply records for perfmap-symbolized
JIT code are offset from the real addresses by a constant that differs per
function and per run (002, `misc/no-block-level-heat.md`). Do not label a
block "hottest" from them. Instead, rank functions by samples, then localize
inside a function by **loop structure**: back-edges, loop bodies, and each
loop's moves and stack accesses, innermost first. Say "innermost loop", not
"hottest block", in the report.

Profile **both** allocators. Which function is hot can itself differ between
them, and that is a finding.

Then get the disassembly of the hot function under each allocator and diff them.
Give each allocator **its own copy of the module**: `disassemble` writes the
`.cwasm` beside the input, so running both in place would have them overwrite
each other's artifact inside the sightglass checkout.

```bash
cd ~/regalloc2
mkdir -p $R/profiles/asm && cd $R/profiles/asm
cp ~/sightglass/benchmarks/bz2/benchmark.wasm bz2-regicide.wasm
cp ~/sightglass/benchmarks/bz2/benchmark.wasm bz2-backtracking.wasm
cd ~/regalloc2

./scripts/disassemble -O regalloc-algorithm=regicide \
    $R/profiles/asm/bz2-regicide.wasm -- --funcs all --addresses \
    > $R/profiles/bz2-regicide.asm
./scripts/disassemble -O regalloc-algorithm=backtracking \
    $R/profiles/asm/bz2-backtracking.wasm -- --funcs all --addresses \
    > $R/profiles/bz2-backtracking.asm

diff -u $R/profiles/bz2-backtracking.asm $R/profiles/bz2-regicide.asm \
    > $R/profiles/bz2.asm.diff
```

Use `--filter <function>` instead of `--funcs all` once you know which function
you want — a whole benchmark's disassembly is large and mostly irrelevant.

Beware that **function numbering and ordering can differ between the two
disassemblies**; match functions by name, not by index or offset.

`asm_tools.py` (in this skill's `scripts/`) does the mechanical parts on a
`--funcs all --addresses` disassembly: `mix` (the per-function table of
`references/analyzing-allocation.md`), `extract N [--strip]`, `same OLD NEW`
(which functions really changed between two builds), `loops` (innermost
first, with line offset and a heap-offset fingerprint) and `slot2slot`.

**Match loops between allocators by their heap offsets**, not by size or
order. When regicide's version of a loop is 3-5x longer (exit stubs), the set
of `[xN, #off]` offsets its body touches still identifies it: in 005 that
matched all four of tinygo `countMatches`' innermost loops to ion's.

**Corpus `u0-N` is not disassembly `function[N]`.** The corpus counts defined
functions only; the disassembly's index includes imported functions. For a
module with `k` function imports, `function[M]` is `u0-(M-k)` (tinygo-regex:
`k` = 15; sqlite3: `k` = 10). Count imports with
`grep '(import' module.wat | grep -c '(func'`, then confirm the match by moves
(the `run.rs` count and the disassembly's reg-reg `mov` count agree to within
one or two). The whole-file
`diff` is for orientation; the per-function `--filter` output is what you
actually analyze.

## 5. Analyze, and write one report per benchmark

For each of the top `N` most-improved and top `N` most-regressed benchmarks,
write `$R/improvements/<benchmark>.md` or `$R/regressions/<benchmark>.md`.

The full method — what to look for in a block, the "why" ladder from
instructions down to regicide's design choices, how to count estimated cycles, and
which log targets answer which question — is in
`references/analyzing-allocation.md`. **Read it before analyzing your first
block**, especially §"Counting, mechanically", which has the arm64 spill-counting
trap and the per-function mix table that localizes a regression in one pass.

`references/known-findings.md` has what previous runs established about
regicide's design, so that a level-3 explanation starts from the known
mechanisms rather than deriving them again. The report layout and a filled-in example are in
`references/report-templates.md`.

In short, each per-benchmark report contains:

- the date and time, and the benchmark's speedup or slowdown (with the metric
  and whether it was significant);
- the `scripts/log` command that reproduces the compile with logging;
- the `scripts/visualize` command that renders the HTML visualizations for this
  benchmark's hot functions, into `$R/viz/<benchmark>/`, **plus a table saying
  which generated `.html` file corresponds to which hot function**;
- the 3 hottest functions, each with its sample share, the `run.rs` command for
  its corpus input, the `scripts/disassemble` command for its disassembly, and a
  link to its visualization page;
- for each function, its 3 hottest basic blocks, each with the disassembly under
  both allocators and an analysis of what the allocator did differently.

**`-f` matches the capture's name, which for most modules is the corpus
placeholder `u0-N`, not the function name** — even for tinygo-regex, whose
*disassembly* is name-sectioned. A `-f 'countMatches'` renders nothing. Work
out `u0-N` from the import offset (§4) and filter on that:
`-f '^u0-(99|223|229)$'`. Allow for sqlite3: one hot VDBE page was 105 MB
and the run took ~10 minutes, mostly scanning the other 3,469 captures.

The visualization filenames are **not predictable**: a dump is named
`<pid>-<counter>[-<function>].postcard`, so the page is
`<pid>-<counter>[-<function>].html` with the *compiling process's* pid in it.
Take the mapping from `visualize`'s stderr, which echoes every path it writes,
or from `ls $R/viz/<benchmark>/`, and write it into the report while you have
it — a reader cannot reconstruct it from the command alone. A function that
failed to render leaves a `.log` and no `.html`; say that instead of linking a
page that does not exist.

```bash
cd ~/regalloc2
./scripts/visualize -f 'hot_fn_1|hot_fn_2|hot_fn_3' \
    ~/sightglass/benchmarks/bz2/benchmark.wasm $R/viz/bz2/
ls $R/viz/bz2/
```

`-f` is a case-insensitive POSIX extended regex, so one `|`-joined invocation
covers all three hot functions and skips the rest of the module — which matters,
since a benchmark is hundreds of functions and each page is a separate compile's
worth of rendering.

### Start from what changed since the last analyzed revision

Before any per-block work, ask of each benchmark: **did its hot code change
since the regicide revision the last analysis measured?**
(`references/known-findings.md` names it.) Rebuild both revisions here, so
the comparison lives in this report and does not depend on an old report
directory:

```bash
cd ~/regalloc2
S=.claude/skills/analyze-regicide-codegen/scripts
W=~/sightglass/benchmarks
for rev in <last-analyzed-rev> HEAD; do
    ./scripts/build-at $rev $R/bisect $W/tinygo/tinygo-regex.wasm $W/hashset/benchmark.wasm ...
done
for c in $R/bisect/*.cwasm; do
    wasmtime/target/release/wasmtime objdump --funcs all --addresses $c > ${c%.cwasm}.asm
done
python3 $S/asm_tools.py same $R/bisect/hashset-<old>.asm $R/bisect/hashset-<new>.asm
```

Validate once: `same $R/bisect/<m>-<new>.asm $R/profiles/<m>-regicide.asm`
should report 0 functions differing. In 006 that comparison settled keccak
and siphashx24 (hot code unchanged, so only the measurement moved). It also
localized hashset's flip from +5% to a win to `benchmark()`, and then to
`cf70ddd` by replaying one input at `cf70ddd` and its parent. In 005 the same
check found hashset's hot code identical while its cycles moved 6 pp, which
pointed at a cold-looking driver function.

**Do not take a moved number as a code change, or the reverse.** sqlite3
moved from +1.46% n.s. to -5% cycles because its code changed; siphashx24
moved from +0.73% n.s. to +1.35-2.31% significant on identical code, because
the host was quieter. keccak went from -0.8% n.s. to -1.2% significant on
identical code in 006.

### Start from the event counts

Before reading any block, replay the hot function's corpus input with the event
log and tally events by kind:

```bash
RUST_LOG=regicide::region_allocation=trace,regicide::coalescing=trace \
    regicide/target/release/examples/run -q $R/corpus/<module>-<NNNN>-u0-<N>.postcard 2>&1 \
    | grep -oE "event: [A-Za-z]+" | sort | uniq -c | sort -rn
```

Three ratios found every mechanism in analysis 002; the fourth row is 005's:

| signature | what it means |
| --- | --- |
| `CoalesceRefused` ≳ `SpansCoalesced`, or `coalesce-out-of-fuel` nonzero | coalescing gave up (known-findings §2; fixed by `efb70f6`, so this would be a regression of that fix) |
| `BundleSplitBetweenSpans` ≫ `BundleConstructed`, nearly all `index: 1` | split peeling (known-findings §3); tally the `index:` field to check |
| `BundleSpilledToStack` in the tens of thousands | peeled pieces finding no register; stack traffic follows |
| `SpansCoalesced` ≫ `CoalesceRefused`, few splits, yet stack traffic far above ion's | not the allocator's core passes: look at spill-slot assignment and edge moves (005: slot-to-slot copies, known-findings §8) |

Two traps in the text log:

- **`SpanConstructed` span IDs are provisional.** They are logged before the
  block's spans are sorted, and only the in-memory log is remapped (under the
  `visualizer` feature). The span → *block* mapping is right; span → *value*
  is permuted within each block. Use the visualizer's data when you need the
  value.
- **`BundleAssignedNonPreferred` includes unhinted bundles.** Its
  `preference: FixedEntitySet {}` marks a bundle that had no hint to honor.
  Exclude those when computing a hint-honor rate.

A slow replay (minutes for one function) is itself a symptom: the peel loop
in §3 of known-findings is O(spans × registers) interference queries.

### The "why" ladder

Work down the "why" ladder for each block, not just the first level:

1. **What** differs in the code? A stack slot where the other got a register, an
   extra register-register move, a spill or reload the other avoided, a spill
   hoisted out of a loop or left inside it.
2. **Why** did the allocator do that? Bundle splitting, eviction, spill
   placement, coalescing refused, a fixed-register constraint. `scripts/log` and
   `scripts/visualize` answer this.
3. **Why** did regicide's design — its regions, its heuristics, its priority
   order, its cost model — lead to that decision?
4. **So what** should we fix or preserve?

Write each report as you finish the benchmark, not all at the end.

## 6. Write the aggregate report

`$R/report.md` is what the user actually reads. It contains:

- the date and time;
- a top-line summary of the **wall-time (cycles)** results: geomean
  speedup/slowdown, how many benchmarks were significantly sped up, how many
  significantly slowed down, and a table of every benchmark with a
  significant cycles difference, sorted from largest slowdown to largest
  speedup, with the instruction delta beside it as a diagnostic column;
- the top `N` things to look into to improve code quality;
- the top `N` things we do well and must preserve;
- caveats;
- **last, the compile-time table** (§7).

**Rank by breadth and heat, not by how interesting the bug is.** An issue that
recurs across every benchmark is worth far more than a one-off, and an issue in
the hottest block of a hot function is worth far more than one in a block that
never showed up in a profile. Say explicitly, for each item, how many benchmarks
it appeared in and how hot the code was — that is what makes the list
actionable.

Things the top line must do, learned from getting them wrong:

- **Lead with wall time.** The headline is "regicide is significantly slower
  on X and faster on Y by cycles". Instruction counts go in a column and a
  sentence, never in the headline.
- **Name the cases where instructions and cycles disagree, and say which
  way.** "More instructions, less time" is regicide's usual trade (006:
  sqlite3, keccak, blake3), and the reader needs to know it so they do not
  read an instruction regression as a code-quality regression.
- **Give each finding a cost in cycles.** Breadth without cost is not
  actionable: the same mechanism was worth ~1% on one benchmark and 53% on
  another. If cycles could not attribute a cost to it, say so; do not
  substitute an instruction count.
- **Say what you did *not* analyze, and where it ranked.** A report covering the
  5th, 6th and 10th worst regressions while the top four are untouched should
  say so prominently, not bury it. (Then go analyze the worst one — in 001
  it was the strongest evidence in the report; in 002 it turned out to have a
  different root cause from the rest, which is exactly why it must be
  analyzed rather than assumed.)

**Revise the whole report when a late result changes it**, rather than appending
a correction. If a `-i 90` re-run lands after the per-benchmark write-ups, go
back through them. A reader should not have to reconcile a stale severity
claim in one file against a corrected one in another.

Template in `references/report-templates.md`.

## 7. Compile time vs ion — measure and tabulate, nothing more

**After** the code-quality report is written (§6), measure how long each
allocator takes to compile each benchmark, and add the result as the
**last section of `report.md`**. This is a measurement only:

- **No analysis.** Do not explain why a module compiles faster or slower,
  do not profile compilation, do not open a per-benchmark file about it, and
  do not let it into the code-quality rankings or the fix/preserve lists.
- **`insts-retired` is the metric here**, unlike execution. Compilation is
  the allocator's own work, with no move-vs-memory trade to hide, so retired
  instructions track compile wall time closely and are quiet. Low sample
  counts suffice: `-i 5`.
- **The whole `pca.suite`**, all 17 modules, including the three left out
  of the execution runs: they are compiled like any other module, and
  compiling them is not host-bound.

```bash
cd ~/regalloc2
S=.claude/skills/analyze-regicide-codegen/scripts
./scripts/sightglass -p compilation -i 5 benchmarks/pca.suite \
    > $R/sightglass-data/compile-insts-retired.csv
(cd ~/sightglass && cargo run --release -q -- effect-size \
    -f ~/regalloc2/$R/sightglass-data/compile-insts-retired.csv -i csv -o csv) \
    > $R/sightglass-data/compile-effect-size.csv
python3 $S/rank_benchmarks.py --all \
    $R/sightglass-data/compile-effect-size.csv \
    > $R/sightglass-data/compile-ranking.txt
```

~3 minutes at regicide `cf70ddd`. `scripts/sightglass` measures the
compilation phase once per process, so `-i 5` is 5 processes per allocator.
The slowest module is the one that decides the run's length. On
2026-10-09 regicide retired 109x ion's instructions compiling
`shootout-switch` (414 billion vs 3.8 billion); give it its own invocation
if it ever makes the run too long to wait for.

The section in `report.md`, verbatim in shape:

```markdown
## Compile time vs ion

Instructions retired in sightglass's compilation phase (`-p compilation
-m insts-retired -i 5`), whole `pca.suite`, same Wasmtime build for both
allocators. Lower is faster; delta is (regicide - ion) / ion. Raw data:
[`sightglass-data/compile-insts-retired.csv`](sightglass-data/compile-insts-retired.csv);
ranking: [`sightglass-data/compile-ranking.txt`](sightglass-data/compile-ranking.txt).

geomean **+531%**; significantly slower 17 of 17, faster 0.

| benchmark | ion | regicide | delta | significant |
| --- | ---: | ---: | ---: | --- |
| `shootout-switch` | 3,807,357,489 | 414,129,598,658 | +10,777% | yes |
| … | | | | |
```

Worst first, like `rank_benchmarks.py` prints it. Do not quote `Sum Total`
here either: it is dominated by whichever module is largest.

## Environment notes

- **Host is macOS on arm64.** `wasmtime hot-blocks` is Linux-only (it needs
  `perf`) and does not exist here; `scripts/profile` is the substitute.
- **`/bin/bash` is 3.2.** No `mapfile`, no associative arrays, no `${x^^}`. The
  scripts themselves run under `#!/usr/bin/env bash`, but one-liners you write
  get 3.2 unless you invoke a newer one deliberately.
- **`reports/` is gitignored**, via a bare `reports` pattern in
  `~/.gitignore`. Report files are untracked by design; committing one needs
  `git add -f`, which you should not do unless asked.
- **`regicide/` is its own cargo workspace**, excluded from regalloc2's and
  checked out as a git worktree. Build regicide things from inside
  `regicide/`, or pass `--manifest-path regicide/Cargo.toml`.
- **`wasmtime/` is a separate checkout too**, and the scripts drive it with
  `--manifest-path wasmtime/Cargo.toml`.
- **Two cargo profiles, not three.** `sightglass`, `disassemble`, `log`,
  `make-corpus` and `visualize` all use `release`; `profile` uses `profiling`
  (optimized plus line tables). Budget a few minutes per Wasmtime build with
  warm dependencies, considerably more from cold. The scripts used to run a
  *debug* Wasmtime, which made every `wasmtime compile` painfully slow; that is
  fixed, so do not budget for it.
- **`wasmtime compile` takes seconds for every `pca.suite` module** at
  regicide `288da68` (005: tinygo-regex 6 s, sqlite3 7 s, hashset 11 s). It
  has not always: `shootout-switch` took ~25 minutes in 001 and tinygo-regex
  ~40 in 002, and a module whose codegen regresses badly is the one that will
  get slow again. If a disassembly takes minutes, that is a finding, and that
  module needs its own sightglass invocation.
- `wasm-tools` must be on `PATH` for `scripts/profile` on a component and for
  `scripts/time-cwasm` (`cargo install wasm-tools`).
- `samply-text` must be on `PATH` for `scripts/profile`
  (`cargo install samply-text`); the script errors out with a hint if it is not.
- Run every long job in the background and do the write-up of the previous step
  while it works (see "How to sequence the work").
- **Count entries with `find ... | wc -l` or `ls -1`, not `ls | wc -l`** — `ls`
  columnizes when its output is a pipe on this host, so `ls | wc -l` silently
  undercounts (it reported 197 for a 393-file corpus).

## Pitfalls

Ordered roughly by how much they cost when hit.

- **Judging execution by `insts-retired`.** It is a bad cost estimate for a
  register allocator, because moves are its blind spot, and regicide trades
  reloads for moves by design. In 006 three of the five worst benchmarks by
  instructions were cycles wins or parity; in 001 a +15.06% "regression" was
  +1.15% on cycles. Rank, file and cost by cycles (§2).
- **Concluding "moves are cheap" from small benchmarks.** Excess moves cost
  ~1% at heapsort's loop exits and **~50% of wall-clock time** on
  `shootout-switch`, where jump-table fan-out multiplied a register rotation
  ~4,000 times. (The *causes* differ — residual cross-region reconciliation
  vs. `MAX_MERGE_SPANS` — but the lesson about cost holds.) Say "cheap per
  occurrence, severe where the shape multiplies it" and name which case you
  measured.
- **Counting spills as `str`/`ldr` on arm64.** Spills are `stur`/`ldur` against
  `sp`; the `str`/`ldr` columns look reassuringly equal while spill traffic has
  doubled. Filter on `\[sp`. See `references/analyzing-allocation.md`.
- **Writing the report at the end.** The most common way this work gets lost.
  Write continuously (see Invariants).
- **Analyzing a host-bound benchmark.** On `pca.suite`, 97.8% of execution cost
  is in three modules that are host-GC-bound, host-memcpy-bound, or an
  unprofilable component composition. Check the call tree under `array_call_native` before
  analyzing anything (§4).
- **Trusting `Sum Total`.** On this suite it reads +0.84% against a
  per-benchmark geomean of +20.10% — a ~24x understatement. Per-benchmark
  results are what the report is about.
- **Believing a cycles result from its mean alone.** Check median, trimmed mean
  and minimum-to-minimum, and the within-engine spread. That is what separated
  two real results from one noise artifact in the last run.
- **Concluding "no difference" from an underpowered run.** Three iterations of
  `cycles` resolves nothing, and `-i 30` left three of ten benchmarks
  unresolved or flipping in 006. Use `-i 60`, then `-i 90`; if a benchmark
  still does not resolve, say its wall time is inconclusive, without
  substituting an instruction count.
- **Analyzing compile time.** §7 is a table. Profiling compilation or
  explaining a compile-time delta is out of scope.
- **Benchmarking on a loaded host.** Check `uptime` first, not after a result
  fails to resolve, and do not let your own corpus builds compete with a cycles
  run.
- **Analyzing cold code.** Always go through the profile first. A block with no
  samples is not a finding.
- **Analyzing only the regressions.** Half the deliverable is knowing what to
  preserve — and a win can be invisible to the metric you are ranking by.
- **Stopping at level 1 of the "why" ladder.** "Regicide inserted an extra move"
  is an observation. The finding is *why* it did, and what about its design made
  it do so.
- **Trusting a previous report's mechanism.** 001's explanation of
  `shootout-switch` (region boundaries, dropped hints) was wrong; the event
  log showed coalescing refusing ~109,000 legal merges. Re-derive the
  mechanism from the event counts before building on it.
- **Copying a percentage without checking its sign convention.** Reports use
  negative = faster; sightglass's output, raw `effect-size` CSV and analysis
  001's files do not all agree. See "Sign convention".
- **Calling a cycles win "register choice" when the loop is unchanged.**
  001's sieve win did not reproduce after an unrelated change shifted the
  identical hot loop across a cache line. If the hot loop's instructions match
  ion's, suspect layout first.
- **Launching sightglass on a module regicide takes 40 minutes to compile.**
  It is paid once per process. Not a problem at present (every module
  compiles in seconds), but check §1/§4 disassembly times before assuming so.
- **Reading a pruned profile.** `samply-text`'s default threshold hid 76% of
  sqlite3's guest samples; the ranking was missing its #2 function. Check
  `guest_profile.py`'s coverage line (§4).
- **Dismissing a function as cold because its self time is small.** hashset's
  1.5%-self-time driver cost the benchmark ~5 pp through reloads on the path
  of 11,000 calls. When the hot code is unchanged and the cycles are not,
  look at what changed and bisect at runtime (`scripts/build-at`, §3).
- **Running your own builds during a timing run.** In 005 four scratch
  `cargo build`s launched beside a cycles run raised hashset's CV from ~5% to
  29%, and the run had to be thrown away.
- **Building a finding on a profile self-time delta alone.** In 006 five
  functions moved 13-69% in self time with no code difference that could
  explain it (e.g. sqlite3 `function[401]` -69% on 5 fewer instructions per
  call; `MatchRunePos` +0.16 samples/run on code with fewer moves than ion's).
  Report them as unexplained; do not attribute them.
- **Filtering `scripts/visualize` by function name.** Captures are named
  `u0-N`; a name filter silently renders nothing (§5).
- **Linking old reports from a new one.** Reports are standalone
  (Invariants). Rebuild an old revision with `build-at` instead.
- **Checking in-flight branches only after writing the recommendation.** The
  user works on regicide in other branches, and tags dropped experiments
  (`git tag -l 'code-quality-*'`). `git branch -a` and
  `git log --all --since=<date>` against the files you are about to blame —
  read-only — and say how your finding relates to what is in flight.
- **Proposing a fix without checking it exists.** One session's draft
  recommended adding cross-region register hinting; `hint_across_regions` had
  been there all along, and the real finding was one level deeper (in 001, the
  hints were mostly not honored at assignment; since fixed, see
  known-findings §1). Grep before you recommend — including the user's
  in-flight branches.
- **Reading the flat profile view.** It is dominated by sleeping threads. Use
  `-t`.
- **Matching functions between disassemblies by index.** Match by name;
  numbering can differ.
- **Skipping the whole-suite corpus, or building per-module ones instead.**
  It is required (§3): the report's static-quality section needs all 17
  modules, and building per-module corpora pays the Wasmtime build once per
  module. 006 built per-module corpora only, and its static table covered
  eight modules.
- **Drifting into step 3.** If you find yourself editing regicide, stop. Write
  the finding down instead.

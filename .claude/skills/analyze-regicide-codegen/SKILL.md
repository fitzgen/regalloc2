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

1. **Run sightglass**, comparing Wasmtime with regicide against Wasmtime with
   regalloc2's backtracking (ion) allocator. ← you do this
2. **Analyze** the results: why is regicide faster or slower on each? ← you do this
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
- **Code quality, not compile speed.** You are optimizing the code that comes
  *out* of the allocator, not how fast the allocator runs. A finding of the form
  "regicide allocates faster" is out of scope; so is "this analysis would make
  regicide slower to run".
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
| **Cycles** (≈ wall time) | sightglass `-m cycles` | What we actually care about. Very noisy for small differences, so often unresolvable — see §2. |
| **Native instructions executed** | sightglass `-m insts-retired` | Much quieter. But blind to the difference between a register-register move (cheap) and a spill or reload that touches memory (expensive). |
| **Virtual cycles** | hand-counted per block | Our proxy for "what would a real CPU spend here", counted over one block. Definition and cost table in `references/analyzing-allocation.md`. Lower is better. |
| **Stack frame size** | `run.rs` `slots`, disassembly | How many spill slots we created. All else equal, smaller is better. |
| **Moves dynamically executed** | hot block × static moves | A move in the innermost loop costs far more than a move in a cold block. |
| **Moves statically generated** | `run.rs` counts | `moves`, `spills`, `reloads`, `remats`. Lower is better — but moves in `COLD` blocks are often deliberate (known-findings §4). |

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

**Compile time no longer gates the screen** (analysis 005, regicide
`288da68`): the whole 14-benchmark `-i 5` screen took ~3 minutes, an
8-benchmark `-i 30` cycles run ~2 minutes, and every module in `pca.suite`
compiles in seconds under regicide. In 002 a single module (`tinygo-regex`,
then 40 minutes per compile) turned a 10-minute screen into 2 hours, because
sightglass compiles inside every measured process. If a regicide change
brings a slow compile back, it will show up in §1's disassembly time; then
run that module in its own invocation and keep it out of `-i 30` cycles runs.
Sightglass buffers its CSV to the end, so a half-finished run gives you
nothing.

A sequence that keeps the critical path short:

1. §0 report skeleton (seconds).
2. **Check the host is quiet**: `uptime`, `ps -Ao %cpu,comm -r`. Do this
   *first*, not when a result fails to resolve. `cycles` is the metric that
   decides, and on a loaded machine it will not resolve; see §2. Note what
   you cannot stop (the user's fuzzers and experiments, endpoint security)
   in `misc/measurement-notes.md`.
3. §1 preflight, which pays for the first `release` Wasmtime build (~7 min
   after regalloc2/regicide changes). Write §0 while it builds.
4. §2 the `insts-retired` screen of the whole suite minus its host-bound
   modules (~3 min).
5. **Immediately** `cycles` on everything that ranked significant either way
   (`-i 30`), then `-i 45` on any result that is noisy or disagrees in
   direction with instructions (~2 min each). See the warning below.
6. §3 per-module corpora and §4 disassemblies for every benchmark you will
   analyze, in the background (seconds to a few minutes each). **Do not run
   them during a cycles run**; your own builds are the one load you control.
7. §4-§5 profile and analyze, benchmark by benchmark, writing each report as
   you finish it. **Start from what changed since the last analysis**
   (§5, "Start from the diff against the last analysis"): in 005, 5 of 8
   benchmarks' hot functions were byte-identical to 003's, which settled
   them in minutes and pointed straight at the ones that moved.
8. §6 aggregate report.

### Launch `cycles` early, and do not write severity claims before it lands

**The single most expensive mistake available in this workflow.** Instruction
counts find the mechanism; they are a bad estimate of what it costs. For a
register allocator the difference is usually register-register moves, which is
exactly `insts-retired`'s blind spot — a `mov` and a cache-missing load both
count as 1.

Observed on `pca.suite` in one session:

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

Separate **mechanism** from **magnitude** in every report, and derive each
mechanism from the event log rather than from the code shape: in that session
every magnitude moved, and one mechanism — `shootout-switch`'s, attributed to
region boundaries by analogy with heapsort — turned out to be wrong (002 found
`MAX_MERGE_SPANS`).

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
| `corpus/` | `scripts/make-corpus` output, so allocations can be replayed cheaply. |
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

**Screen with `insts-retired`, confirm with `cycles`** — and run them in the
order below, not sequentially. Instruction counts rank the suite cheaply and
find every mechanism; cycles is the metric we care about, needs far more samples,
and frequently will not resolve at all. Full policy and the reasoning in
`references/measurement.md`.

```bash
cd ~/regalloc2
R=reports/codegen-analysis-NNN   # the directory from §0

# 1. The suite minus its host-bound modules. `splay`, `shootout-memmove` and
#    `cm-online-stats` are 97.8% of pca.suite's cost and have no
#    register-allocated hot code (§4). ~3 minutes in 005.
grep -v '^[[:space:]]*#' ~/sightglass/benchmarks/pca.suite | grep -v '^[[:space:]]*$' \
    | grep -vE 'splay|memmove|cm-online' > $R/sightglass-data/screen.suite
./scripts/sightglass -i 5 $(sed 's|^|benchmarks/|' $R/sightglass-data/screen.suite) \
    > $R/sightglass-data/screen-insts-retired.csv

# 2. Cycles on every benchmark that ranked significant either way, BEFORE the
#    deep analysis. `--pin` binds each process to one core (advisory on Apple
#    Silicon; cannot hurt).
./scripts/sightglass -m cycles -i 30 <the picks from rank_benchmarks.py> -- --pin \
    > $R/sightglass-data/confirm-cycles.csv

# 3. Again at -i 45 on whatever cycles_robustness.py calls NOISY or MIXED, and
#    on any benchmark where cycles and instructions disagree in direction.
./scripts/sightglass -m cycles -i 45 <those> -- --pin \
    > $R/sightglass-data/confirm2-cycles.csv
python3 .claude/skills/analyze-regicide-codegen/scripts/cycles_robustness.py \
    $R/sightglass-data/confirm-cycles.csv
```

`cycles_robustness.py` prints mean, median, 20%-trimmed mean and
minimum-to-minimum per benchmark with a verdict: `robust` when all four
agree in sign, `MIXED` when they do not, `NOISY` when either engine's CV
exceeds 5%. In 005 it flagged blake3's +4.20% "regression" (one 16%
outlier; median -0.43%) on sight.

If a compile-time regression ever makes the full screen slow again, fall back
to a subset screen of five or six small Wasm-bound benchmarks first so the
analysis is not blocked on it.

Sightglass buffers its CSV until the end, so a long run gives you nothing
until it finishes; there is no partial output to poll.

Then rank the benchmarks and pick the extremes:

```bash
cd ~/sightglass
cargo run --release -q -- effect-size \
    -f ~/regalloc2/$R/sightglass-data/screen-insts-retired.csv -i csv -o csv \
    > ~/regalloc2/$R/sightglass-data/screen-effect-size.csv

python3 ~/regalloc2/.claude/skills/analyze-regicide-codegen/scripts/rank_benchmarks.py \
    ~/regalloc2/$R/sightglass-data/screen-effect-size.csv --top 5
```

`rank_benchmarks.py` prints every benchmark ranked worst-first with its delta
and whether sightglass calls it significant, then the top-`N` each way as
copy-pasteable `.wasm` paths for the confirmation run. Save its output into
`$R/sightglass-data/ranking.txt`.

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
- **Cycles may simply not resolve.** If it does not, say so plainly in the
  report — rank by `insts-retired` and record that cycles was inconclusive.
  Do not promote a noisy cycles number to a finding. Check robustness (median,
  trimmed mean, minimum-to-minimum, within-engine spread) before believing one
  that *is* significant; `references/measurement.md` has the snippet.
- **Severity written from instruction counts.** For a register allocator the
  difference is usually register-register moves, which `insts-retired` cannot
  distinguish from a cache miss. One session saw +15.06% instructions become
  +1.15% cycles, and a +2.23% "regression" become a -7.38% win. Write the
  mechanism when you have it; write the magnitude when cycles lands.

If `insts-retired` and `cycles` disagree in direction, that is informative
rather than suspicious: it usually means moves were traded for memory traffic
(or the reverse). In 005, sqlite3 was +6.28% instructions and **-5% cycles**
(regicide has 29% fewer stack accesses, 37% more moves), and hashset -3.93%
instructions and **+5% cycles**. Both were the most important results of the
run. Note it and look at the code.

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
  --benchmark-phase execution -m cycles --processes 15 --iterations-per-process 3 --pin \
  --raw -f csv benchmarks/hashset/benchmark.wasm > ~/regalloc2/$R/sightglass-data/align6-cycles.csv
```

If the delta survives (hashset's did: +6.19%), it is not layout.

## 3. Build a corpus

Capture the allocator inputs so that replaying an allocation costs one
allocation (a few seconds) instead of a Wasmtime run.

**Build a per-module corpus for each benchmark you are going to analyze, into
its own directory.** That is what the analysis actually needs, it costs a couple
of minutes per small module, and it keeps provenance unambiguous:

```bash
cd ~/regalloc2
./scripts/make-corpus -c $R/corpus-heapsort \
    ~/sightglass/benchmarks/shootout/shootout-heapsort.wasm
./scripts/make-corpus -c $R/corpus-keccak \
    ~/sightglass/benchmarks/shootout/shootout-keccak.wasm
```

In 005 the five per-module corpora took 9 s to 195 s each (sqlite3: 3,472
inputs, the slowest). Replaying the hottest functions takes about a second
each (tinygo `countMatches`: 1.2 s, down from 471 s in 002).

A whole-suite corpus is also useful: it is a fixed, reusable workload and the
source of the suite-wide static baseline. It used to be impractical (002:
2 of 17 modules in 68 minutes); at current compile speeds it should finish,
but run it in the background and never alongside a cycles run.

Expand the suite into module paths and hand them all to `make-corpus` at once —
one invocation, so a single Wasmtime build serves every module:

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

./scripts/make-corpus $R/corpus "${WASMS[@]}"
```

This takes a while — it compiles every module in the suite — so **run it in the
background** and get on with §4 while it works. `make-corpus` exits nonzero if
any module captured nothing; check for that rather than assuming it finished
cleanly.

Each capture lands as `$R/corpus/<module>-<counter>[-<function>].postcard`
(postcard-encoded, not JSON). `<module>` is the `.wasm`'s base name, except for
sightglass's many identically-named `benchmark.wasm` modules, where it is the
*directory* name instead — so `blake3-scalar/benchmark.wasm` captures as
`blake3-scalar-0000-...` and does not collide with `hashset`'s. Re-running over
a module overwrites rather than accumulating duplicates.

The input for a function you care about is found by name, when the module
carries a name section:

```bash
ls $R/corpus | grep -i <function-name>
```

Replay one, with regicide's own event log, to see the decisions behind an
allocation:

```bash
cd ~/regalloc2/regicide
RUST_LOG=trace cargo run --all-features --example run -- \
    ../$R/corpus/bz2-0003-some_function.postcard
```

A whole-suite corpus also gives you a cheap aggregate: the per-function counts
over every function in every benchmark, in one run.

```bash
cd ~/regalloc2/regicide
cargo run --all-features --example run -- -q ../$R/corpus \
    > ../$R/misc/corpus-counts.txt
tail -5 ../$R/misc/corpus-counts.txt      # the `=== totals ===` line
```

Those totals (`moves`, `spills`, `reloads`, `remats`, `critical-edges`, `slots`)
are suite-wide static allocation quality in six numbers, and they are the
numbers step 3 will diff against after a change. Save them, and quote them in
the aggregate report. **Caveat:** since Cranelift reports `COLD` blocks, some
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

**An old report's `.cwasm`s are a free baseline.** `disassemble` leaves them
in `$R/profiles/asm/`, and they still load in today's Wasmtime; timing 003's
beside today's reproduced both reports' cycles results in one session.

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

Those three are **97.8% of the suite's instruction count**, and the screen
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
instructions down to regicide's design choices, how to count virtual cycles, and
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

### Start from the diff against the last analysis

Before any per-block work, ask of each benchmark: **did its hot code change
since the last report?** Previous reports keep their disassemblies in
`profiles/`, so this is one command per benchmark:

```bash
S=.claude/skills/analyze-regicide-codegen/scripts
OLD=reports/codegen-analysis-<prev>
python3 $S/asm_tools.py same $OLD/profiles/hashset-regicide.asm $R/profiles/hashset-regicide.asm
```

and the same for the corpus inputs (`cmp` the `.postcard`s; replay both
through the old and new `run`). In 005, keccak's, blake3's, siphashx24's,
rust-protobuf's and hashset's hot functions were byte-identical to 003's.
That settled three benchmarks in minutes (prior analysis stands, magnitude
updated) and exposed hashset: identical hot code, but its cycles moved 6 pp,
so the cause had to be elsewhere.

**Do not take a moved number as a code change, or the reverse.** sqlite3
moved from +1.46% n.s. to -5% cycles because its code changed; siphashx24
moved from +0.73% n.s. to +1.35-2.31% significant on identical code, because
the host was quieter.

### Start from the event counts

Before reading any block, replay the hot function's corpus input with the event
log and tally events by kind:

```bash
RUST_LOG=regicide::region_allocation=trace,regicide::coalescing=trace \
    regicide/target/release/examples/run -q $R/corpus-X/<fn>.postcard 2>&1 \
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
- a top-line summary of the sightglass results: geomean speedup/slowdown, how
  many benchmarks were significantly sped up, how many significantly slowed
  down, and a table of every benchmark with a significant difference, sorted
  from largest slowdown to largest speedup;
- the top `N` things to look into to improve code quality;
- the top `N` things we do well and must preserve.

**Rank by breadth and heat, not by how interesting the bug is.** An issue that
recurs across every benchmark is worth far more than a one-off, and an issue in
the hottest block of a hot function is worth far more than one in a block that
never showed up in a profile. Say explicitly, for each item, how many benchmarks
it appeared in and how hot the code was — that is what makes the list
actionable.

Three things the top line must do, learned from getting them wrong:

- **Report both metrics, and name the disagreement if there is one.** "Behind on
  12 of 17 by instruction count; on cycles most of that does not resolve and
  regicide wins the one benchmark that does" is the truth, and the tension is
  itself a result. A top line quoting one metric misdescribes the comparison.
- **Give each finding a cost, and say which metric it came from.** Breadth
  without cost is not actionable: the same mechanism was worth ~1% on one
  benchmark and 53% on another.
- **Say what you did *not* analyze, and where it ranked.** A report covering the
  5th, 6th and 10th worst regressions while the top four are untouched should
  say so prominently, not bury it. (Then go analyze the worst one — in 001
  it was the strongest evidence in the report; in 002 it turned out to have a
  different root cause from the rest, which is exactly why it must be
  analyzed rather than assumed.)

**Revise the whole report when a late result changes it**, rather than appending
a correction. If cycles lands after the per-benchmark write-ups, go back through
them. A reader should not have to reconcile a stale severity claim in one file
against a corrected one in another.

Template in `references/report-templates.md`.

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

- **Stating severity from `insts-retired`.** It finds mechanisms; it is a bad
  cost estimate for a register allocator, because moves are its blind spot. One
  session had a +15.06% "regression" that was +1.15% on cycles, and a benchmark
  filed under `regressions/` that was a -7.38% *win* (which later turned out to
  be layout). Launch cycles early;
  write the mechanism up immediately and the magnitude only once cycles lands.
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
  `cycles` resolves nothing. Either raise `-i` or report `insts-retired` and say
  cycles was inconclusive — and note a `-i 3` screen can miss a real regression
  a `-i 5` run resolves.
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
- **Letting a whole-suite corpus block the analysis.** Per-module corpora for
  your actual targets are what the work needs (§3).
- **Drifting into step 3.** If you find yourself editing regicide, stop. Write
  the finding down instead.

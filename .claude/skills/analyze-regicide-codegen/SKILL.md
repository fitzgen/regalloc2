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
| **Moves statically generated** | `run.rs` counts | `moves`, `spills`, `reloads`, `remats`. Lower is better. |

## How to sequence the work

Builds dominate the wall clock here: the first Wasmtime build of a given cargo
profile takes tens of minutes, and `scripts/sightglass`, `scripts/profile`,
`scripts/disassemble`/`log` and `scripts/make-corpus` between them want three
different profiles (`release`, `profiling`, `debug`). **Start the long jobs in
the background and write up the previous step while they run** — do not sit
idle watching a build.

A sequence that keeps the critical path short:

1. §0 report skeleton (seconds).
2. §1 preflight — the first `debug` Wasmtime build. Start it, then finish §0.
3. §2 the `insts-retired` screen — the `release` build. Start it in the
   background.
4. §3 the whole-suite corpus — reuses the `debug` build. Start it in the
   background too; it compiles every module in the suite.
5. When the screen lands, rank it and launch the `cycles` confirmation run on
   the picks, in the background.
6. §4–§5 profile and analyze, benchmark by benchmark, writing each report as you
   finish it. This is where the thinking happens and it needs no build.
7. §6 aggregate report.

## 0. Set up the report directory

Pick the next unused number and create the directory tree **before** you start
measuring, so the first results have somewhere to land:

```bash
cd ~/regalloc2
ls reports/                              # find the highest codegen-analysis-NNN

# `$R` is used throughout this skill for the report directory. Set it in every
# shell you use, since shell state does not persist between commands.
R=reports/codegen-analysis-001
mkdir -p $R/{misc,sightglass-data,profiles,corpus,improvements,regressions}
```

`NNN` is a unique, incrementing number, so that historical reports are never
clobbered: take the highest existing one and add one.

| Path | Holds |
| --- | --- |
| `report.md` | The aggregate report (§6). Create it early as a skeleton and fill it in. |
| `sightglass-data/` | Raw sightglass CSVs, so others can re-analyze without re-measuring. |
| `profiles/` | `scripts/profile` output you worked from. |
| `corpus/` | `scripts/make-corpus` output, so allocations can be replayed cheaply. |
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

**This is also where you pay for the first Wasmtime `debug` build** — a couple
of minutes when dependencies are warm, much longer from cold. Start it, then set
up the report skeleton while it runs.

## 2. Run the benchmarks

`scripts/sightglass` does nearly all of this: it builds `wasmtime-bench-api`
once and hands the *same* dylib to sightglass twice, once per allocator, so a
run compares allocators rather than builds. Full flag reference in
`references/tools.md`.

**Screen with `insts-retired`, confirm with `cycles`.** Instruction counts are
quiet enough to rank the suite cheaply; cycles is the metric we care about but
needs many more samples to resolve, so spend it only on the benchmarks you are
going to write about.

```bash
cd ~/regalloc2
R=reports/codegen-analysis-001

# Screen: the whole suite, insts-retired, cheap. CSV goes to stdout.
./scripts/sightglass > $R/sightglass-data/screen-insts-retired.csv
```

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

Confirm just those benchmarks with cycles, at high sample count:

```bash
cd ~/regalloc2
./scripts/sightglass -m cycles -i 30 \
    benchmarks/bz2/benchmark.wasm benchmarks/regex/benchmark.wasm ... \
    > $R/sightglass-data/confirm-cycles.csv
```

Read `references/measurement.md` before interpreting any of this. The three
things that most often go wrong:

- **Direction.** These are counts, so **lower is faster**. Check which engine is
  `a_engine` before calling anything a win.
- **Significance.** Sightglass prints `Δ` and a comparison sentence *only* when
  the difference is significant; otherwise "No difference in performance". The
  synthetic `Sum Total` row reaches significance far more readily than any
  individual benchmark.
- **Cycles may simply not resolve.** If it does not, say so plainly in the
  report — rank by `insts-retired` and record that cycles was inconclusive.
  Do not promote a noisy cycles number to a finding.

If `insts-retired` and `cycles` disagree in direction, that is informative
rather than suspicious: it usually means moves were traded for memory traffic
(or the reverse). Note it and look at the code.

## 3. Build a corpus for the whole suite

Capture the allocator inputs so that replaying an allocation costs one
allocation instead of a Wasmtime rebuild.

**Build the corpus over every benchmark the suite runs, not just the top-`N` you
are going to analyze.** The analysis subset is chosen from a noisy screen and
will change between invocations; the corpus is a fixed, reusable workload that
later invocations, and the optimization work in step 3, can replay without
re-capturing. Capturing the whole suite costs one extra Wasmtime run per module
and saves repeating this every time the picks change.

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

Each capture lands as `$R/corpus/<module>-<counter>[-<function>].json`. The
module-name prefix is why one directory can hold the whole suite without
collisions, and re-running over a module overwrites rather than accumulating
duplicates. The input for a function you care about is found by name:

```bash
ls $R/corpus | grep -i <function-name>
```

Replay one, with regicide's own event log, to see the decisions behind an
allocation:

```bash
cd ~/regalloc2/regicide
RUST_LOG=trace cargo run --all-features --example run -- \
    ../$R/corpus/bz2-benchmark-0003-some_function.json
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
the aggregate report.

## 4. Find the hot code

For each benchmark you are writing up, profile it and work down from the hottest
code. Cold code is not worth analyzing: a perfect fix to a block that never
executes is worth nothing, and a block you never even sampled is worth less than
that.

```bash
cd ~/regalloc2
./scripts/profile -t -n 200 ~/sightglass/benchmarks/bz2/benchmark.wasm \
    > $R/profiles/bz2-regicide.txt
./scripts/profile -t -n 200 --regalloc backtracking \
    ~/sightglass/benchmarks/bz2/benchmark.wasm \
    > $R/profiles/bz2-backtracking.txt
```

Read the **call tree** (`-t`), not the flat view: the flat view is topped by
Wasmtime's idle threads parked in the async runtime, which are asleep rather
than working. The guest's frames sit under `array_call_native`.

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
disassemblies**; match functions by name, not by index or offset. The whole-file
`diff` is for orientation; the per-function `--filter` output is what you
actually analyze.

## 5. Analyze, and write one report per benchmark

For each of the top `N` most-improved and top `N` most-regressed benchmarks,
write `$R/improvements/<benchmark>.md` or `$R/regressions/<benchmark>.md`.

The full method — what to look for in a block, the "why" ladder from
instructions down to regicide's design choices, how to count virtual cycles, and
which log targets answer which question — is in
`references/analyzing-allocation.md`. **Read it before analyzing your first
block.** The report layout and a filled-in example are in
`references/report-templates.md`.

In short, each per-benchmark report contains:

- the date and time, and the benchmark's speedup or slowdown (with the metric
  and whether it was significant);
- the `scripts/log` command that reproduces the compile with logging;
- the 3 hottest functions, each with its sample share, the `run.rs` command for
  its corpus input, and the `scripts/disassemble` command for its disassembly;
- for each function, its 3 hottest basic blocks, each with the disassembly under
  both allocators and an analysis of what the allocator did differently.

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
- The first build of each profile is slow, and the scripts want three different
  ones: `release` (`sightglass`), `profiling` (`profile`), and `debug`
  (`disassemble`, `log`, `make-corpus`, `visualize`). Budget ~2 minutes per
  Wasmtime build once its dependencies are warm, considerably more from cold.
- **`disassemble`, `log`, `make-corpus` and `visualize` run a *debug* Wasmtime**,
  so the `wasmtime compile` step itself is slow — a couple of minutes for even a
  200KB module, and proportionally worse for `sqlite3` or `spidermonkey`. This
  is the cost that makes a whole-suite `make-corpus` a background job. Budget
  for it rather than assuming a compile is quick.
- Run every long job in the background and do the write-up of the previous step
  while it works (see "How to sequence the work").

## Pitfalls

- **Writing the report at the end.** The most common way this work gets lost.
  Write continuously (see Invariants).
- **Concluding "no difference" from an underpowered run.** Three iterations of
  `cycles` resolves nothing. Either raise `-i` or report `insts-retired` and say
  cycles was inconclusive.
- **Trusting `Sum Total`.** It reaches significance much more easily than
  individual benchmarks and can hide a per-benchmark regression. Per-benchmark
  results are what the report is about.
- **Analyzing cold code.** Always go through the profile first. A block with no
  samples is not a finding.
- **Analyzing only the regressions.** Half the deliverable is knowing what to
  preserve.
- **Stopping at level 1 of the "why" ladder.** "Regicide inserted an extra move"
  is an observation. The finding is *why* it did, and what about its design made
  it do so.
- **Reading the flat profile view.** It is dominated by sleeping threads. Use
  `-t`.
- **Matching functions between disassemblies by index.** Match by name;
  numbering can differ.
- **Drifting into step 3.** If you find yourself editing regicide, stop. Write
  the finding down instead.

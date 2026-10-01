# Measuring, and not fooling yourself

How to run the comparison, which metric decides, and the specific ways this
measurement goes wrong.

The key structural fact: `scripts/sightglass` builds `wasmtime-bench-api` **once**
and hands the same dylib to sightglass twice, once per allocator. So the
comparison is between two allocators in one binary, not between two builds —
there is no way for an unrelated code change to contaminate it, and the engines
appear as `regicide` and `backtracking` in the CSV.

---

## The two metrics

### `insts-retired` — the screen

Native instructions retired. Quiet: differences of a fraction of a percent are
resolvable with 3 samples, which is why it is `scripts/sightglass`'s default.

Its blind spot is exactly the thing this analysis is about: **it cannot tell a
register-register move from a reload.** Both are one instruction; one touches
memory and costs roughly ten times as much. An allocator change that converts
three reloads into three register moves is a real win that `insts-retired` scores
as *zero*.

So it is a good **ranking** instrument and a bad **verdict**.

### `cycles` — the verdict

Elapsed ticks of a fixed-rate timer around the phase (on arm64 macOS the ~24MHz
`cntvct_el0` virtual counter), i.e. wall-clock latency in ticks. It is what we
actually care about, and it does see the memory traffic.

It is also **much noisier**, and for the small differences a register allocator
produces it frequently cannot resolve them at all. That is the central tension
of this skill, and the honest answer is often "cycles was inconclusive".

### The policy

**Screen with `insts-retired` over the whole suite; confirm the extremes with
`cycles` at a high sample count.** Spending 30 iterations of cycles on 40
benchmarks to read 10 of them is waste.

```bash
# Screen: whole suite, cheap.
./scripts/sightglass > $R/sightglass-data/screen-insts-retired.csv

# Confirm: just the benchmarks you will write about.
./scripts/sightglass -m cycles -i 30 \
    benchmarks/bz2/benchmark.wasm benchmarks/hashset/benchmark.wasm \
    > $R/sightglass-data/confirm-cycles.csv
```

If cycles does not resolve, **say so in the report** and rank by
`insts-retired`. Do not promote a noisy cycles number to a finding, and do not
quietly drop the metric you said you would measure.

### When they disagree

Direction disagreement between the two is **informative, not suspicious**, and
is often the most interesting signal available:

- **Cycles better, instructions flat or worse** — memory traffic went down. More
  instructions doing cheaper work: fewer spills, reloads traded for
  register-register moves or remats. A genuine win that `insts-retired` cannot
  see.
- **Instructions better, cycles worse** — fewer instructions doing more
  expensive work. Usually a move eliminated in favor of a stack operand.

Note the disagreement and go look at the code; that is where the finding is.

---

## Reading what sightglass reports

`scripts/sightglass` passes `--raw -f csv`, so **stdout is the raw per-sample
CSV**, not the human-readable analysis. Columns:

```
arch,engine,engine_flags,wasm,process,iteration,phase,event,count
```

Progress messages go to stderr, so redirecting stdout gives a clean CSV.

To get an analysis out of it, convert with `effect-size`, which re-runs
sightglass's own significance test on saved samples — so you can re-analyze
without re-measuring:

```bash
cd ~/sightglass
cargo run --release -q -- effect-size \
    -f ~/regalloc2/$R/sightglass-data/screen-insts-retired.csv -i csv -o csv \
    > ~/regalloc2/$R/sightglass-data/screen-effect-size.csv
```

Output columns:

```
arch,wasm,phase,event,a_engine,a_engine_flags,a_mean,b_engine,b_engine_flags,
b_mean,significance_level,half_width_confidence_interval
```

Then rank it:

```bash
python3 ~/regalloc2/.claude/skills/analyze-regicide-codegen/scripts/rank_benchmarks.py \
    ~/regalloc2/$R/sightglass-data/screen-effect-size.csv --top 5
```

Drop `-o csv` for a human-readable comparison, which prints one block per
`phase :: event :: benchmark` with a Min/Max/Mean/Median table per engine.

---

## The four ways this goes wrong

### 1. Direction

These are **counts**, so **lower is faster**. A positive delta in
`rank_benchmarks.py` means the candidate (regicide) is *faster*, because it
reports `(baseline - candidate) / baseline`. But reading the raw
`effect-size` CSV, check which engine landed in `a_engine` before calling
anything a win — `effect-size` emits each pair once, in an order it chooses.
Getting this backwards inverts the entire report.

### 2. Significance

Sightglass prints `Δ = … ± …` and an "A is X to Y faster than B" sentence **only
when the difference is significant**; otherwise "No difference in performance".
Its test is a Behrens–Fisher confidence interval on the difference of means, and
reduces to

```
|a_mean - b_mean| > |half_width_confidence_interval|
```

which is what `rank_benchmarks.py` applies — no re-derived statistics.
`-s`/`--significance-level` defaults to `0.01` (99%).

Insignificant per-benchmark blocks are hidden by default in the human-readable
output; `--show-insignificant` shows them. By default `rank_benchmarks.py` lists
only significant rows too, and `--all` shows everything.

**An underpowered run reports "no difference" for everything.** Three iterations
of `cycles` resolves essentially nothing. If the whole suite comes back
insignificant, the run is the problem, not the allocator —
`rank_benchmarks.py` says so explicitly and marks its picks as guesses.

### 3. `Sum Total`

Sightglass appends a synthetic `Sum Total` benchmark that sums each sample's
counts across the suite. It reaches significance far more readily than any
individual benchmark, which makes it the headline number — and **it can hide a
per-benchmark regression completely**. This skill is about per-benchmark
behavior; report `Sum Total` as context, never as the result.
`rank_benchmarks.py` keeps it out of the rankings for this reason.

### 4. Preflight

If the Wasmtime build ignores `-O regalloc-algorithm=regicide`, every benchmark
comes back "no difference" and the whole run measures one allocator against
itself. Check the disassemblies differ *before* the long run (§1 of SKILL.md).
This failure mode is indistinguishable from "regicide and backtracking perform
identically", which is why it has to be ruled out up front.

---

## Sample counts

`scripts/sightglass`'s `-i`/`--iterations` is a **total** sample count; the
script converts it into sightglass's processes × iterations-per-process. For the
execution phase it uses 3 iterations per process (the phase measures three
iterations in one process) and spreads the total over processes; every other
phase measures once per process.

| Use | Flags | Why |
| --- | --- | --- |
| Screening the suite | default (`-i 3`, `insts-retired`) | Instruction counts are quiet; 3 is enough to rank. |
| Confirming the extremes | `-m cycles -i 30` | Cycles needs the samples. |
| A result you doubt | `-m cycles -i 60` or more | A lone "significant" result at low sample count can be a fluke that vanishes. Re-confirm before believing it. |

Pass `--pin` through (`-- --pin`) to bind each process's iterations to one core,
which cuts scheduling noise. On Apple Silicon the affinity API is advisory, so
treat it as "can't hurt" rather than a guarantee.

If a result will not resolve, quiet the host first — check `uptime` and
`ps -Ao %cpu,comm -r` for backup daemons, file indexers and endpoint-security
agents, which inject correlated outliers — then raise `-i`.

---

## The per-benchmark regression gate

When you want a pass/fail answer rather than a ranking, the sibling skill's gate
script works unchanged on regicide data:

```bash
python3 ~/.claude/skills/optimize-cranelift-codegen/scripts/check_no_pessimization.py \
    $R/sightglass-data/screen-effect-size.csv \
    --baseline backtracking --candidate regicide
```

It applies the same significance test, ranks worst-first, and exits nonzero if
any benchmark regressed significantly. Note that its framing is a *gate* on a
change — for this skill a regression is the subject of the analysis rather than
a failure, so the exit code is informational. `rank_benchmarks.py` is the better
tool for choosing what to analyze.

---

## Bookkeeping

Keep every raw CSV under `$R/sightglass-data/`, named for what it is
(`screen-insts-retired.csv`, `confirm-cycles.csv`), so every number in the
report can be re-derived without re-measuring.

Record in `$R/misc/measurement-notes.md`: host OS and arch, date, the suite, the
metrics used, the `-i` values, whether `--pin` was on, and anything you learn
about noise on this machine. The next invocation of this skill starts from your
notes.

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

**Screen with `insts-retired`; confirm with `cycles` — and launch the cycles run
early, not as a closing formality.** Spending 30 iterations of cycles on 40
benchmarks to read 10 of them is waste, but so is writing up severity from
instruction counts and then having to revise it.

A cheap three-step that avoids both:

```bash
# 1. Subset screen: five or six small benchmarks, ~4 minutes. Enough to pick
#    targets, and it does not block on the full screen.
./scripts/sightglass -i 5 \
    benchmarks/shootout/shootout-heapsort.wasm \
    benchmarks/shootout/shootout-keccak.wasm \
    > $R/sightglass-data/subset-insts-retired.csv

# 2. Cycles on those picks, in the background, BEFORE the deep analysis.
./scripts/sightglass -m cycles -i 30 \
    benchmarks/shootout/shootout-heapsort.wasm \
    benchmarks/shootout/shootout-keccak.wasm \
    > $R/sightglass-data/confirm-cycles.csv

# 3. The suite minus its host-bound modules, also in the background. The
#    authority for ranking; its wall clock is dominated by slow regicide
#    compiles (see "Compile time decides what you can measure").
./scripts/sightglass -i 5 $(sed 's|^|benchmarks/|' $R/sightglass-data/screen.suite) \
    > $R/sightglass-data/screen-insts-retired.csv   # screen.suite: SKILL.md §2
```

Observed agreement between (1) and (3): `shootout-sieve` came back at **+2.23%
in both**, `shootout-keccak` at +11.00% / +11.06%. A cheap subset screen is a
good substitute for picking targets.

One exception worth knowing: a benchmark can be *significant* in a `-i 5` subset
run and *insignificant* in the `-i 3` full screen, purely on sample count —
`shootout-heapsort` was +15.06% significant in one and +12.72% not significant
in the other. Take the higher-sample number, and note that the low-power run
*understated* a real regression rather than inventing one.

If cycles does not resolve, **say so in the report** and rank by
`insts-retired`. Do not promote a noisy cycles number to a finding, and do not
quietly drop the metric you said you would measure.

### Separate mechanism from magnitude, and do not state severity before cycles

**The most expensive mistake available here.** `insts-retired` is an excellent
*mechanism detector* — quiet enough to rank 17 benchmarks at `-i 3`, and it
pointed straight at every real mechanism in the last full run. It is a **bad
cost estimator** for a register allocator, because the difference is usually
register-register moves, which is precisely its blind spot.

What one session (001) actually measured, with what 002 measured five days
later in brackets:

| benchmark | insts-retired | cycles | |
| --- | ---: | ---: | --- |
| `shootout-switch` | +237.23% | **+53.39%** [+48.93%] | significant — a 1.5x slowdown |
| `shootout-heapsort` | +15.06% | **+1.15%** [-0.27%] | not significant — ~10x smaller |
| `shootout-keccak` | +11.00% | **+1.15%** [-1.27%] | not significant |
| `shootout-sieve` | +2.23% | **-7.38%** [-1.39%, n.s.] | significant — **a win**, until it wasn't |
| `sqlite3` (002 only) | +21.80% | [+2.27%, n.s.] | ~10x smaller again |

Three things follow, and each cost rework to learn:

1. **A benchmark filed as a regression was a win — and then, five days later,
   was not.** `shootout-sieve`'s hot function emitted 37 *more* moves than
   ion's with identical stack traffic and measured ~6% faster (001). After an
   unrelated change removed some moves *ahead of* its innermost loop, the win
   shrank to -1.39%, not significant (002) — the loop itself was identical to
   ion's in every build, but had moved across a 64-byte boundary. Lesson: a
   cycles difference in a benchmark whose hot loop matches ion's instruction
   for instruction is probably layout. Check before attributing it to the
   allocator.
2. **The overstatement factor varies**, so you cannot apply a correction: ~10x
   on the small benchmarks and sqlite3, ~4–5x on `shootout-switch`, where
   fan-out multiplied the moves.
3. **"Excess moves are cheap" is a conclusion reachable from an
   unrepresentative sample.** Moves that cost ~1% at heapsort's loop exits
   cost ~50% of wall-clock time on `shootout-switch`, where jump-table fan-out
   multiplied a register rotation ~4,000 times and produced 5.2x the code (a
   different cause, but the same currency). The honest finding is **cheap per
   occurrence, severe where the code shape multiplies it** — and you cannot
   tell which case you are in from instruction counts.

So: write the mechanism up as soon as you have it, with event-log and
disassembly evidence. Leave the severity sentence until cycles lands. In 001
every magnitude moved, and one mechanism (switch's) later proved wrong: derive
mechanisms from the event log, not by analogy.

### Check robustness before believing any cycles result

A surprising cycles number deserves the same scrutiny whichever direction it
points. Four cheap checks, all from the raw CSV:

```python
import csv, collections, statistics as st
d = collections.defaultdict(list)
for r in csv.DictReader(open(csvfile)):
    d[r['engine']].append(int(r['count']))
bt, rg = sorted(d['backtracking']), sorted(d['regicide'])
k = len(bt) // 5
f = lambda a, c: 100 * (c - a) / a   # negative => regicide faster
print("mean       ", f(st.mean(bt), st.mean(rg)))
print("median     ", f(st.median(bt), st.median(rg)))
print("20%-trimmed", f(st.mean(bt[:-k]), st.mean(rg[:-k])))
print("minimum    ", f(bt[0], rg[0]))   # least noise-contaminated sample
print("CV%        ", 100*st.stdev(bt)/st.mean(bt), 100*st.stdev(rg)/st.mean(rg))
```

This is what distinguished the real results from the noise in that session:

- `shootout-switch` +53.39%: held at every estimator down to **+36.64%**
  minimum-to-minimum, and backtracking's *slowest* sample beat regicide's
  *median*. Believable.
- `shootout-sieve` -7.38%: held at median -5.43%, trimmed -5.61%, minimum
  -6.08%. Believable *as a measurement* — but it did not survive an unrelated
  code-layout change (see above). Robust is not the same as attributable.
- `hashset` +7.02%: **1.8x spread inside one engine's own 30 samples** (CV
  13.9%). Not a result; reported as noise.

Quote the minimum-to-minimum figure when a result matters — it is the most
conservative framing and the least contaminated by scheduler noise.

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

These are **counts**, so **lower is faster**. `rank_benchmarks.py` reports
`(candidate - baseline) / baseline`, so a **negative** delta means the
candidate (regicide) is *faster* and a positive one means it is slower — the
house convention for every report (SKILL.md, "Sign convention"). But reading
the raw `effect-size` CSV, check which engine landed in `a_engine` before
calling anything a win — `effect-size` emits each pair once, in an order it
chooses. Getting this backwards inverts the entire report.

**Analysis 001's report files use the opposite sign** (positive = faster).
The numbers quoted from it in this skill have been converted; the files in
`reports/codegen-analysis-001/` have not.

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

### 3. `Sum Total` — on `pca.suite`, quoting it is a factor-of-24 error

Sightglass appends a synthetic `Sum Total` benchmark that sums each sample's
counts across the suite. It reaches significance far more readily than any
individual benchmark, which makes it the headline number — and **it can hide a
per-benchmark regression completely**.

This is not a theoretical nuance on this suite. Measured:

| | `insts-retired` |
| --- | ---: |
| `Sum Total` | **+0.84%** (significant) |
| per-benchmark geomean | **+20.10%** |
| benchmarks significantly slower | **12 of 17** |
| benchmarks significantly faster | **0** |

`Sum Total` understates the regression by roughly **24x**, because 97.8% of the
suite's instruction count is in three benchmarks that are host-GC-bound,
host-memcpy-bound, or unprofilable — and which duly came back at +0.00%, +0.26%
and +0.37%. `Sum Total` is substantially a measurement of Wasmtime's runtime,
not of either allocator. See §4 of `SKILL.md`.

**So: never quote `Sum Total` as the result, and never compute a cost-weighted
aggregate over this suite.** Treat each benchmark as one observation regardless
of its absolute size, which is what `rank_benchmarks.py` does — it keeps
`Sum Total` out of the rankings for this reason.

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

**Quiet the host *before* you start, not after a result fails to resolve.**
Check `uptime` and `ps -Ao %cpu,comm -r` in the first minute of the analysis —
for backup daemons, file indexers, endpoint-security agents and your own stray
`cargo` builds, all of which inject correlated outliers. Since `cycles` is the
metric that actually decides, host quiescence is a precondition for the whole
analysis, not a troubleshooting step.

In one session the host sat at **load average ~27** throughout, with four
`rustc` processes, binaries from another worktree, and Microsoft Defender at
~58% CPU. The consequences were concrete:

- **Four of six cycles measurements did not resolve.** Several probably would
  have on an idle machine, which would have turned "unresolved" into real
  severity numbers.
- **`hashset` came back unusable** — 1.8x spread inside one engine's own 30
  samples.
- `insts-retired` was unaffected: two independent runs agreed to 0.00
  percentage points on `shootout-sieve`. It counts the benchmark process's own
  retired instructions rather than elapsed time, so **the instruction ranking
  is trustworthy even on a loaded host** — which is a good reason to lean on it
  for ranking and to be correspondingly sceptical of cycles numbers from the
  same conditions.

Beware also of competing for CPU with *yourself*: a whole-suite corpus build or
a second disassembly running alongside a cycles measurement degrades the
measurement you care about. Stop the lower-value job.

---

## The per-benchmark regression gate

When you want a pass/fail answer rather than a ranking, the sibling skill's gate
script works on regicide data — **but it prints deltas in the opposite sign
convention** (`(baseline - candidate) / baseline`, positive = faster). Negate
any number you copy from it into a report:

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

## Compile time decides what you can measure

Sightglass compiles the module once per measured process. A module regicide
compiles slowly multiplies straight into the run: in 002, tinygo-regex took
regicide ~40 minutes per compile, so a `-i 5` screen (2 processes) spent ~80
minutes on it alone and a `-i 30` cycles run would have taken ~7 hours. Know
each module's regicide compile time (from the §1/§4 disassembly) before
choosing which modules go into which run, and keep the slow ones separate.
For a module too slow to measure on cycles, the profile from its `.cwasm`
(guest samples per run) is the fallback — label it as a profile, not a
measurement.

## Bookkeeping

Keep every raw CSV under `$R/sightglass-data/`, named for what it is
(`screen-insts-retired.csv`, `confirm-cycles.csv`), so every number in the
report can be re-derived without re-measuring.

Record in `$R/misc/measurement-notes.md`: host OS and arch, date, the suite, the
metrics used, the `-i` values, whether `--pin` was on, and anything you learn
about noise on this machine. The next invocation of this skill starts from your
notes.

# Measuring, and not fooling yourself

How to run the comparison, which metric decides, and the specific ways this
measurement goes wrong.

The key structural fact: `scripts/sightglass` builds `wasmtime-bench-api` **once**
and hands the same dylib to sightglass twice, once per allocator. So the
comparison is between two allocators in one binary, not between two builds —
there is no way for an unrelated code change to contaminate it, and the engines
appear as `regicide` and `backtracking` in the CSV.

---

## The two metrics, and which one decides

### `cycles` — the ranking and the verdict

Elapsed ticks of a fixed-rate timer around the phase (on arm64 macOS the ~24MHz
`cntvct_el0` virtual counter), i.e. wall-clock latency in ticks. It is what we
actually care about, and it does see the memory traffic.

It is also **noisy**, and the differences a register allocator produces are
small, so it needs many samples: **`-i 60` to rank, `-i 90` to confirm**.
At the old `-i 30`, 006 left rust-protobuf unresolved, flipped blake3 and
hash3 between "no difference" and "significant", and pinned hashset's size
only to within a factor of three.

### `insts-retired` — a diagnostic

Native instructions retired. Quiet: differences of a fraction of a percent
are resolvable with 3-5 samples, and it is insensitive to host load.

Its blind spot is exactly the thing this analysis is about: **it cannot tell
a register-register move from a reload.** Both are one instruction; one
touches memory. Regicide's design trades reloads for register moves, so it
regularly runs **more instructions in less time**:

| benchmark (006) | insts-retired | cycles |
| --- | ---: | ---: |
| `sqlite3` | +5.96% | -3.8% to -4.6% |
| `shootout-keccak` | +8.75% | -1.2% to -1.3% |
| `blake3-scalar` | +5.23% | -0.2% to -0.3% |

and occasionally the reverse (005, hashset: -3.93% instructions, +5%
cycles, from reloads after calls). So it never ranks benchmarks and never
states a cost. What it is good for: **where it disagrees with cycles, there
is a moves-for-memory trade in that benchmark**, and the per-benchmark report
should find and quantify it (stack accesses, moves, estimated cycles).

### The policy

**Rank by `cycles` on every benchmark, from the start.** Every module
compiles in seconds, so there is no reason to screen with a cheaper metric:
the 14-benchmark suite at `-i 60` is a few minutes. The previous policy
(screen by instructions, confirm a subset with cycles) filed cycles wins as
regressions and spent its sample budget on benchmarks chosen by the wrong
metric.

```bash
# 1. The ranking (suite minus host-bound modules; SKILL.md §2).
./scripts/sightglass -m cycles -i 60 $BENCHES -- --pin \
    > $R/sightglass-data/cycles.csv

# 2. -i 90 on whatever scripts/cycles_robustness.py calls NOISY or MIXED,
#    on anything not significant whose estimators agree in sign, and on
#    anything you will build a finding on.
./scripts/sightglass -m cycles -i 90 <those> -- --pin \
    > $R/sightglass-data/cycles-confirm.csv

# 3. The diagnostic, same benchmarks.
./scripts/sightglass -i 5 $BENCHES > $R/sightglass-data/insts-retired.csv
```

If cycles does not resolve for a benchmark even at `-i 90`, **say its wall
time is inconclusive**, give the spread of the robust estimators, and leave
it out of the improved/regressed picks. Do not rank it by instructions
instead, and do not promote a noisy cycles number to a finding.

**Compile time is the exception** (SKILL.md §7). There, `insts-retired` is
the metric: compilation has no move-vs-memory trade to hide, so retired
instructions track compile wall time and are quiet enough for `-i 5`.

### Separate mechanism from magnitude

**The most expensive mistake available here** is writing a cost from
instruction counts. `insts-retired` is a good *mechanism detector*: where it
moves, something in the code changed. It is a **bad cost estimator** for a
register allocator, because the difference is usually register-register
moves, which is precisely its blind spot. The cost is the cycles number;
everything else is evidence for the mechanism.

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
points. Four cheap checks, all from the raw CSV, which
`scripts/cycles_robustness.py` prints for every benchmark at once:

```bash
python3 .claude/skills/analyze-regicide-codegen/scripts/cycles_robustness.py \
    $R/sightglass-data/cycles.csv $R/sightglass-data/cycles-confirm.csv
```

It reports mean, median, 20%-trimmed mean and minimum-to-minimum (negative =
regicide faster), each engine's CV, and a verdict: `robust` (all four agree
in sign), `MIXED` (some disagree), `NOISY` (an engine's CV over 5%).

This is what distinguished the real results from the noise in that session:

- `shootout-switch` +53.39%: held at every estimator down to **+36.64%**
  minimum-to-minimum, and backtracking's *slowest* sample beat regicide's
  *median*. Believable.
- `shootout-sieve` -7.38%: held at median -5.43%, trimmed -5.61%, minimum
  -6.08%. Believable *as a measurement* — but it did not survive an unrelated
  code-layout change (see above). Robust is not the same as attributable.
- `hashset` +7.02%: **1.8x spread inside one engine's own 30 samples** (CV
  13.9%). Not a result; reported as noise.
- 005, `blake3-scalar` +4.20%: median -0.43%, regicide CV 16.3% from one
  outlier sample. `NOISY`; re-measured at `-i 45`: -0.56% n.s.
- 005, `hashset` +5.19% / +4.20% (two runs): CV 4-7%, median and trimmed mean
  +3 to +5% in both; then +6.19% with 64-byte function alignment. Real, and
  not layout.
- 006, `hashset` -6.79% / -4.78% / -3.27% aligned: `NOISY` in all three
  (regicide CV 6-10%), estimators from -0.5% to -15.8%, but every one
  negative. Reported as "a win of uncertain size", which is the honest
  framing when the sign is robust and the size is not.
- 006, `rust-protobuf` -1.12% n.s. then +0.55% n.s. (`MIXED`: mean positive,
  median negative). Inconclusive on cycles; the instruction win stands
  alone.

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
In 005 both kinds appeared and were the run's headline: sqlite3 (+6.28%
insts, -5% cycles; 29% fewer stack accesses) and hashset (-3.93% insts, +5%
cycles; ~3,400 extra reloads each right after a call).
In 006 three of the five worst benchmarks by instructions were cycles wins
or parity: sqlite3 (+5.96% / -4%), keccak (+8.75% / -1.2%), blake3 (+5.23% /
-0.3%). Run cycles on *every* significant benchmark, not only the top `N`
each way, or a ranking by instructions files wins as regressions.

**Rule out layout** before attributing either to the allocator: re-run with
`-C cranelift-log2_min_function_alignment=6` passed to both engines (SKILL.md
§2 has the command). It shifts every function's placement and leaves
allocation alone.

### Timing outside sightglass

For comparisons sightglass cannot express (Wasmtime built at two regicide
revisions, an old report's `.cwasm` against today's), `scripts/time-cwasm`
interleaves runs and times the `bench.start`..`bench.end` span. It is a
bisection and diagnosis tool; the report's top line still comes from
sightglass. It is just as sensitive to host load: at load ~10 with 80
rounds it resolved ~2% steps, at load 30+ nothing.

**Never run builds of your own alongside any timing.** In 005 four scratch
`cargo build`s raised hashset's CV from ~5% to 29% and the run was discarded.

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
    -f ~/regalloc2/$R/sightglass-data/cycles.csv \
    -f ~/regalloc2/$R/sightglass-data/cycles-confirm.csv -i csv -o csv \
    > ~/regalloc2/$R/sightglass-data/cycles-effect-size.csv
```

Several `-f` files pool their samples, which is how a benchmark's `-i 60` and
`-i 90` runs combine into one verdict.

Output columns:

```
arch,wasm,phase,event,a_engine,a_engine_flags,a_mean,b_engine,b_engine_flags,
b_mean,significance_level,half_width_confidence_interval
```

Then rank it:

```bash
python3 ~/regalloc2/.claude/skills/analyze-regicide-codegen/scripts/rank_benchmarks.py \
    ~/regalloc2/$R/sightglass-data/cycles-effect-size.csv --top 5 --all
```

Its "confirm these with cycles" footer predates ranking by cycles; when the
input is already cycles, ignore it.

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

**Analysis 001 used the opposite sign** (positive = faster). Numbers from it
quoted in this skill have been converted. Its report directory, like 002-005,
has been deleted, so the quoted numbers are the only record.

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
| **Ranking execution** (every benchmark) | `-m cycles -i 60 -- --pin` | Wall time decides. Double the `-i 30` this skill used to use, which left three of ten benchmarks unresolved or flipping in 006. |
| **Confirming** (`NOISY`, `MIXED`, near-significant, or anything a finding rests on) | `-m cycles -i 90 -- --pin` | Pool with the `-i 60` samples in `effect-size`. |
| Execution diagnostic | `-i 5` (`insts-retired`) | Quiet; locates moves-for-memory trades. Never ranks. |
| **Compile time** (SKILL.md §7) | `-p compilation -i 5` (`insts-retired`) | Quiet, and a good proxy for compile wall time, since compilation has no move-vs-memory trade. |

`scripts/sightglass`'s own default for `-m cycles` is still `-i 30`, so
always pass `-i` explicitly.

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
    $R/sightglass-data/cycles-effect-size.csv \
    --baseline backtracking --candidate regicide
```

It applies the same significance test, ranks worst-first, and exits nonzero if
any benchmark regressed significantly. Note that its framing is a *gate* on a
change — for this skill a regression is the subject of the analysis rather than
a failure, so the exit code is informational. `rank_benchmarks.py` is the better
tool for choosing what to analyze.

---

## Compile time decides what you can measure

Sightglass compiles the module once per measured process, so a module regicide
compiles slowly multiplies straight into the run: in 002, tinygo-regex took
~40 minutes per compile, so a `-i 5` run spent ~80 minutes on it alone and a
`-i 30` cycles run would have taken ~7 hours. **At regicide `cf70ddd` (006)
every module analyzed compiled in 3-9 s**, so `-i 60` and `-i 90` cycles runs
are affordable. Keep an eye on §1/§4 disassembly times anyway: a module that
suddenly takes minutes is a scheduling problem. Do not analyze it, though:
compile time is tabulated in SKILL.md §7, not explained.

## Bookkeeping

Keep every raw CSV under `$R/sightglass-data/`, named for what it is
(`cycles.csv`, `cycles-confirm.csv`, `insts-retired.csv`,
`compile-insts-retired.csv`), so every number in the
report can be re-derived without re-measuring.

Record in `$R/misc/measurement-notes.md`: host OS and arch, date, the suite, the
metrics used, the `-i` values, whether `--pin` was on, and anything you learn
about noise on this machine. The next invocation of this skill starts from your
notes.

# Report templates

Copy these and fill them in. Two rules that matter more than the layout:

1. **Write continuously.** Create each file as a skeleton when you start the
   benchmark and fill it in as you go. Do not hold findings in your context and
   write everything at the end — others monitor progress through these files,
   and a shutdown must not cost the analysis.
2. **Date-stamp everything.** `YYYY-MM-DD, HH:MM`, from
   `date '+%Y-%m-%d, %H:%M'`.

Line-wrap markdown at about 80 columns.

---

## `report.md` — the aggregate report

This is the file the user actually reads. Everything else is evidence for it.

```markdown
# regicide codegen analysis NNN

**2026-09-30, 14:22** · host: macOS arm64 · suite: `benchmarks/pca.suite` ·
N = 5

regicide (candidate) vs regalloc2 backtracking/ion (baseline). Counts are
cycles or instructions, so lower is faster. Deltas are (regicide - ion) / ion:
**negative means regicide is faster** (-12% = 12% fewer), positive means
slower.

## Top line

Wall time (sightglass `cycles`, `-i 60` on all 14, `-i 90` on 5) decides.
Instructions retired is shown beside it as a diagnostic only.

| | **cycles** | insts-retired (diagnostic) |
| --- | --- | --- |
| geomean | **-0.8%** | +2.3% |
| significantly faster | **5** | 3 |
| significantly slower | **2** | 7 |
| no difference / inconclusive | 7 | 4 |

<!-- Name every benchmark where the two disagree in direction, and the trade
     behind it, in a sentence: "regicide runs more instructions but less time
     on sqlite3, keccak and blake3: fewer stack accesses, more register
     moves." Never headline the instruction column. Never quote `Sum Total`. -->

Raw data: `sightglass-data/`. Commands: `misc/commands.md`.

## Static allocation quality, suite-wide

From `run -q` over the whole-suite corpus (`corpus/`, required; SKILL.md §3):
`misc/corpus-counts.txt` and `misc/corpus-counts-by-module.txt`, 17 modules
/ 6,416 functions. These are the numbers a change in step 3 should move.

| | suite total |
| --- | ---: |
| reg-reg moves (of which cold) | 210,177 (8,054) |
| spills | 23,656 |
| reloads | 71,911 |
| remats | 21,862 |
| spill slots | 8,658 |
| estimated cycles | 103,351,088 |
| `coalesce-out-of-fuel` | 0 |

Then one row per module, all 17, same columns.

<!-- There is no `run.rs` for regalloc2's backtracking allocator, so this column
     stands alone: it is a baseline for future regicide runs, not a comparison.
     Say so rather than leaving a reader to assume a comparison. -->


## Every benchmark with a significant difference in wall time

Largest slowdown first, by cycles. The instruction column is a diagnostic.

| benchmark | cycles | robust? | insts-retired | analyzed |
| --- | ---: | --- | ---: | --- |
| `tinygo/tinygo-regex` | **+21.4%** | yes | +17.0% | [regressions/tinygo-regex.md](regressions/tinygo-regex.md) |
| … | | | | |
| `sqlite3` | **-4.2%** | yes | +6.0% (more insts, less time) | [improvements/sqlite3.md](improvements/sqlite3.md) |

## Top 5 things to fix

Ranked by breadth × heat, not by how interesting the bug is: an issue in several
benchmarks beats a one-off, and an issue in the hottest block beats one in a
block that barely sampled.

### 1. Eviction cost ignores loop depth

- **What:** the eviction cost model prices a spill once rather than
  once-per-iteration, so a value used in a deep loop loses its register to a
  value used once outside it.
- **Where:** `regicide/src/region_allocation.rs`, the eviction cost comparison.
- **Seen in:** `bz2` (hottest block, 41% of samples), `regex` (2 blocks),
  `blake3-scalar` (hottest block). **3 of 5 benchmarks analyzed.**
- **Cost where measured:** +3.1% cycles on `bz2` (wall time, `-i 60`,
  robust); +8 estimated cycles in its innermost loop body.
- **Evidence:** [regressions/blake3-scalar.md](regressions/blake3-scalar.md) §Block 3.

### 2. …

## Top 5 things we do well and must preserve

Wins get recorded precisely because the next change can silently undo them.

### 1. Spill hoisting out of loops

- **What:** regicide hoists a spill to the loop preheader where backtracking
  leaves it in the body.
- **Why it works:** <!-- the mechanism, so a future change can be checked
  against it -->
- **Seen in:** `hashset` (-12.06%, hottest block), `bz2` (block 7).
- **How a change could break it:** <!-- what to watch for -->
- **Evidence:** [improvements/hashset.md](improvements/hashset.md) §Block 2.

## Caveats

<!-- Be explicit. Anything the reader would otherwise have to guess:
     - benchmarks whose wall time stayed inconclusive at -i 90, with the
       estimators' spread
     - benchmarks skipped and why
     - functions where fewer than 3 hot functions/blocks existed
     - anything that looked wrong and was not run to ground -->

## Compile time vs ion

<!-- Last section, written after everything above (SKILL.md §7). A table and
     a pointer to the raw data. No analysis, no profiles, no explanations. -->

Instructions retired in sightglass's compilation phase (`-p compilation
-m insts-retired -i 5`), whole `pca.suite`, same Wasmtime build for both
allocators. Lower is faster; delta is (regicide - ion) / ion. Raw data:
[`sightglass-data/compile-insts-retired.csv`](sightglass-data/compile-insts-retired.csv);
ranking: [`sightglass-data/compile-ranking.txt`](sightglass-data/compile-ranking.txt).

geomean **+531%**; significantly slower 17 of 17, faster 0.

| benchmark | ion | regicide | delta | significant |
| --- | ---: | ---: | ---: | --- |
| `shootout/shootout-switch` | 3,807,357,489 | 414,129,598,658 | +10,777% | yes |
| `tinygo/tinygo-regex` | 4,191,930,551 | 73,746,094,534 | +1,659% | yes |
| … | | | | |
```

---

## `improvements/<benchmark>.md` and `regressions/<benchmark>.md`

One per benchmark, same shape for both directions.

Four rules specific to these files:

1. **Title with the cycles delta.** When instructions disagree in
   direction, name both: `-4.2% cycles despite +6.0% instructions`. A title
   quoting only the instruction delta misdescribes the benchmark.
2. **Severity comes only from cycles.** Write the mechanism — what differs,
   why the allocator did it, what in its design led there — with
   instruction, move, stack-access and estimated-cycle counts as evidence.
   Every "how bad is it" sentence quotes the cycles result. If cycles was
   inconclusive, say so; do not write a severity from instructions.
3. **Every delta uses the sign convention** (SKILL.md): `(variant - baseline)
   / baseline`, negative = faster. When a table compares two runs or two
   regicide revisions rather than ion vs regicide, name the baseline in the
   column header (e.g. `moves, 630ca5e → 2a56dbe`).
4. **Say whether the hot code changed since the last analyzed regicide
   revision**, near the top, with this run's evidence: `asm_tools.py same`
   on `$R/bisect/` builds made with `scripts/build-at` at that revision and
   at HEAD. **Do not link or quote an earlier report**; reports are
   standalone (SKILL.md, Invariants). If the hot code is unchanged, say so
   and keep the analysis short, but make it self-contained: the mechanism in
   a paragraph, with this run's disassembly and counts. Spend the rest of
   the file on what *is* new: the measurement, and any function that did
   change. A benchmark whose hot code is identical but whose cycles moved is
   usually the most informative report of the run (hashset in 005: the
   cause was a cold-looking driver function). Two benchmarks with the same
   mechanism and verdict can share one file.

File each benchmark by its **cycles** result: `regressions/` if regicide is
significantly slower in wall time, `improvements/` if faster. A benchmark that
runs more instructions in less time is an improvement; its file should explain
the trade.

````markdown
# <benchmark> — regicide <-9.1%> cycles

**2026-09-30, 15:04**

| metric | backtracking | regicide | delta | significant |
| --- | --- | --- | --- | --- |
| **cycles** (`-i 60` + `-i 90`, pooled) | 412,003 | 374,551 | **-9.1%** | yes, robust |
| insts-retired (diagnostic, `-i 5`) | 900,271 | 791,710 | -12.06% | yes |

<!-- When cycles resolved and disagrees in direction or magnitude, say so here
     in a sentence, with the robust estimators (median / trimmed / minimum) so
     a reader can judge it. See references/measurement.md. -->

Reproduce the compile with logging:

```bash
cd ~/regalloc2
WASMTIME_LOG=regicide::region_allocation=trace ./scripts/log \
    ~/sightglass/benchmarks/hashset/benchmark.wasm 2> /tmp/hashset.log
```

Profiles: `../profiles/hashset-regicide.txt`,
`../profiles/hashset-backtracking.txt`.

Render the HTML visualizations of regicide's allocations for this benchmark's
hot functions. The `-f` filter is a case-insensitive POSIX extended regex over
*capture* names, which are the corpus placeholders `u0-N` (work out `N` from
the import offset), so one `|`-joined invocation covers all of them at once:

```bash
cd ~/regalloc2
./scripts/visualize -f '^u0-(<N1>|<N2>|<N3>)$' \
    ~/sightglass/benchmarks/hashset/benchmark.wasm \
    reports/codegen-analysis-NNN/viz/hashset/
```

Which page is which function:

| hot function | visualization |
| --- | --- |
| `<hot-fn-1>` | [`41287-0012-<hot-fn-1>.html`](../viz/hashset/41287-0012-<hot-fn-1>.html) |
| `<hot-fn-2>` | [`41287-0031-<hot-fn-2>.html`](../viz/hashset/41287-0031-<hot-fn-2>.html) |
| `<hot-fn-3>` | render failed, see `viz/hashset/41287-0044-<hot-fn-3>.log` |

<!-- This table is not optional and cannot be derived from the command. The
     file names are `<pid>-<counter>-<function>.html`, and the pid is the
     compiling process's, so nobody can predict them — take them from
     `visualize`'s own stderr listing (it echoes each path it writes) or by
     `ls`-ing the output directory, and record the mapping while you have it.
     A function whose render failed leaves a `.log` beside it and no `.html`;
     say that in the row rather than linking a page that is not there.
     `visualize` is regicide-only, so these pages show what regicide did, not a
     side-by-side — see references/tools.md. -->

## Hot functions

Top 3 by sample share, from `scripts/profile -t`. <!-- If the benchmark has
fewer than 3 hot functions, say so and stop; do not pad with cold code. -->

### 1. `<function>` — 52% of samples

Replay its allocation:

```bash
cd ~/regalloc2/regicide
RUST_LOG=trace cargo run --all-features --example run -- \
    ../reports/codegen-analysis-NNN/corpus/hashset-0012-<function>.postcard
```

Disassemble it under both allocators:

```bash
cd ~/regalloc2
./scripts/disassemble -O regalloc-algorithm=regicide \
    ~/sightglass/benchmarks/hashset/benchmark.wasm -- --filter <function> --addresses
./scripts/disassemble -O regalloc-algorithm=backtracking \
    ~/sightglass/benchmarks/hashset/benchmark.wasm -- --filter <function> --addresses
```

Visualization of its allocation — live ranges, splits and evictions:
[`viz/hashset/41287-0012-<hot-fn-1>.html`](../viz/hashset/41287-0012-<hot-fn-1>.html).
<!-- The page for this function out of the set rendered above. Name the file
     even though the benchmark-level table already lists it: a reader who jumps
     straight to a function should not have to scroll back up. -->

Per-function allocation counts from `run.rs` (regicide only — there is no
corpus replay for the backtracking allocator, see `references/tools.md`):

| | regicide |
| --- | --- |
| moves | 9 |
| spills | 1 |
| reloads | 2 |
| remats | 3 |
| critical-edges | 2 |
| slots | 3 |

#### Block 2 — 31% of samples, innermost loop

**Disassembly** (regicide left, backtracking right, or two fenced blocks):

```
<the actual instructions — quote them, so a reader who disagrees with the
 reasoning can still use the evidence>
```

**Estimated cycles**, regicide's cost model at weight 1 (register operand,
reg-reg move or remat = 1; stack operand, spill or reload = 5;
`scripts/estimated_cycles.py`):

| | backtracking | regicide |
| --- | --- | --- |
| register operands | 14 | 13 |
| reg-reg moves | 2 | 1 |
| remats | 0 | 0 |
| stack operands | 0 | 0 |
| spills + reloads | 2 | 0 |
| **estimated cycles** | **26** | **14** |

**Level 1 — what differs.** …

**Level 2 — why the allocator did it.** … <!-- cite the log events -->

**Level 3 — what about regicide's design led there.** … <!-- name the module and
the heuristic -->

**Level 4 — fix or preserve.** … <!-- and how broadly it applies -->

#### Block 5 — 11% of samples

…

### 2. `<function>` — 18% of samples

…
````

---

## `misc/` — keep this populated

Not optional scratch space; it is where the next invocation of this skill starts
from. Worth writing as you go:

| File | Holds |
| --- | --- |
| `commands.md` | Every command you actually ran, in order. Makes the whole analysis reproducible and is nearly free to maintain. |
| `measurement-notes.md` | Host, date, suite, metrics, `-i` values, `--pin`, observed noise. |
| `preflight.md` | The §1 check that the two allocators really differ, and its result. |
| `environment.md` | Tool gotchas, build times, anything that cost you time and would cost the next person the same. |
| `skill-feedback.md` | What this skill got wrong, omitted, or should say differently. Read it when next updating the skill. |
| `dead-ends.md` | What you investigated that went nowhere, so nobody repeats it. Include explanations you *ruled out*, not just paths you abandoned — "splits do not lose hints, here is the code" is as valuable as a positive finding. |

**When the analysis is done, fold the durable conclusions into
`references/known-findings.md` in the skill itself** — the mechanisms, their
status (confirmed in code / source-only / hypothesis), what was ruled out, and
the open questions. `misc/` is the evidence for *this* run; `known-findings.md`
is what the next run should not have to rediscover. Keep the per-run files as
the citations it points at.

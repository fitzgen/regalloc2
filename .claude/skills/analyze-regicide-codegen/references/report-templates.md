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
instructions or cycles, so lower is faster; a positive delta means regicide is
faster.

## Top line

| | insts-retired | cycles |
| --- | --- | --- |
| geomean | +0.54% | inconclusive |
| significantly faster | 4 | 1 |
| significantly slower | 3 | 0 |
| no difference | 34 | 40 |
| `Sum Total` | +0.79% (significant) | +0.3% (not significant) |

Ranking is by `insts-retired`, which resolves; cycles was run at `-i 30` on the
ten benchmarks below and resolved only `hashset`. <!-- Say this plainly rather
than implying cycles backed the whole ranking. -->

Raw data: `sightglass-data/`. Commands: `misc/commands.md`.

## Static allocation quality, suite-wide

From `run.rs` over the whole-suite corpus (`misc/corpus-counts.txt`), 17
modules / 4,812 functions. These are the numbers a change in step 3 should move.

| | regicide |
| --- | --- |
| reg-reg moves | 38,114 |
| spills | 9,220 |
| reloads | 12,678 |
| remats | 1,904 |
| critical edges | 3,355 |
| spill slots | 14,702 |

<!-- There is no `run.rs` for regalloc2's backtracking allocator, so this column
     stands alone: it is a baseline for future regicide runs, not a comparison.
     Say so rather than leaving a reader to assume a comparison. -->


## Every benchmark with a significant difference

Largest slowdown first.

| benchmark | insts-retired | cycles | analyzed |
| --- | --- | --- | --- |
| `blake3-scalar/benchmark` | -10.93% | n.s. | [regressions/blake3-scalar.md](regressions/blake3-scalar.md) |
| `quicksort/benchmark` | -6.13% | n.s. | [regressions/quicksort.md](regressions/quicksort.md) |
| … | | | |
| `hashset/benchmark` | +12.06% | +9.1% | [improvements/hashset.md](improvements/hashset.md) |

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
- **Cost where measured:** +18 virtual cycles in `bz2`'s innermost loop body.
- **Evidence:** [regressions/blake3-scalar.md](regressions/blake3-scalar.md) §Block 3.

### 2. …

## Top 5 things we do well and must preserve

Wins get recorded precisely because the next change can silently undo them.

### 1. Spill hoisting out of loops

- **What:** regicide hoists a spill to the loop preheader where backtracking
  leaves it in the body.
- **Why it works:** <!-- the mechanism, so a future change can be checked
  against it -->
- **Seen in:** `hashset` (+12.06%, hottest block), `bz2` (block 7).
- **How a change could break it:** <!-- what to watch for -->
- **Evidence:** [improvements/hashset.md](improvements/hashset.md) §Block 2.

## Caveats

<!-- Be explicit. Anything the reader would otherwise have to guess:
     - cycles inconclusive on N of M benchmarks
     - benchmarks skipped and why
     - functions where fewer than 3 hot functions/blocks existed
     - anything that looked wrong and was not run to ground -->
```

---

## `improvements/<benchmark>.md` and `regressions/<benchmark>.md`

One per benchmark, same shape for both directions.

Two rules specific to these files:

1. **Name both metrics in the title when they disagree**, e.g.
   `-2.23% instructions but +7.38% cycles`. A title quoting only the
   instruction delta misdescribes the benchmark.
2. **If the `cycles` row is still pending, the prose must not characterize
   severity.** Write the mechanism — what differs, why the allocator did it,
   what in its design led there — and leave the "how bad is it" sentence until
   cycles lands. In one session three per-benchmark reports had to be revised
   because severity was written from instruction counts.

A benchmark can also belong in `regressions/` by instruction count while being
a *win* on cycles. Keep it where the ranking put it, and say so plainly at the
top rather than silently filing it under the other direction.

```markdown
# <benchmark> — regicide <+12.06% / -10.93%>

**2026-09-30, 15:04**

| metric | backtracking | regicide | delta | significant |
| --- | --- | --- | --- | --- |
| insts-retired | 900,271 | 791,710 | +12.06% | yes |
| cycles (`-i 30`) | 412,003 | 374,551 | +9.1% | yes |

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

**Virtual cycles** (weights: register operand or reg-reg move = 1; stack operand
or stack-touching move = 10):

| | backtracking | regicide |
| --- | --- | --- |
| register operands | 14 | 13 |
| reg-reg moves | 2 | 1 |
| stack operands | 0 | 0 |
| spills + reloads | 2 | 0 |
| **virtual cycles** | **36** | **14** |

**Level 1 — what differs.** …

**Level 2 — why the allocator did it.** … <!-- cite the log events -->

**Level 3 — what about regicide's design led there.** … <!-- name the module and
the heuristic -->

**Level 4 — fix or preserve.** … <!-- and how broadly it applies -->

#### Block 5 — 11% of samples

…

### 2. `<function>` — 18% of samples

…
```

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

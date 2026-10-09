# What we already know about regicide's codegen

Findings from previous runs of this skill, with the evidence that pins them and
their status. **Read this before analyzing.** It saves rediscovering what is
already established, and it records the wrong turns previous runs actually
took.

Each item says how it was established, so you can tell a measured fact from a
hypothesis. Verify anything load-bearing still holds: regicide changes, and a
finding here reflects the code as it was when written.

All percentages follow the skill's sign convention: **negative = regicide
faster, positive = regicide slower**, relative to ion.

*Last updated 2026-10-09, from `reports/codegen-analysis-006/` (2026-10-08,
regicide `cf70ddd`). Older report directories (001-005) were deleted at the
user's request. This file is now the only record of what they found, and
their numbers below are quoted, not re-checkable. All analyses: `pca.suite`
minus three host-bound modules, macOS arm64.*

The user's step-3 sessions keep their own record in
`regicide/.claude/skills/optimize-regicide-codegen/references/known-findings.md`:
what was tried against these findings, what landed, what was dropped. Read
it too; it is the authority on in-flight and abandoned fixes.

**Reports must be standalone.** The user asked (2026-10-08) that a new
report not reference other reports: it cites only its own measurements, plus
regicide commits and builds it made itself (`bisect/`). Use this file to
*know* history; do not link old reports from a new one.

---

## 0. Where things stand (2026-10-08, analysis 006, regicide `cf70ddd`)

**On cycles regicide is behind on two benchmarks, and only one by much.**

| benchmark | insts-retired | cycles (006) | mechanism | cycles in 005 (`288da68`) |
| --- | ---: | --- | --- | ---: |
| `tinygo-regex` | +16.97% | **+21.39%** robust, survives 64-B alignment | §8 slot promises broken at big joins; §11 loop-exit stubs | +19.1% |
| `libsodium-siphashx24` | +4.09% | **+1.25%** robust, survives alignment | §9 `x19/x20` save/restore per call | +1.4% to +2.3% |
| `libsodium-hash3` | +0.64% | +0.03% n.s. / -0.27% | parity | — |
| `libsodium-core2` | +0.21% | -0.14% n.s. | parity | — |
| `blake3-scalar` | +5.23% | -0.21% n.s. / **-0.31%** | §5 straight-line split seams; no cycles cost | -0.6% n.s. |
| `rust-protobuf` | -1.90% | -1.12% n.s. / +0.55% n.s. (MIXED) | §10 whole-function coalescing | -2.4% |
| `kotlin-richards` | -1.73% | **-1.13%** (median -0.64%) | §10 loop-carried and call-crossing values in registers | -2.2% |
| `shootout-keccak` | +8.75% | **-1.17% / -1.30%** | §5 remat and tight slots beat ion's store-reload pairs | -0.8% n.s. |
| `sqlite3` | +5.96% | **-3.79% / -4.62%** | §10 heap base and `vmctx` in callee-saved registers | -4.4% to -5.6% |
| `hashset` | -4.30% | **-6.79% / -4.78% / -3.27% aligned** (NOISY; win of uncertain size) | §9 now on the right side, after `cf70ddd` | **+4.2% to +6.2%** |

`insts-retired` geomean over the 14: +19.49% (002) → +5.63% (003) → +2.38%
(005) → **+2.27%** (006). Four benchmarks (sieve, hex-simd, heapsort, switch)
are within ±0.14% instructions and were not timed.

**What moved between 005 and 006** (regicide `288da68` → `cf70ddd`, located by
`scripts/build-at` at both revisions + `asm_tools.py same`):

- **hashset flipped from +5% to a win.** The material change is `cf70ddd`
  ("let clobber-bound heavy bundles evict a lighter bundle even when it would
  break a hint"). Replaying the driver `benchmark()` (`hashset-0004-u0-3`) at
  `dfe762b` (its parent) gives 12,558 reloads; at `cf70ddd`, **3,146**. Stack
  loads per call: regicide 0.28, ion 1.53. Its four hot `HashTable` functions
  are at parity with ion and did not change. This is the §9 hashset symptom
  below, now fixed.
- **tinygo-regex: slot-to-slot copies 7,157 → 1,964 module-wide** (ion: 608)
  after `3c6c16c` (block-param slot classes); stack accesses 62,893 → 47,029
  (ion 29,095). Cycles did not move (+19% → +21%, within run-to-run
  variation), so the remaining mechanism matters more than the copies
  removed. See §8.
- **`1db4308` demanded-move hoisting:** a small, uniform gain (§12).
- keccak's and siphashx24's hot functions are unchanged; their cycles moves
  are measurement, not code.

## 1. Hints — largely fixed by the 2026-10-05 commits

001 found that regicide built register hints and then mostly did not honor
them. Regicide `1dd7a91`..`bcbdf05` (cross-region hints name the child's
*actual* register; union of hints kept beside the intersection; start/end-of-
span preference; block-param slot sharing; hinted bundles ranked first)
addressed it. Replaying 001's inputs through `630ca5e` and `2a56dbe` gave
moves -19% to -28% per corpus (heapsort `__original_main` 218 → 95), with
spills, reloads and slots flat.

**How to measure the hint-honor rate.** `BundleAssignedNonPreferred` is
emitted for *every* non-preferred assignment, including bundles with no hint at
all (its `preference` field is then `FixedEntitySet {}`). Count only hinted
bundles:

```
honor rate = Preferred / (Preferred + NonPreferred with non-empty preference)
```

001 divided by all `NonPreferred`, which **overstated the problem ~2x**.
After the commits: ~65% suite-wide. 006: 73% in tinygo `countMatches`, 53% in
`machine.add`.

**What remains:** most non-preferred assignments are of *unhinted* bundles.
In straight-line code (keccak 3,237 of 4,560 non-preferred assignments
unhinted; blake3 796 of 904) that is §5's seam-move source.

Already ruled out (001), still true: splits do not lose hints
(`region_allocation.rs` re-applies the parent's preference on every split
path).

## 2. `MAX_MERGE_SPANS` reported a false `Overlap` — FIXED by `efb70f6`

A disjointness walk capped at 200 steps returned "overlaps" for legal merges.
On `shootout-switch` (a ~1,000-arm `br_table`) it produced 109,350
`CoalesceRefused` and 75,737 moves vs ion's 61: **+48.93% cycles** (002).
`efb70f6` replaced it with treap-based disjoint interval sets; switch went to
109 moves and -0.62% n.s.

A fuel limit remains, with its own `OutOfFuel` reason; `run` prints
`coalesce-out-of-fuel`, and it was **0 in every corpus in 003, 005 and 006**.
If it is ever nonzero, this section applies again.

**The signature to look for:** a `CoalesceRefused` count comparable to or
above `SpansCoalesced`, all `Overlap`.

**Correction to 001:** 001 attributed this regression to region boundaries and
dropped hints. The event log contradicted that. Derive mechanisms from event
counts, not by analogy.

## 3. Between-span splitting always cut at index 1 — LARGELY DEFUSED

`best_split_between_spans` kept a cut only if strictly better, scanning in
program order, and `Assignments::interference_weight` charges a conflicting
bundle's whole weight to any half that touches it, so every cut tied and
`k = 1` won: long bundles were peeled one span at a time. 96–99% of splits
were at index 1 in every corpus (002), and the peel loop was why
tinygo-regex once took 40 minutes to compile.

- `a183083`/`1f98a23`: ties broken at the middle of the first tied run.
- **`7d1217e`**: a bundle with no uses and an `Any` constraint goes to the
  stack whole instead of being split (`has_nothing_to_split_for`). Took
  tinygo from +48% to +19% cycles.

006, `countMatches`: 2,558 between-span splits, index histogram spread (210
at 1, 190 at 2, 157 at 3, …); 132,509 merges, 296 refusals. Healthy. The
`interference_weight` blindness to cut position is still there for bundles
with uses. Do not report §3 as a mechanism without checking
`BundleSplitBetweenSpans` and its `index:` histogram on the current build.

## 4. Cranelift supplies cold-block frequencies and constant remats

Wasmtime `00a6476`: `block_frequency` returns `COLD` for blocks Cranelift sinks
to the end of the function, `NORMAL` otherwise; there is **still no
loop-depth frequency**. `is_rematerializable` returns `CheaperThanReload` for
constant definitions (`f9602d1` added `CheaperThanMove`).

- **Coalescing never merges across a cold edge**, so some moves land in cold
  blocks on purpose. `run` prints an `of which cold:` line and an
  `estimated-cycles` line that weights cold blocks 0.05x; use them before
  calling a rise in total `moves` a regression.
- **Cold blocks are their own regions** (`f12e432`), allocated last; spill
  placement never climbs into a more frequent sole predecessor.
- **Loop weighting is regicide's own:** `block_weight` (in
  `demanded_moves.rs` and `spill_slot_allocation/slot_classes.rs`) is 10× per
  loop level, i.e. it assumes ten iterations per loop entry. §11 is where
  that assumption bites.

## 5. Register-starved straight-line code: more moves, no cycles cost — PRESERVE the trade

`keccak_core` and blake3 `compress_in_place` are each **one fully unrolled
block** (the only branch is the stack-limit check), so static counts equal
dynamic cost and there is no block ranking to make. Neither changed between
`288da68` and `cf70ddd`.

**Mechanism (006, both):** one block means every bundle is one span: 0
between-span splits and little coalescing (20 and 4 merges). Values that do
not fit are split within the span and then into per-use minimal pieces
(`split_into_minimal_or_spill`); minimal pieces get registers first
(`Priority`), and spill weight is use density, so sparse long-lived values
lose. Neighbouring minimal pieces are mostly unhinted, so they land in
different registers and a move joins them. `SplitScore` does not price that
move.

| `keccak_core` | ion | regicide |
| --- | ---: | ---: |
| insts | 6,201 | 6,743 |
| reg-reg moves | 100 | 400 (scattered singly, never in runs) |
| constant remats | — | +190 (ion reloads one mask 59 times) |
| stack accesses | 1,811 | 1,863 (+174 stores, -122 reloads) |
| store-then-reload within 4 insts | 33 | **0** |
| frame | 768 B, 94 slots | **336 B, 41 slots** |
| cycles | | **-1.2%** |

**The lesson:** +8.75% instructions, +3.6% estimated cycles, and still a
cycles win. Estimated cycles get the direction wrong here too: ion's
store-then-quick-reload pairs stall store-to-load forwarding, and regicide's
reloads sit one instruction ahead of their use. blake3 (+5.23% insts, +40
stack accesses in `compress_in_place`) is parity for the same reason: the
block is bound by `add→eor→ror` dependency chains, not loads.

**Fix, low priority:** a move term in `SplitScore`, or hint each minimal
piece towards its neighbour's register. Worth doing only if it keeps remat,
spill-at-def/reload-at-use and tight slot packing.

## 6. Measurement lessons specific to this codebase

- **Instruction counts are a bad severity estimate, in both directions.**
  ~10x overstatement for move-heavy regressions (heapsort, keccak, sqlite3 in
  001/002); ~4–5x where fan-out multiplied moves (switch). In 006, three of
  the five worst benchmarks by instructions were cycles **wins** or parity
  (sqlite3, keccak, blake3). Never write severity from `insts-retired`.
- **The direction can flip with the metric, and that is the result.**
  sqlite3 +6% insts / -4% cycles (fewer stack accesses, more moves) in both
  005 and 006; hashset -3.9% insts / +5% cycles in 005 (reloads on call
  paths).
- **A noisy-host result can be a quiet-host result's sign.** siphashx24 was
  +0.73% n.s. in 003 on a loaded host and +1.25-2.31% significant in 005 and
  006 on byte-identical code. Re-measure before calling a small delta new.
- **hashset is noisy on this host:** regicide CV 6-10% in 006 across three
  runs; the win's size ranged -0.5% to -15.8% by estimator. Quote it as "a
  win of a few percent".
- **Layout control that works:** `-C cranelift-log2_min_function_alignment=6`
  on both engines. 006: tinygo (+21.9% median) and siphashx24 (+1.07%)
  survive it.
- **sieve's -7.38% cycles "win" (001) was most likely layout:** an identical
  7-instruction loop, moved across a 64-byte boundary, did not reproduce
  (-1.39% n.s., 002).
- **Profile self-time deltas on unchanged or better code are common**, and
  were not explained in 006: tinygo `MatchRunePos` +0.16 samples/run with 40
  fewer moves than ion; rust-protobuf `dlmalloc` +14-24% on an equal fast
  path; hashset `HashTable::add` -13% at parity; sqlite3 `function[401]` -69%
  on 5 fewer instructions per call; kotlin `function[60]` +0.26/run on
  strictly better code. Do not build a finding on a self-time delta without a
  code difference that explains its size. macOS has no block-level heat.
- **Move elimination is not a limiter.** Both passes default to `Global`; a
  surviving move was created by assignment, splitting or coalescing.
- **`critical-edges` is 0 by default.** Wasmtime's
  `regalloc_lazy_critical_edges` (default off) was measured neutral to
  slightly negative in 004 (+0.60% insts geomean).

## 7. The suite itself distorts the picture

~98% of `pca.suite`'s execution cost is in three benchmarks with no
meaningful register-allocated hot code: `splay` (~96% host GC),
`shootout-memmove` (~99% host memcpy), `cm-online-stats` (a multi-component
composition). Drop them from the execution runs (`SKILL.md` §2). Never quote
`Sum Total`.

`kotlin-richards` is a component wrapping one WASIp1 core module;
`scripts/profile` extracts and profiles that module. About 30% of its guest
time is Wasmtime host code for `call_ref`, equal under both allocators.
`hex-simd` is 86% WASI `fd_write`: host-bound. blake3's second-hottest frame
(`0x2510`, ~9-11%) is host WASI I/O.

## 8. Block parameters in a different slot from their arguments — the tinygo-regex regression, PARTLY FIXED

At a join with many predecessors, a stack-resident block parameter in a
different slot from an incoming argument costs that edge a stack-to-stack copy
(`ldr xN,[sp,#a]; str xN,[sp,#b]`). Go's lowering threads many live values
through big joins as block parameters, so **this is a tinygo-shaped problem**.

History: `7d1217e` put more zero-use values on the stack whole, and
`countMatches`' copies rose 710 → 2,176 (005). **`3c6c16c` (2026-10-07)**
added slot classes (`spill_slot_allocation/slot_classes.rs`): parameters are
unioned with their arguments wherever reservations do not overlap, heaviest
edges first, and the first member colored *promises* the class a slot.
Module-wide copies 7,157 → 1,964; sqlite3 209 → 70.

**What remains (006):** `countMatches` has 641 copies (ion 49), mostly on
edges into three joins with 74, 32 and 24 predecessors. Of 395 mismatched
argument/parameter pairs there, **381 are in the same slot class**. The
classes form, but the promise breaks: 29 of 56 multi-member classes are
spread over 2–7 slots, and 394 values take another class's slot (319 from
other classes, 75 from no class).

**Where:** `spill_slot_allocation.rs`, `alloc_class_slot`.
`slot_classes.rs` `least_promised` hands a slot already promised to one class
to a value outside it. A member that then finds its class's slot taken falls
back to `prefer`. **Fix:** keep promised slots out of `least_promised` for
outsiders, at the cost of a few frame slots. Bound: at most the 381 copies.
How much of tinygo's +21% that is was **not measured**; a scratch build of
`alloc_class_slot` timed with `build-at` + `time-cwasm` would tell.

**Signature:** `asm_tools.py slot2slot` (or `Reload{stackA, rX}` immediately
followed by `Spill{rX, stackB}` in `run`'s printout), with event counts that
otherwise look healthy.

## 9. Callee-saved registers: no cost model (003, 005, 006)

Regicide has no notion of what a callee-saved register costs (a save/restore
pair per call of the function) or saves (no reload after each call it
spans). `Machine` has only a register order; it does not know which registers
are callee-saved.

- **siphashx24, hot function `[48]` (`u0-34`, 93% of samples), still
  open:** `order_non_preferred_candidates` (`region_allocation.rs`) sorts by
  `(!region_clobbers.contains, region_preferences.contains)` above the class
  order. Registers no bundle in the region prefers therefore come before
  caller-saved registers another bundle hinted, so unhinted bundles take
  `x19`/`x20` where ion uses none. The loop body is identical to ion's (34
  instructions, no moves, no stack). Per call regicide adds an
  `stp`/`ldp` pair, a 5-instruction stack-limit check and 8 entry/exit moves:
  19 memory ops per call vs ion's 15. **+1.25% cycles** (006). Code unchanged
  since 003.
  - Probe (006): rank class index >= 16 after `region_preferences`. That
    drops `x19`/`x20`, adds 25 moves in the caller, passes `--check`, and
    moves corpus totals by +26 moves. The cut-off at 16 is hard-coded for
    aarch64, so it is a probe only. 001's footprint-based proxy regressed
    keccak and blake3 and was dropped (step-3 notes).
- **hashset driver `benchmark()`: FIXED by `cf70ddd`.** A base pointer
  (3,419 uses, live across thousands of calls) had lost callee-saved `x20`
  and was reloaded after each `bl` (+4-6% cycles in 005). `cf70ddd` lets
  clobber-bound heavy bundles evict hinted bundles that are much lighter,
  and keys are rematerialized as immediates. Reloads 12,558 → 3,146. Guard:
  `hashset-0004-u0-3` reloads ≈ 3,146.
- **Small instances (006):** `HashTable::add` saves one extra pair (`x25`);
  sqlite3 `function[401]` saves all five callee-saved registers though only
  its cold path uses them; blake3 `hash_many` reloads one argument after
  each of 16 calls instead of keeping it in a callee-saved register.
- **The counterweight:** §10's biggest wins are callee-saved registers held
  across calls. A fix must **price** the save/restore once per function, not
  avoid callee-saved registers.

## 10. What regicide does better than ion — PRESERVE

- **Loop-carried values in registers through small loops.** Inner regions
  are allocated first and their registers relayed up as hints
  (`hint_across_regions`), so a value carried around a loop stays in one
  register where ion spills and reloads it on the back edge. 006:
  - tinygo `countMatches`: every matched inner loop beats ion's; scan loop 0
    vs 3 stack accesses per iteration; estimated cycles 33→18, 51→26,
    47→27.
  - kotlin-richards `function[71]`: 0 vs 5 per iteration (58→33, 52→27).
  - sqlite3 inner loops: 0 vs 2-3 per iteration.
  - hashset `rehash`: 2 vs 10 moves per outer iteration.
- **Long-lived pointers in callee-saved registers across calls.**
  - sqlite3 VDBE (`function[191]`, `u0-181`): ion reloads the heap base 539
    times and `vmctx` 1,027 times. Over the entry paths of all 185 opcode
    arms: 783 vs 1,367 stack accesses (-43%), bought with 1,097 vs 84 moves.
    159 arms touch the stack less, 19 more. **-3.8% to -4.6% cycles.**
  - kotlin-richards: in a 47-call loop nest, 181 → 24 stack accesses, 16 →
    216 moves (one `mov x2, x19` per call).
  - hashset `benchmark()` (§9).

  A change judged on move counts scores every one of these as a regression.
- **Whole-function coalescing.** rust-protobuf `encode_varint` (`u0-31`):
  35 bundles all assigned at the first phase, 190 merges, 0 refusals, 0
  splits; moves 53 → 18, one callee-saved pair fewer. `decode_varint` 33 →
  19. tinygo `MatchRunePos`: 28 vs 68 moves. VDBE: 86,706 merges, 91
  refusals.
- **Remat, spill-at-def/reload-at-use, small frames** (§5).
- **What still loses inside sqlite3's win:** the dispatch latch + head is 28
  insts / 11 stack accesses / 5 moves vs ion's 19 / 7 / 0 (81 vs 56 estimated
  cycles), once per opcode. The head re-stores six loop-carried values every
  opcode, because `spill_placement.rs` walks a spill up to the first legal
  block in the value's own loop region, which is the header. The latch
  reloads five values the arms left only in slots. Whether the six stores are
  redundant on every incoming path was not measured.
- **Local weakness inside rust-protobuf's win:** in `dlmalloc`
  (`u0-219`), a 2-span bundle evicts the function-long shadow-stack pointer,
  which is split five times and spilled (1 spill, 5 reloads, all on the
  `sbrk` slow path). The fast path is equal to ion's.

## 11. Loop-exit stubs: the other side of the inner-loop win (006)

Because regions are allocated inner first, a loop gets its values into
registers, and the parent region then picks the join's locations
independently. The exit edge pays the difference. In tinygo `countMatches`,
edge stubs that only move values total **5,720 instructions / 3,987 stack
accesses vs ion's 693 / 585**. The scan loop's exit stub is 27 instructions
vs ion's 4: 23 even after removing slot copies (7 moves, 6 reloads, 8
spills). Its entry does the mirror image: a rotation `x20→x2, x4→x20, …`.

The outer dispatch loop is worse *inside* the loop too: 68 vs 32 stack
accesses per iteration (547 vs 356 estimated cycles). Jump-table arms reload
1-2 values each, and `x19`/`x20`/`x22` are stored in six blocks per iteration
to feed the exit stubs. `machine.add` has 2-5-move permutations on each of 11
jump-table arms, and eight early-exit stubs with 5 stack accesses each vs
ion's 1-3.

**Why:** nothing weighs an exit edge by how often the loop is *entered*
relative to how often it *iterates*. `block_weight` assumes 10 iterations
per entry; a scan loop that runs a handful of times loses on that bet.

**Fix directions:**
- have the parent adopt the inner loop's locations for values the loop never
  writes (the exit stub's spills are of such values);
- or price the exit edge against the in-loop savings.

Either must keep §10's inner-loop win. Together with §8 this is tinygo's
+21%. The share of each was not measured: profile deltas say `countMatches`
is 64% of the extra guest samples and `machine.add` 18%.

## 12. Demanded-move hoisting (`1db4308`) — a small uniform gain, PRESERVE

The pass hoists a write that every successor of a branch makes first into
the branch block, when cheaper under estimated cycles. Measured in 006 with
`run -q` vs `run -q --no-hoist-demanded` over each per-module corpus:
estimated cycles -0.22% (kotlin) to -1.79% (sqlite3), reloads -0.4% to -7.9%,
**never worse on any corpus total**. Worth ~0.8% estimated cycles on tinygo's
two hot functions. It did not change sqlite3's dispatch latch.

## 12b. Suite-wide static counts: the baseline (2026-10-09, `cf70ddd`)

The first whole-suite corpus (all 17 `pca.suite` modules, 6,416 inputs, 11
minutes to capture), replayed with `run -q`:

| moves (cold) | spills | reloads | remats | slots | estimated cycles | out-of-fuel |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 210,177 (8,054) | 23,656 | 71,911 | 21,862 | 8,658 | 103,351,088 | 0 |

sqlite3 is 3,472 of the 6,416 inputs. A step-3 change is judged against
these; cold moves are deliberate (§4).

## 13. Compile time vs ion: a baseline, not a finding (2026-10-09)

Recorded so the next §7 table has something to compare against. **Not
analyzed, by design** (SKILL.md §7). `./scripts/sightglass -p compilation
-i 5 benchmarks/pca.suite` at regicide `cf70ddd`, instructions retired; ~2.7
minutes:

| module | ion | regicide | delta |
| --- | ---: | ---: | ---: |
| `shootout-switch` | 3.81 G | 414 G | +10,777% |
| `tinygo-regex` | 4.19 G | 73.7 G | +1,659% |
| `hashset` | 1.52 G | 19.6 G | +1,190% |
| `libsodium-core2` / `-siphashx24` / `-hash3` | 0.33-0.37 G | 3.9-4.0 G | +983% to +1,064% |
| `shootout-sieve` / `-memmove` | 0.23 G | 1.73 G | +659% to +666% |
| `sqlite3` | 6.72 G | 39.5 G | +488% |
| `shootout-keccak` | 0.32 G | 1.26 G | +296% |
| `shootout-heapsort`, `hex-simd`, `blake3-scalar`, `rust-protobuf`, `kotlin-richards`, `cm-online-stats` | | | +146% to +188% |
| `splay` | 0.053 G | 0.111 G | +112% |

Geomean **+531%**, all 17 significantly slower. Within-engine spread under
0.5% at `-i 5`, so the low sample count is enough.

---

## Open questions, in priority order

1. **Keep slot-class promises** (§8). Bound the tinygo-regex share with a
   scratch `alloc_class_slot` + `build-at` + `time-cwasm`.
2. **Price loop-exit edges** (§11): the other half of tinygo's +21%. Needs
   runtime measurement, since there is no block-level heat on macOS.
3. **A callee-saved cost model** (§9): siphashx24 +1.25%; must keep sqlite3,
   kotlin, hashset and protobuf's wins. Needs a `Machine` API change.
4. **VDBE dispatch head stores** (§10): are the six per-opcode stores
   redundant on every incoming path?
5. **Unexplained self-time deltas** (§6). A layout or I-cache explanation for
   `MatchRunePos` (+0.16/run) would tell whether tinygo's callers' size
   costs more than their own code.
6. **Straight-line seam moves** (§5), **cut-sensitive split score** (§3),
   **sieve's layout hypothesis** (§6). Low priority.

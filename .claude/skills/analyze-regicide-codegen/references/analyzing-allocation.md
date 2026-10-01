# How to analyze a region of code

This is the methodological core of the skill: given a hot basic block and the
two allocators' output for it, how to work out what happened and why it matters.

The unit of analysis is **one basic block**, usually inside an innermost hot
loop. Blocks are small enough to reason about exhaustively and big enough to
contain a real decision.

---

## The "why" ladder

Four levels. Each per-block analysis should climb as far as the evidence takes
it, and say where it stopped. Stopping at level 1 is the most common way this
work comes out useless: "regicide inserted an extra move" is an observation, not
a finding. The finding is *why* it did, and what about its design made it do so.

### Level 1 — What differs in the generated code?

Diff the two disassemblies of the block and characterize the difference. The
things worth looking for, roughly in descending order of how much they cost:

- **A spill or reload one allocator emitted and the other did not.** Touches
  memory; the most expensive single difference.
- **An operand in a stack slot where the other allocator got it a register.**
  Every use pays.
- **A spill or reload *inside* a loop that the other hoisted out of it.** Cost
  multiplies by the trip count — the single highest-leverage difference you can
  find.
- **An extra register-register move.** Cheap individually, but they cluster.
- **A different register choice** that forces a move at a block boundary or at
  a call, often because of a fixed-register or reuse constraint.
- **More spill slots** than the other allocator used, i.e. a bigger frame.

Count them, don't eyeball them. §"Virtual cycles" gives the arithmetic.

**A benchmark regicide wins overall can still contain a block where it does
something bad**, masked by a win elsewhere. Analyze the block on its own terms
and record what you see, regardless of which direction the benchmark went.

### Level 2 — Why did the allocator do that?

This is where `scripts/log` and `scripts/visualize` earn their keep. The
question is always "what decision produced that instruction?", and the answers
look like:

- The bundle was **split**, and the split point put a move here.
- The bundle was **evicted** by a higher-priority one, and had to go somewhere
  worse.
- **Coalescing was refused** — the two spans conflicted, or wanted disjoint
  locations — so the copy survived.
- **Spill placement** chose this point rather than a cheaper one.
- A **fixed-register constraint** (a call's ABI registers, a reused operand)
  forced the assignment, and everything else moved around it.
- The value was **rematerialized** instead of reloaded, or vice versa.

The log prints each event as `event: <Variant> { .. }`, so you can grep for the
decision you care about by name. The variants worth knowing, by question:

| Question | Events to grep for |
| --- | --- |
| Did this bundle get a register, and was it the one it wanted? | `BundleAssignedPreferred`, `BundleAssignedNonPreferred`, `BundleAssignedViaEviction`, `BundleHint` |
| Did it lose its register? | `BundleEvicted`, `BundleSpilledToStack`, `ValueSpilled` |
| Was it split, and how? | `BundleSplit`, `BundleSplitBetweenSpans`, `BundleSplitWithinSpan`, `BundleSplitIntoMinimal`, `BundleSplitFailed`, `EagerSplit`, `OverwriteSplit`, `SpanSplit`, `BundleNarrowedToEarlyHalf` |
| Why is this copy still here? | `CoalesceRefused`, `SpansCoalesced`, `CrossRegionAffinity` |
| Where did the spill go? | `SpillPlaced`, `SpillBlockChosen`, `SpillSlotAssigned`, `SpillSlotCreated`, `SpillSlotGroupCreated` |
| Why does this move exist? | `MoveEmitted`, `MoveResolved`, `ParallelMoveBatchSolved`, `HalfMoveSource`, `HalfMoveDest`, `CriticalEdgeEdit` |
| Why was a move *not* removed? | `EditEliminated`, `DeadEditEliminated` |
| Remat instead of reload? | `BundleRematerialized`, `RematEmitted` |
| Scratch register pressure | `ScratchRegChosen`, `ScratchRegBorrowed` |

`CoalesceRefused` carries a `RefusedReason` — `ConstraintConflict` (the two
sets' uses want disjoint locations) or `Overlap` (they are live at the same
time) — which usually answers "why is this copy still here?" outright.

Get at it by narrowing the log to the pass in question and the function in
question. The event log is regicide's own account of its decisions:

```bash
cd ~/regalloc2
WASMTIME_LOG=regicide::region_allocation=trace ./scripts/log \
    ~/sightglass/benchmarks/bz2/benchmark.wasm 2> /tmp/alloc.log
```

Once you have a corpus (§3 of SKILL.md), replaying one function is much faster
than recompiling the module, and gives the same events:

```bash
cd ~/regalloc2/regicide
RUST_LOG=regicide::region_allocation=trace \
  cargo run --all-features --example run -- ../$R/corpus/bz2-benchmark-0003-foo.json
```

`references/tools.md` has the full table of log targets and the question each
one answers. `region_allocation` (15 event sites) and `move_generation` (12) are
where most answers live.

For anything involving splits, evictions or live-range shapes, the HTML
visualization is far easier to read than the log:

```bash
./scripts/visualize -f '<function-name>' \
    ~/sightglass/benchmarks/bz2/benchmark.wasm $R/misc/viz-bz2/
```

Note that `visualize` is **regicide-only** — there is no equivalent for
regalloc2's backtracking allocator. For the backtracking side you have the
disassembly and `regalloc2=trace`.

### Level 3 — Why did regicide's design lead to that decision?

The level that makes a finding actionable. Given that regicide split a bundle or
evicted another, what about its architecture, algorithms or heuristics made that
the chosen move? Regicide is **region-based**, which is the structural
difference from regalloc2's ion-style backtracking, so the usual suspects are:

- **Region granularity** — the region tree decided the unit of allocation, and
  the value's live range did not fit it well.
- **Priority order** — which bundle got to pick first, and on what basis.
- **The eviction cost model** — what it thought the eviction would cost versus
  what it actually cost.
- **Split heuristics** — where it chose to cut, and what that cost at the seam.
- **Spill placement policy** — whether it considered loop depth, and how.
- **Move elimination reach** — `local` versus `global`; whether a move survived
  only because elimination could not see across the relevant boundary. Testing
  this is cheap: replay with `-r none` / `-r local` / `-r global` and diff the
  counts.

**The escalation ladder is the structure to reason against.** Region allocation
retries a bundle through four `Phase`s, in order, each giving up something
(`regicide/src/region_allocation.rs`, `enum Phase`):

| Phase | What it tries |
| --- | --- |
| `AssignEvict` | Assign a register, evicting a lower-priority bundle if needed. Nothing has been given up yet. |
| `SplitBetweenSpans` | Cut the bundle at a span boundary and retry each half. Optimistically keeps bundles large. Falls straight through if there is no boundary to cut at. |
| `SplitWithinSpan` | Cut between two uses inside one span and retry. Falls through if no span has two uses. |
| `SplitIntoMinimalOrSpill` | Split into minimal bundles; if already minimal, spill this bundle or one it conflicts with. The end of the road. |

So "why is there a reload here?" usually resolves to **how far down this ladder
the bundle fell, and what pushed it**. A bundle that reached
`SplitIntoMinimalOrSpill` was refused three times first, and the events say by
what. Quote the phase it ended in — it localizes the finding better than
"regicide spilled it" does.

Name the module and, where you can, the specific heuristic. "Regicide's
eviction cost model undervalues a spill inside a loop, in
`region_allocation.rs`" is a finding someone can act on; "regicide's heuristics
are worse here" is not.

Reading the source is in scope and often the fastest route — just don't change
it.

### Level 4 — So what?

Each block analysis ends with one of:

- **Fix:** what to improve, and roughly where. Note how broadly it applies — the
  same pattern in several benchmarks is worth far more than a one-off.
- **Preserve:** what regicide did better, and what a future change might break.
  Wins need recording precisely because the next change could silently undo
  them.

---

## Virtual cycles: putting a number on one block

Instruction counts do not distinguish a register-register move from a reload,
but the difference is roughly an order of magnitude. So for a single block,
count operands and moves by kind and weight them. This is the metric that lets
you say "this block got better" about a block, rather than about a benchmark.

| Thing | Cost |
| --- | --- |
| Register operand | 1 |
| Register-register move | 1 |
| Stack-slot operand | 10 |
| Move that touches the stack (spill or reload) | 10 |

Sum over the block, for each allocator. Lower is better.

```
virtual_cycles = reg_operands + reg_moves + 10 * (stack_operands + spills + reloads)
```

The weights are deliberately crude — they encode "memory is about ten times a
register" and nothing more. They are for **comparing two allocations of the same
block**, not for predicting absolute performance. Do not carry a virtual-cycle
number across blocks or sum it over a function; it means nothing there.

Two refinements worth applying when they matter:

- **Weight by loop depth.** A block in a doubly-nested loop executes far more
  often than the function entry. If the block is in a loop and the other
  candidate block is not, say so — the raw virtual cycles understate it.
- **Dynamic, not static.** The profile tells you how hot the block actually is.
  Ten virtual cycles saved in the hottest block beats a hundred saved in a block
  with two samples.

Record the count in the per-block analysis as a small table, both allocators
side by side, with the operand and move counts it was derived from so the
arithmetic is checkable.

---

## A worked example

What a good per-block analysis looks like. (Illustrative — the numbers are made
up; yours come from the tools.)

> ### Block 3 of `BZ2_compressBlock` (41% of samples, innermost loop)
>
> **Level 1 — what differs.** Regicide reloads `v47` at the top of the block and
> spills it again at the bottom; backtracking keeps it in `x23` across the whole
> loop. Otherwise the two blocks are instruction-for-instruction identical.
>
> | | backtracking | regicide |
> | --- | --- | --- |
> | register operands | 14 | 13 |
> | reg-reg moves | 2 | 1 |
> | stack operands | 0 | 0 |
> | spills + reloads | 0 | 2 |
> | **virtual cycles** | **16** | **34** |
>
> The block is the innermost loop body, so the two memory touches are paid every
> iteration.
>
> **Level 2 — why.** `regicide::region_allocation` shows `v47`'s bundle evicted
> by the bundle for `v12` at the call in block 2, then split at the region
> boundary; the piece covering block 3 got no register and spill placement put
> the reload at the block top. Backtracking never evicts it: `v47` has a longer
> live range and wins on priority.
>
> **Level 3 — design.** The eviction compared `v47`'s spill cost against `v12`'s
> without accounting for `v47`'s use being inside a loop two levels deep, so it
> priced one reload rather than one-per-iteration. The region boundary then
> turned a single eviction into a split, and the split seam is where the reload
> landed.
>
> **Level 4 — so what.** **Fix:** weight spill cost by loop depth in the
> eviction cost model (`region_allocation.rs`). Seen in the hottest block of
> `bz2` and in two blocks of `regex`, so it is not a one-off.

---

## Practical notes

- **Go through the profile first, always.** A block with no samples is not a
  finding, however bad its codegen looks.
- **Match functions between the two disassemblies by name.** Numbering and
  ordering can differ between allocators.
- **Quote the actual instructions** in the report. A reader who disagrees with
  your reasoning can still use the disassembly.
- **The input and output dumps are often the fastest orientation.**
  `regicide::lib` trace-logs the whole input before allocation and the whole
  output plus `counts` after, which is quicker to read than reconstructing the
  function from events.
- **Replay with a pass turned off to attribute a cost.** `run.rs`'s `-r` / `-d`
  flags make "how much is redundant move elimination buying here?" a one-command
  experiment.
- **`run.rs`'s counts are per-function totals**, not per-block: `moves`,
  `spills`, `reloads`, `remats`, `critical-edges`, `slots`. Use them to pick
  which function to look at and to diff whole corpora; use the disassembly for
  per-block work.

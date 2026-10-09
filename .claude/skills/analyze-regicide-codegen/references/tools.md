# The tools

Every script lives in `~/regalloc2/scripts/`, takes `-h`, and is safe to read —
they are short, commented bash. This file is the summary; `-h` is the source of
truth if they drift.

All of them build Wasmtime or regicide on first use, and the first build of a
given cargo profile is slow (several minutes for Wasmtime with warm dependencies, much longer from cold). Prefer running them
in the background over waiting.

---

## `scripts/sightglass` — benchmark the two allocators

Runs sightglass comparing register allocators and writes CSV to stdout.

```
./scripts/sightglass [OPTIONS] [BENCHMARKS...]
```

The important design point: it builds `wasmtime-bench-api` **once** and passes
the *same* dylib to sightglass twice, once per allocator, each with its own
`-O regalloc-algorithm=...` and its own `-n` name. So a run compares
**allocators, not builds** — there is no way for a stray rebuild to contaminate
the comparison, and the engines appear in the CSV as `regicide` and
`backtracking`.

| Flag | Default | Notes |
| --- | --- | --- |
| `BENCHMARKS...` | `benchmarks/pca.suite` | `*.wasm` or `*.suite`. Resolved relative to the sightglass checkout when not found as given, so `benchmarks/bz2/benchmark.wasm` works. |
| `-r`, `--regalloc` | `both` | `regicide`, `backtracking`, or `both`. |
| `-p`, `--phase` | `execution` | `compilation`, `instantiation`, `execution`, `all`. We want `execution`. |
| `-m`, `--measure` | `insts-retired` | Passed to sightglass's `-m`. The other one you want is `cycles`. |
| `-i`, `--iterations` | 3 for `insts-retired`, else 30 | **Total** samples per benchmark. The script converts this into sightglass's processes × iterations-per-process for you. |
| `-h`, `--help` | | |

Anything after `--` is forwarded to `sightglass-cli benchmark`.

`SIGHTGLASS` overrides the checkout location (default `~/sightglass`).

The script passes `--raw -f csv`, so **stdout is the raw per-sample CSV**, not
the human-readable analysis. Redirect it to a file under
`$R/sightglass-data/`; progress messages go to stderr. To get the
human-readable comparison instead, feed the CSV to `effect-size` (below) or
re-run without `--raw` by hand.

### Turning the CSV into a ranking

```bash
cd ~/sightglass
cargo run --release -q -- effect-size -f <the>.csv -i csv -o csv > es.csv
```

`effect-size` re-runs sightglass's own significance test on saved samples, so
you can re-analyze without re-measuring. Its output columns:

```
arch,wasm,phase,event,a_engine,a_engine_flags,a_mean,b_engine,b_engine_flags,
b_mean,significance_level,half_width_confidence_interval
```

and the significance test is exactly `|a_mean - b_mean| > |half_width_ci|`.

Two scripts in this skill consume that file:

- `scripts/rank_benchmarks.py es.csv --top N` — ranks every benchmark
  worst-first with its delta and significance, then prints the top `N` each way
  as `.wasm` paths ready to paste into a confirmation run. This is the one you
  want for picking what to analyze.
- `~/.claude/skills/optimize-cranelift-codegen/scripts/check_no_pessimization.py`
  — the sibling skill's per-benchmark regression gate. It works on regicide
  data; pass `--baseline backtracking --candidate regicide`. Useful when you
  want a pass/fail answer rather than a ranking. **Its deltas use the opposite
  sign (positive = faster)**; negate them before quoting.

---

## `scripts/profile` — find the hot functions

Profiles a Wasm program's execution under `samply-text`.

```
./scripts/profile [OPTIONS] path/to/wasm [-- GUEST_ARGS]
```

The module is compiled ahead of time, so the profile shows the program
*running* rather than Cranelift compiling it, and the run is repeated for a
fixed span because one iteration of a small program yields too few samples.

| Flag | Default | Notes |
| --- | --- | --- |
| `--loop SECS` | 5 | Keep re-running for this many seconds; `0` runs it exactly once. |
| `--regalloc WHICH` | `regicide` | `regicide` or `backtracking`. **Profile both.** |
| `-h`, `--help` | | |

Every other flag is forwarded to `samply-text record`: `-t`/`--tree` for a call
tree, `-c` to collapse direct recursion, `-s PCT` for the tree's pruning
threshold, `-n N` for more rows, `-r` for the sampling rate. Anything after
`--` goes to the Wasm program as its own arguments. **Defaults it adds:**
`-n 100000` unless `-n` is given, and with `-t`, `-s 0.005` unless `-s` is
given. `samply-text`'s own default (`-s 0.1`) hid 76% of sqlite3's guest
samples.

**Use `-t`, keep the stderr.** The flat view is topped by Wasmtime's idle
threads parked in the async runtime — asleep, not working. The guest's own
frames are under `array_call_native`. Redirect stderr to
`<name>.prof.stderr` beside the profile: its `loop-driver: N run(s)` line is
what `scripts/guest_profile.py` normalizes by.

```bash
./scripts/profile -t --loop 10 B.wasm > $R/profiles/B-regicide.txt 2> $R/profiles/B-regicide.prof.stderr
python3 .claude/skills/analyze-regicide-codegen/scripts/guest_profile.py \
    $R/profiles/B-backtracking.txt $R/profiles/B-regicide.txt
```

`guest_profile.py` ranks frames under `array_to_wasm_trampoline` by self
samples per run, prints a base/candidate delta table, and a **coverage**
figure (visible self samples ÷ trampoline total). Below ~75% the tree was
pruned; lower `-s`.

**Components.** Given a component, the script extracts its main core module
with `wasm-tools component unbundle --threshold 0` and profiles that, when the
component wraps one WASIp1 module exporting `_start` whose only imports are
`wasi_snapshot_preview1` and `bench` (kotlin-richards). It refuses anything
else, e.g. cm-online-stats, a composition of components. Function indices in
the profile are then the core module's, matching a disassembly of the
extracted module, and the component's `wasm[0]::` functions.

Built with the `profiling` cargo profile (optimized, with line tables) and run
with `--profile=perfmap`, so the JIT-compiled Wasm frames symbolicate instead of
coming back as bare offsets.

### Profiling an existing `.cwasm`

`scripts/profile` recompiles the module every time. That takes seconds at
present; if regicide ever takes minutes to compile a module again
(tinygo-regex took 40 minutes in 002), profile the
`.cwasm` that `scripts/disassemble` already wrote, with the release
`wasmtime`. samply cannot inject into the system-signed `/bin/sh`, so the
re-run loop must be a compiled binary:

```rust
// /tmp/loopdrv.rs -- rustc -O -o /tmp/loopdrv /tmp/loopdrv.rs
use std::{env, process::{Command, Stdio}, time::{Duration, Instant}};
fn main() {
    let mut a = env::args().skip(1);
    let secs: u64 = a.next().unwrap().parse().unwrap();
    let argv: Vec<String> = a.collect();
    let (start, mut runs) = (Instant::now(), 0);
    while start.elapsed() < Duration::from_secs(secs) {
        let s = Command::new(&argv[0]).args(&argv[1..]).stdout(Stdio::null()).status().unwrap();
        runs += 1;
        if !s.success() { std::process::exit(1); }
    }
    eprintln!("loop-driver: {runs} runs");
}
```

```bash
cd ~/sightglass/benchmarks/<dir>      # benchmarks read inputs by relative path
samply-text record -t -n 250 -- /tmp/loopdrv 15 \
    ~/regalloc2/wasmtime/target/release/wasmtime run --profile=perfmap \
    --allow-precompiled -W unknown-imports-default -S cli --dir .::. \
    $R/profiles/asm/<bench>-<alloc>.cwasm > $R/profiles/<bench>-<alloc>.txt
```

The two allocators complete different numbers of runs in the same window, so
compare **guest samples per run** (`array_to_wasm_trampoline` total ÷
`loop-driver: N runs`), not raw samples. Guest frames symbolize via the
perfmap even without line tables.

**Do not try to get per-address heat from samply's raw JSON.** The `address`
recorded for a perfmap JIT frame is offset from the real address by a constant
that varies per function and per run; block attributions from it are wrong.

### Running a sightglass benchmark standalone

Sightglass benchmarks import `bench.start` / `bench.end`, so they do not run on
their own. `scripts/profile` already passes
`-W unknown-imports-default -S cli`, which handles it. If you invoke `wasmtime`
yourself, preload a stub:

```bash
cat > /tmp/bench-stub.wat <<'EOF'
(module (func (export "start")) (func (export "end")))
EOF
cd ~/sightglass/benchmarks/bz2 && \
  wasmtime run --preload bench=/tmp/bench-stub.wat --dir . ./benchmark.wasm
```

The `cd` and `--dir .` matter: benchmarks open their inputs by relative path.

`scripts/support/bench-timer.wat` is a `bench` stub that also **times** the
benchmark: it reads the monotonic clock at `bench.start` and `bench.end` and
writes `bench-ns <N>` to stderr, the same span sightglass measures, without
process startup. `scripts/time-cwasm` uses it.

---

## `scripts/build-at` — Wasmtime with regicide from another revision

```
./scripts/build-at [-w DIR] [COMPILE_OPTIONS] REV path/to/out path/to/wasm...
```

Builds a scratch Wasmtime under `/tmp/build-at` (or `-w DIR`) whose regicide
is `git archive REV` of the regicide worktree, everything else being this
checkout's regalloc2 HEAD and Wasmtime HEAD plus its uncommitted diff, and
compiles each module to `out/<module>-<sha>.cwasm`. The first build takes
~3.5 minutes; later revisions rebuild incrementally (~15 s plus regicide).
Nothing in the real checkouts is touched.

For a runtime bisect across regicide commits, when replaying a corpus cannot
show the cost (005, hashset: hot code unchanged, cycles moved 6 pp). Validate
the setup by building at the revision an old report measured and checking
its `.cwasm` disassembles identically (`wasmtime objdump --funcs all X.cwasm
| md5`). Two traps it handles, worth knowing if you do this by hand: a
symlinked regicide swapped between revisions is not rebuilt (cargo
fingerprints by path), and `git archive` preserves old mtimes, which cargo
can take as up to date.

Regicide revisions whose public API differs from what regalloc2's shim
expects will not build; the error names the API. Everything from `e9b676b`
(2026-10-06) to `288da68` builds.

## `scripts/time-cwasm` — interleaved timing of precompiled builds

```
./scripts/time-cwasm [-n ROUNDS] [-d DIR] [-o FILE] BASELINE.cwasm OTHER.cwasm...
```

Runs each `.cwasm` once per round for `-n` rounds (default 40), in turn, so
host noise falls on all alike, timing the `bench.start`..`bench.end` span via
`bench-timer.wat`. Prints each one's median, 20%-trimmed and minimum delta
against the first, in the house sign convention. `-d` is the directory the
benchmark reads its inputs from (`~/sightglass/benchmarks/<dir>`).

Uses: comparing `build-at` outputs; timing an old report's
`profiles/asm/*.cwasm` beside today's (005 reproduced 003's hashset result
this way); a quick A/B before committing to a sightglass run. It is not a
substitute for sightglass cycles in the report's top line. ~80 rounds
resolved ~2% differences on a host at load 10-15; at load 30+ (005, other
sessions' jobs) CV reached 25% and nothing resolved.

---

## `scripts/disassemble` — see the generated code

```
./scripts/disassemble [COMPILE_OPTIONS] path/to/the.wasm [-- OBJDUMP_OPTIONS]
```

Compiles the module and prints a disassembly of its functions. `COMPILE_OPTIONS`
go to `wasmtime compile` (this is where `-O regalloc-algorithm=...` goes);
options after `--` go to `wasmtime objdump`.

Useful objdump options: `--funcs all` (all functions, not just the first),
`--addresses` (instruction addresses), `--filter <substring>` (restrict to
matching function names), `--addrmap` (annotate with the Wasm offset each
instruction came from).

The `.cwasm` is written **beside the module**, as `path/to/the.cwasm`, so that a
disassembly can be diffed against a rebuild. Note that this means it writes into
the sightglass checkout when you disassemble a benchmark there.

The canonical comparison:

```bash
./scripts/disassemble -O regalloc-algorithm=regicide  B.wasm -- --funcs all > /tmp/r.asm
./scripts/disassemble -O regalloc-algorithm=backtracking B.wasm -- --funcs all > /tmp/b.asm
diff -u /tmp/b.asm /tmp/r.asm
```

**Match functions by name, not index or offset** — numbering and ordering can
differ between the two.

A component disassembles fine; each core module's functions are prefixed
`wasm[0]::`, `wasm[1]::`, and so on.

### `asm_tools.py` — the mechanical analyses

`.claude/skills/analyze-regicide-codegen/scripts/asm_tools.py` works on
`--funcs all --addresses` output:

| subcommand | what |
| --- | --- |
| `mix BASE.asm CAND.asm` | per-function insts / stack accesses / reg-reg moves, both allocators, sorted by delta |
| `extract FILE.asm N [--strip]` | one `function[N]`; `--strip` masks addresses for diffing builds |
| `same OLD.asm NEW.asm` | which functions changed between two builds, ignoring addresses |
| `loops FUNC.asm` | loops innermost first, with `head%64`, cache lines spanned, heap-offset fingerprint |
| `slot2slot BASE.asm CAND.asm` | stack-slot-to-stack-slot copies per function (known-findings §8) |

Stack accesses use the narrow filter (single register to `[sp`), so
prologue `stp`/`ldp` pairs are not counted.

### `estimated_cycles.py` — estimated cycles over a path

```
python3 .claude/skills/analyze-regicide-codegen/scripts/estimated_cycles.py FUNC.asm START-END [START-END ...]
```

Regicide's estimated cycles (the cost model `run` prints;
`references/analyzing-allocation.md`) over the concatenation of inclusive
hex address ranges: a loop's in-loop path, an exit stub, one opcode arm.
Every instruction counts at block weight 1, since disassembly carries no
loop depth or frequency. Prints instructions, register operands, reg-reg
moves, immediate `mov`s (remats), stack accesses and the total. **It counts
any `[sp` operand**, with `stp`/`ldp` as two, so on a range that holds a
prologue it reports more stack accesses than `asm_tools.py`'s narrow filter.

---

## `scripts/log` — see the allocator's decisions

```
./scripts/log [OPTIONS] path/to/wasm
```

Compiles the module with register allocation logging on, writing the log to
**stderr** (redirect with `2>`). Options are forwarded to `wasmtime compile`.

Passes `--features trace-log`, `-C cache=n` (a cache hit would log nothing) and
`-C parallel-compilation=n` (so each function's records stay together), and
discards the compiled output — this compiles for the log, not for an artifact.

The default `WASMTIME_LOG` is

```
cranelift_codegen::machinst::compile=trace,cranelift_codegen::timing=trace,regalloc2=trace,regicide=trace
```

which is a *lot* of output. `WASMTIME_LOG` is honored if already set, so narrow
it to the pass you are asking about:

```bash
WASMTIME_LOG=regicide::spill_placement=trace ./scripts/log the.wasm 2> log.txt
```

Regicide's event log is what you are actually reading. Events are emitted by the
`event!` macro, which `trace!`s each one as `event: <Debug>`; because `log`
records the module it was expanded in, **per-module filtering works on events**.
The modules that emit them, with the question each answers and how many event
sites it has:

| Target | Sites | Answers |
| --- | --- | --- |
| `regicide::region_allocation` | 15 | Why did this bundle get this register? Why was it evicted, split, or spilled? **The main one.** |
| `regicide::move_generation` | 12 | Why does this move exist? |
| `regicide::live_ranges` | 8 | What is live where; how spans were built. |
| `regicide::coalescing` | 5 | Why were two spans joined, or refused? |
| `regicide::spill_slot_allocation` | 5 | Which slot, and why that many slots? |
| `regicide::spill_placement` | 2 | Where did the spill go, and why there? |
| `regicide::redundant_move_elimination` | 1 | Why was a move kept rather than removed? |
| `regicide::dead_move_elimination` | 1 | Same, for dead moves. |

Caveats:

- **`SpanConstructed` span IDs are provisional in the text log.** They are
  emitted before the block's spans are sorted; only the in-memory event log is
  remapped, under the `visualizer` feature. Span → block is correct; span →
  value is permuted within each block. Joining later events
  (`SpansCoalesced`, `CoalesceRefused`) to values through `SpanConstructed`
  gives nonsense (in 002: "92,174 `CrossEdge` merges between different
  values"). Use the visualizer's JSON when you need the value.
- `regicide::liveness` emits nothing — liveness results surface through
  `live_ranges`. Filtering on it gets you silence.
- `region_allocation`'s own submodules (`assignments`, `splitting`) emit no
  events directly; they are all logged from `region_allocation` itself, so
  filter on the parent.

Besides the events, `regicide::lib` trace-logs the whole input before allocation
and the whole output plus `counts` after it, which is often the fastest way to
see what changed.

A `trace!` in regicide is compiled out unless `trace-log` or `debug_assertions`
is on, which is why the script passes `--features trace-log`.

---

## `scripts/visualize` — the fancy version of `log`

```
./scripts/visualize [OPTIONS] path/to/wasm path/to/out
```

Writes a standalone HTML+JS visualization of each of the module's register
allocations into the output directory. This is `log`'s information rendered as
something you can actually navigate — use it when the textual log is too much.

| Flag | Notes |
| --- | --- |
| `-f`, `--filter FUNC` | Only visualize captures whose name matches this POSIX extended regex (case-insensitive). **Use it** — a whole benchmark is hundreds of functions. The name is the capture's, which is the corpus placeholder `u0-N` for every `pca.suite` module checked in 006 (tinygo-regex included), so filter `-f '^u0-(99\|229)$'`, not by function name: a name filter silently renders nothing. |

Anything after `--` goes to `wasmtime compile`.

Works by compiling with `REGICIDE_DUMP` set, which captures each allocation's
input under `<out>/dumps/`, then rendering each with regicide's `visualize`
example. A single allocation that fails to render does not sink the
rest; its error goes to `<out>/<name>.log`.

Each page is `<out>/<pid>-<counter>[-<function>].html`, matching its capture's
name: the pid is the *compiling process's*, and the counter is in function order
(`parallel-compilation=n`). So **the filename for a given function is not
predictable before the run** — take it from the stderr listing, which echoes
every path written, or by `ls`-ing the output directory afterwards. An
unnameable function gets no `-<function>` part and can only be identified by its
counter.

**Regicide only.** There is no equivalent for regalloc2's backtracking
allocator, so a visualization shows you what regicide did, not a side-by-side.

---

## `scripts/make-corpus` + `regicide/examples/run.rs` — replay allocations

Together these let you re-run a register allocation **without Wasmtime or Wasm
in the loop at all** — an allocation instead of a rebuild. This is the fast
iteration loop.

```
./scripts/make-corpus [OPTIONS] path/to/out path/to/wasm...
```

| Flag | Notes |
| --- | --- |
| `-c`, `--clean` | Empty the output directory first, rather than adding to it. |

Captures one **postcard-encoded** file per allocation, named
`<module>-<counter>[-<function>].postcard`. The counter is in function order
(`parallel-compilation=n`). `<module>` is the `.wasm`'s base name, except that
sightglass names nearly every module `benchmark.wasm` and distinguishes them by
*directory*, so `make-corpus` uses the directory name in that case —
`blake3-scalar/benchmark.wasm` becomes `blake3-scalar-0000-...`. Several
benchmarks can therefore share one directory without collision. Re-running over
a module overwrites rather than piling up duplicates; a genuine collision still
gets a `-2` suffix.

**Pass every module you want in one invocation**, since it builds Wasmtime once
and then compiles each in turn.

**The skill builds one corpus per run, of the whole suite, into `$R/corpus/`**
(SKILL.md §3, required). For `pca.suite` at regicide `cf70ddd` that is 11
minutes and 6,416 inputs. A capture is byte-identical whether the module was
built alone or with the rest of the suite, so per-module corpus directories
add nothing; select one module's inputs with a glob,
`$R/corpus/<module>-[0-9]*.postcard`.

It exits nonzero if any module captured nothing, which usually means a cache hit
— the script passes `-C cache=n`, so this should not happen, but check for it.

Find the input for a function by name:

```bash
ls $R/corpus | grep -i <function-name>
```

**This only works for modules with a name section.** Most sightglass benchmarks
are stripped, so captures come out as `benchmark-0004-u0-3.postcard` — `u0-3`
is Wasmtime's placeholder for module 0, *defined* function 3, not a symbol.
The disassembly's `function[M]` counts imports too, so for a module with `k`
function imports, `function[M]` is `u0-(M-k)` (tinygo-regex: 15; sqlite3:
10). Count them with `grep '(import' x.wat | grep -c '(func'`. Or identify the
function you want by its counts (the hot function is usually the one with by
far the most moves or spills):

```bash
cargo run --manifest-path regicide/Cargo.toml --all-features --example run \
    -q -- -q $R/corpus/shootout-heapsort-[0-9]*.postcard | grep -B1 -E "moves [0-9]{3,}"
```

### Replaying with `run.rs`

```
cargo run --manifest-path regicide/Cargo.toml --all-features --example run -- [OPTIONS] <path>...
```

A path is either a directory (every `*.postcard` and `*.json` directly in it,
sorted, so two runs over one corpus can be diffed) or a single input file. The
encoding is detected from the file's contents rather than its name, so an
old JSON corpus still replays. **The input format itself changes between
regicide revisions**, though: a `run` built at `288da68` cannot read a corpus
captured at `cf70ddd` (`1db4308` changed the input). Rebuild the corpus, or
compare revisions at runtime with `scripts/build-at` (SKILL.md §3).

| Flag | Notes |
| --- | --- |
| `-q`, `--quiet` | Print only the counts, not the allocations. Use for a whole corpus. |
| `--check` | Check each allocation against regicide's symbolic checker. |
| `-r`, `--redundant-moves LEVEL` | `none`, `local`, `global`. |
| `-d`, `--dead-moves LEVEL` | `none`, `local`, `global`. |
| `--dead-reg-moves` / `--no-dead-reg-moves` | Whether dead-move elimination considers registers or only spill slots. |
| `--validate` / `--no-validate` | Validate the input before allocating. |

**Every option defaults to what the input file itself asked for**, so a run with
no options replays the allocation exactly as captured. That is what you want for
analysis; the override flags are for asking "what if this pass were off?", which
is a genuinely useful experiment — e.g. `-r none` tells you how much redundant
move elimination is buying on this function.

Output per input is the allocation (unless `-q`) followed by a counts block:

```
  moves 12  spills 3  reloads 5  remats 0  critical-edges 1  slots 4
  of which cold: moves 1  spills 0  reloads 0  remats 0
  estimated-cycles 310.2  reg-operands 220.0  stack-operands 0.0  moves 11.0 ...
  coalesce-out-of-fuel 0
```

These are **the numbers to diff between two builds**, and the per-corpus
`=== totals ===` block at the end aggregates them. It is several lines, so
take it with `grep -A4 '=== totals'`, not `tail -1` (which shows only
`coalesce-out-of-fuel`). `estimated-cycles` weights each block by loop depth
and cold blocks by 0.05; older `run` builds lack the last three lines.

In the full (non-`-q`) printout each edit is a line such as
`edit: Move(Reload { from: stack84, to: r21 })`, and each block's header
lists its parameters' allocations. A `Reload` from one slot immediately
followed by a `Spill` of the same register to a different slot is a
stack-to-stack copy (known-findings §8). `moves` is register-register
moves only; `spills` and `reloads` are the ones that touch memory. `slots` is
the stack frame size in spill slots.

**The corpus route is regicide-only.** `REGICIDE_DUMP` (via the `regicide-dump`
feature) captures regicide's inputs; there is no equivalent that dumps
regalloc2's inputs from a Wasmtime compile, so you cannot replay the same
function through the backtracking allocator this way. `regalloc2-tool` does run
`ion`, `fastalloc` and `regicide` over one function, but it reads a
**bincode-encoded `SerializableFunction`**, a different format from regicide's
captures, and nothing in `scripts/` produces it from a Wasm compile.

So: use `run.rs` counts for regicide's own absolute numbers and for diffing
regicide against regicide; use the **disassembly** for anything that compares
the two allocators. Do not present a `run.rs` counts table as a comparison
between allocators — it cannot be one.

A function needing more registers at once than the machine has is reported as
`skipped`, which is a property of the input rather than a bug.

**Replays are fast at current revisions** (005: tinygo `countMatches`
1.2 s, sqlite3's VDBE ~1 s). They were not always (002: 471 s and 502 s,
the split-peel loop of known-findings §3), and old revisions built for a
before/after comparison still are (`e9b676b` replays the VDBE in 58 s). Use
a timeout per function (`timeout 900 run -q one.postcard`) when replaying
through an old build, so one slow function does not hide the rest.

**A full trace of one big function is enormous.** `RUST_LOG=trace` on
`countMatches` is over 64 MB. Narrow to the targets you need
(`regicide::region_allocation=trace,regicide::coalescing=trace`) and write it
to a file rather than reading it through a tool.

### Rendering one visualization from a corpus input

`scripts/visualize` recompiles the module. When you already have the corpus
input, render directly:

```bash
cargo build --manifest-path regicide/Cargo.toml --release --example visualize \
    --features visualizer,test-utils,deserialize
regicide/target/release/examples/visualize --input $R/corpus/<module>-<NNNN>-u0-<N>.postcard \
    $R/viz/<bench>/<fn>.html
```

Pages for huge functions can be enormous (switch's hot function: 190 MB) —
note the size in the report.

Add `RUST_LOG=trace` for regicide's event log on the replay — the same
information `scripts/log` gives you, without compiling any Wasm:

```bash
cd ~/regalloc2/regicide
RUST_LOG=trace cargo run --all-features --example run -- \
    ../$R/corpus/bz2-0003-foo.postcard
```

Narrow it the same way as `WASMTIME_LOG`, e.g.
`RUST_LOG=regicide::region_allocation=trace`.

`--all-features` is the simplest way to get the `checker`, `deserialize`, `std`
and `trace-log` features the example needs; regicide's `dev-dependencies` enable
them for tests and examples already, so a plain
`cargo run --example run` also works but logs nothing at trace level.

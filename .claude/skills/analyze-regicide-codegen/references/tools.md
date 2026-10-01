# The tools

Every script lives in `~/regalloc2/scripts/`, takes `-h`, and is safe to read —
they are short, commented bash. This file is the summary; `-h` is the source of
truth if they drift.

All of them build Wasmtime or regicide on first use, and the first build of a
given cargo profile is slow (tens of minutes for Wasmtime). Prefer running them
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
  — the sibling skill's per-benchmark regression gate. It works unchanged on
  regicide data; pass `--baseline backtracking --candidate regicide`. Useful
  when you want a pass/fail answer rather than a ranking.

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
tree, `-n N` for more rows, `-r` for the sampling rate. Anything after `--` goes
to the Wasm program as its own arguments.

**Use `-t`.** The flat view is topped by Wasmtime's idle threads parked in the
async runtime — asleep, not working. The guest's own frames are under
`array_call_native`, so `-t -n 200` is the view you want.

Built with the `profiling` cargo profile (optimized, with line tables) and run
with `--profile=perfmap`, so the JIT-compiled Wasm frames symbolicate instead of
coming back as bare offsets.

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

Two caveats:

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
| `-f`, `--filter FUNC` | Only visualize functions whose name matches this POSIX extended regex (case-insensitive). **Use it** — a whole benchmark is hundreds of functions. |

Anything after `--` goes to `wasmtime compile`.

Works by compiling with `REGICIDE_DUMP` set, which captures each allocation's
input under `<out>/dumps/`, then rendering each with regicide's `visualize`
example. A single allocation that fails to render does not sink the
rest; its error goes to `<out>/<name>.log`.

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

**But prefer a per-module corpus for analysis work.** The whole-suite corpus is
a Wasmtime compile per module and the big ones (`sqlite3`, `spidermonkey`,
`shootout-switch`) are slow; a whole-suite run can exceed an hour and may not
finish. A single small module takes a couple of minutes. One directory per
module also keeps provenance unambiguous:

```bash
./scripts/make-corpus -c $R/corpus-heapsort \
    ~/sightglass/benchmarks/shootout/shootout-heapsort.wasm
```

Build the whole-suite corpus only if you actually want the suite-wide static
baseline, and treat it as a background job that may not land.

It exits nonzero if any module captured nothing, which usually means a cache hit
— the script passes `-C cache=n`, so this should not happen, but check for it.

Find the input for a function by name:

```bash
ls $R/corpus | grep -i <function-name>
```

**This only works for modules with a name section.** Most sightglass benchmarks
are stripped, so captures come out as `benchmark-0004-u0-3.postcard` — `u0-3`
is Wasmtime's placeholder for module 0, function 3, not a symbol. For those,
match by function *index* against the disassembly, or identify the function you
want by its counts (the hot function is usually the one with by far the most
moves or spills):

```bash
cargo run --manifest-path regicide/Cargo.toml --all-features --example run \
    -q -- -q $R/corpus-heapsort | grep -B1 -E "moves [0-9]{3,}"
```

### Replaying with `run.rs`

```
cargo run --manifest-path regicide/Cargo.toml --all-features --example run -- [OPTIONS] <path>...
```

A path is either a directory (every `*.postcard` and `*.json` directly in it,
sorted, so two runs over one corpus can be diffed) or a single input file. The
encoding is detected from the file's contents rather than its name, so a corpus
captured before `make-corpus` wrote postcard -- such as
`reports/codegen-analysis-001/corpus-keccak/` -- still replays unchanged.

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

Output per input is the allocation (unless `-q`) followed by a counts line:

```
moves 12  spills 3  reloads 5  remats 0  critical-edges 1  slots 4
```

These are **the numbers to diff between two builds**, and the per-corpus
`=== totals ===` line at the end aggregates them. `moves` is register-register
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

Add `RUST_LOG=trace` for regicide's event log on the replay — the same
information `scripts/log` gives you, without compiling any Wasm:

```bash
cd ~/regalloc2/regicide
RUST_LOG=trace cargo run --all-features --example run -- \
    ../reports/codegen-analysis-001/corpus/bz2-0003-foo.postcard
```

Narrow it the same way as `WASMTIME_LOG`, e.g.
`RUST_LOG=regicide::region_allocation=trace`.

`--all-features` is the simplest way to get the `checker`, `deserialize`, `std`
and `trace-log` features the example needs; regicide's `dev-dependencies` enable
them for tests and examples already, so a plain
`cargo run --example run` also works but logs nothing at trace level.

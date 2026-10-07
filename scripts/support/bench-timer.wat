;; A `bench` module for sightglass benchmarks run outside sightglass: times
;; `bench.start` to `bench.end` with the monotonic clock and writes
;; `bench-ns <nanoseconds>` to stderr. Used by `scripts/time-cwasm` with
;; `wasmtime run --preload bench=...`.
(module
  (import "wasi_snapshot_preview1" "clock_time_get" (func $clock (param i32 i64 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (global $t0 (mut i64) (i64.const 0))
  (func (export "start")
    (drop (call $clock (i32.const 1) (i64.const 1) (i32.const 0)))
    (global.set $t0 (i64.load (i32.const 0))))
  (func (export "end")
    (local $d i64) (local $p i32)
    (drop (call $clock (i32.const 1) (i64.const 1) (i32.const 0)))
    (local.set $d (i64.sub (i64.load (i32.const 0)) (global.get $t0)))
    ;; Format as `bench-ns <d>\n`, right to left, ending at byte 96.
    (local.set $p (i32.const 95))
    (i32.store8 (local.get $p) (i32.const 10))
    (loop $l
      (local.set $p (i32.sub (local.get $p) (i32.const 1)))
      (i32.store8 (local.get $p) (i32.add (i32.const 48) (i32.wrap_i64 (i64.rem_u (local.get $d) (i64.const 10)))))
      (local.set $d (i64.div_u (local.get $d) (i64.const 10)))
      (br_if $l (i64.ne (local.get $d) (i64.const 0))))
    (local.set $p (i32.sub (local.get $p) (i32.const 9)))
    (i64.store (local.get $p) (i64.const 0x2d68636e6562)) ;; "bench-" little-endian
    (i32.store8 offset=6 (local.get $p) (i32.const 110)) ;; n
    (i32.store8 offset=7 (local.get $p) (i32.const 115)) ;; s
    (i32.store8 offset=8 (local.get $p) (i32.const 32))
    (i32.store (i32.const 8) (local.get $p))
    (i32.store (i32.const 12) (i32.sub (i32.const 96) (local.get $p)))
    (drop (call $write (i32.const 2) (i32.const 8) (i32.const 1) (i32.const 16)))))

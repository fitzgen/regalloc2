#!/usr/bin/env python3
"""Static analyses of `scripts/disassemble ... -- --funcs all --addresses` output.

Subcommands (all arm64; stack accesses are counted with the narrow
`(ldr|ldur|str|stur) reg, [sp...` filter, so prologue `stp`/`ldp` pairs are
excluded; see references/analyzing-allocation.md):

  mix BASE.asm CAND.asm [--top N]
      Per-function instructions / stack accesses / reg-reg movs under both
      allocators, sorted by delta. Functions are keyed by full name
      (`wasm[0]::function[N]...`), so components' several core modules do
      not collide.

  extract FILE.asm N [--strip]
      Print `function[N]` (first core module that has it). --strip drops
      addresses and masks branch targets, so two builds' copies of one
      function can be diffed for real changes.

  same OLD.asm NEW.asm
      Which functions differ between two builds of one module, ignoring
      addresses. The fastest way to tell "the allocator changed this code"
      from "this code just moved".

  loops FUNC.asm
      Each backward branch's loop, innermost (smallest) first: instructions,
      stack accesses, movs, line offset of the head (`head%64`), cache lines
      spanned, and the heap offsets it touches, which is a fingerprint that
      matches a loop across allocators when sizes and order differ.

  slot2slot BASE.asm CAND.asm [--top N]
      Count stack-slot-to-stack-slot copies (`ldr xN,[sp,#a]` followed by
      `str xN,[sp,#b]`, a != b) per function. Ion rarely emits them; in
      regicide they mark block parameters whose slot differs from their
      argument's (tinygo `countMatches`: 2,107 vs 49 at regicide `288da68`, 641 at
      `cf70ddd`).
"""

import argparse
import hashlib
import re
import sys

HEADER = re.compile(r"^(?:[0-9a-f]{8} )?(wasm\[\d+\]::\S+):")
INSN = re.compile(r"\s+([0-9a-f]+):\s+(\S+)\s*(.*)")
STACK = re.compile(r"(ldr|ldur|str|stur)$")
STACK_ARGS = re.compile(r"[xwqdsbh]\d+, \[sp")
REGMOV = re.compile(r"[xw]\d+, [xw]\d+$")
BRANCH = re.compile(r"(b|b\.\S+|cbn?z|tbn?z|bl|adr)$")


def functions(path):
    """{name: [(addr, op, args)]} in file order."""
    out, cur = {}, None
    for line in open(path):
        h = HEADER.match(line)
        if h:
            cur = h.group(1)
            out[cur] = []
            continue
        m = INSN.match(line)
        if m and cur is not None:
            out[cur].append((int(m.group(1), 16), m.group(2), m.group(3).strip()))
    return out


def stats(insns):
    sp = sum(1 for _, op, a in insns if STACK.match(op) and STACK_ARGS.match(a))
    mv = sum(1 for _, op, a in insns if op == "mov" and REGMOV.match(a))
    return len(insns), sp, mv


def normalized(insns):
    return [f"{op} {re.sub(r'#0x[0-9a-f]+$', '#T', a) if BRANCH.match(op) else a}"
            for _, op, a in insns]


def short(name):
    return name[:70]


def cmd_mix(args):
    b, c = functions(args.base), functions(args.cand)
    rows = [(n, stats(b.get(n, [])), stats(c.get(n, []))) for n in set(b) | set(c)]
    tb = [sum(r[1][i] for r in rows) for i in range(3)]
    tc = [sum(r[2][i] for r in rows) for i in range(3)]
    print(f"totals insts/[sp]/mov: base {tb[0]}/{tb[1]}/{tb[2]}  cand {tc[0]}/{tc[1]}/{tc[2]}")
    for i, label in enumerate(("insts", "[sp]", "mov")):
        print(f"\n-- top {args.top} by cand-base delta in {label}")
        for n, x, y in sorted(rows, key=lambda r: r[2][i] - r[1][i], reverse=True)[: args.top]:
            print(f"  {short(n):70} base {x[0]:6}/{x[1]:5}/{x[2]:5}  "
                  f"cand {y[0]:6}/{y[1]:5}/{y[2]:5}  {y[i] - x[i]:+d}")
        print(f"-- bottom {args.top // 2}")
        for n, x, y in sorted(rows, key=lambda r: r[2][i] - r[1][i])[: args.top // 2]:
            print(f"  {short(n):70} base {x[0]:6}/{x[1]:5}/{x[2]:5}  "
                  f"cand {y[0]:6}/{y[1]:5}/{y[2]:5}  {y[i] - x[i]:+d}")


def cmd_extract(args):
    on = False
    for line in open(args.file):
        h = HEADER.match(line)
        if h:
            if on:
                return
            on = f"function[{args.n}]" in h.group(1).split("::")[1]
            if on:
                print(h.group(1))
            continue
        if not on or not line.strip():
            continue
        if args.strip:
            m = INSN.match(line)
            if m:
                print("  " + normalized([(0, m.group(2), m.group(3).strip())])[0])
        else:
            print(line.rstrip())


def cmd_same(args):
    a, b = functions(args.old), functions(args.new)
    digest = lambda f: hashlib.md5("\n".join(normalized(f)).encode()).hexdigest()
    differ = [n for n in a if n in b and digest(a[n]) != digest(b[n])]
    only = sorted(set(a) ^ set(b))
    print(f"{len(a)} functions; {len(differ)} differ; {len(only)} in only one build")
    for n in sorted(differ, key=lambda n: -abs(len(b[n]) - len(a[n]))):
        print(f"  {short(n):70} {len(a[n]):6} -> {len(b[n]):6} insts ({len(b[n]) - len(a[n]):+d})")


def cmd_loops(args):
    funcs = functions(args.file)
    insns = [i for f in funcs.values() for i in f] if funcs else []
    if not insns:
        insns = [(int(m.group(1), 16), m.group(2), m.group(3).strip())
                 for m in map(INSN.match, open(args.file)) if m]
    loops = set()
    for addr, op, a in insns:
        t = re.search(r"#0x([0-9a-f]+)$", a)
        if t and (op.startswith("b") and op not in ("bl", "blr", "br")
                  or op in ("cbz", "cbnz", "tbz", "tbnz")):
            target = int(t.group(1), 16)
            if target <= addr:
                loops.add((target, addr))
    for s, e in sorted(loops, key=lambda r: r[1] - r[0])[: args.top]:
        body = [i for i in insns if s <= i[0] <= e]
        n, sp, mv = stats(body)
        offs = sorted({o for _, _, a in body if "[sp" not in a
                       for o in re.findall(r"\[x\d+, #(0x[0-9a-f]+)\]", a)},
                      key=lambda x: int(x, 16))
        print(f"{s:x}..{e:x}  insts {n:4}  [sp] {sp:3}  mov {mv:3}  head%64 {s % 64:2}  "
              f"lines {e // 64 - s // 64 + 1}  heap-offsets {' '.join(offs[:8])}")


def slot_copies(insns):
    n, prev = 0, None
    for _, op, a in insns:
        m = re.match(r"(x\d+), \[sp(?:, #(0x[0-9a-f]+))?\]$", a)
        if op in ("str", "stur") and m and prev and prev[0] == m.group(1) and prev[1] != m.group(2):
            n += 1
        prev = (m.group(1), m.group(2)) if op in ("ldr", "ldur") and m else None
    return n


def cmd_slot2slot(args):
    b, c = functions(args.base), functions(args.cand)
    rows = [(n, slot_copies(b.get(n, [])), slot_copies(c.get(n, []))) for n in set(b) | set(c)]
    print(f"slot-to-slot copies: base {sum(r[1] for r in rows)}, cand {sum(r[2] for r in rows)}")
    for n, x, y in sorted(rows, key=lambda r: -r[2])[: args.top]:
        if x or y:
            print(f"  {short(n):70} base {x:5}  cand {y:5}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0],
                                 formatter_class=argparse.RawDescriptionHelpFormatter,
                                 epilog=__doc__.split("\n", 2)[2])
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("mix"); p.add_argument("base"); p.add_argument("cand")
    p.add_argument("--top", type=int, default=12); p.set_defaults(f=cmd_mix)
    p = sub.add_parser("extract"); p.add_argument("file"); p.add_argument("n")
    p.add_argument("--strip", action="store_true"); p.set_defaults(f=cmd_extract)
    p = sub.add_parser("same"); p.add_argument("old"); p.add_argument("new")
    p.set_defaults(f=cmd_same)
    p = sub.add_parser("loops"); p.add_argument("file")
    p.add_argument("--top", type=int, default=15); p.set_defaults(f=cmd_loops)
    p = sub.add_parser("slot2slot"); p.add_argument("base"); p.add_argument("cand")
    p.add_argument("--top", type=int, default=10); p.set_defaults(f=cmd_slot2slot)
    args = ap.parse_args()
    args.f(args)


if __name__ == "__main__":
    main()

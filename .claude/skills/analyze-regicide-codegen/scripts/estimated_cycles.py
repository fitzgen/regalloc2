#!/usr/bin/env python3
"""Estimated cycles over a path through one function's disassembly.

usage: estimated_cycles.py FUNC.asm START-END [START-END ...]

The same cost model as regicide's `run` example (`EstimatedCycles` in
`regicide/examples/run.rs`), applied to arm64 disassembly:

    estimated = reg_operands + reg_moves + remats
                + 5 * (stack_operands + spills + reloads)

`run` multiplies each block's counts by its weight (10x per enclosing loop,
times the block frequency's `estimate_weight`: 0.05 cold, 1 normal, 10 hot).
A path through disassembly has no loop depth or frequency attached, so this
counts every instruction at weight 1. That is right for one block, or one
loop's in-loop path compared with the same path under the other allocator.
Do not sum across blocks of different loop depth with it.

Each range is inclusive hex addresses; the path is their concatenation.
Per instruction:
  - a register-register `mov`: 1 (a move);
  - a `mov` of an immediate: 1 (a remat; Cranelift's constant remats are
    these, though a constant the source itself materialized looks the same);
  - a load or store whose address is `[sp`: 5 (a spill, reload or stack
    operand; `stp`/`ldp` prologue saves count too, two values each);
  - anything else: 1 per register operand.
"""
import re
import sys

STACK_COST = 5

lines = open(sys.argv[1]).read().splitlines()
insts = []
for l in lines:
    m = re.match(r'\s+([0-9a-f]+):\s+(\S+)\s*(.*)$', l)
    if m:
        insts.append((int(m.group(1), 16), m.group(2), m.group(3)))
REG = re.compile(r'\b([xw]\d+|xzr|wzr)\b')
tot = dict(n=0, regops=0, moves=0, remats=0, stack=0)
for r in sys.argv[2:]:
    a, b = (int(x, 16) for x in r.split('-'))
    for addr, op, args in insts:
        if not (a <= addr <= b):
            continue
        tot['n'] += 1
        if '[sp' in args:
            # A pair moves two values.
            tot['stack'] += 2 if op in ('stp', 'ldp') else 1
        elif op == 'mov' and re.fullmatch(r'[xw]\d+, [xw]\d+', args):
            tot['moves'] += 1
        elif op in ('mov', 'movz', 'movn') and '#' in args:
            tot['remats'] += 1
        else:
            tot['regops'] += len(REG.findall(args))
est = tot['regops'] + tot['moves'] + tot['remats'] + STACK_COST * tot['stack']
print(f"insts {tot['n']}  reg-operands {tot['regops']}  reg-reg moves {tot['moves']}  "
      f"immediate movs {tot['remats']}  stack accesses {tot['stack']}  "
      f"estimated-cycles {est}")

---
name: waveql
description: A high-performance CLI for querying VCD/FST waveform files.
license: MIT
compatibility: opencode
metadata:
  audience: maintainers
  workflow: github
---

# WaveQL Skill

## Purpose
WaveQL allows you to inspect hardware waveforms (VCD/FST) as structured data.

## When to use
- Verify HDL behavior
- Check timing violations
- Debug signal transitions
- Validate reset / clock / enable sequences

## Commands
- `waveql list <file>` → discover signals
- `waveql changes <file>` → signal transitions
- `waveql edges <file>` → rising/falling edges
- `waveql sample <file> --at <time>` → value at time
- `waveql ascii <file>` → human-readable waveform

## Output
Prefer `--format json` when reasoning.
Use `ascii` only for human confirmation.

## Example reasoning pattern
1. List signals
2. Sample critical signals at key times
3. Check edges relative to clock
4. Conclude pass/fail

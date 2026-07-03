---
name: waveql
description: Protocol-first waveform analysis for VCD/FST files with raw inspection, binding, and semantic protocol checks.
license: MIT
compatibility: opencode
metadata:
  audience: maintainers
  workflow: github
---

# WaveQL Skill

## Purpose
WaveQL is a waveform analysis engine for VCD/FST files. Use it to infer **hardware behavior and protocol correctness**, not just raw signal toggles.

The right mental model is:
1. identify the behavior you want to verify,
2. bind signals to roles,
3. ask WaveQL for semantic evidence,
4. only then zoom into raw changes/edges/samples when you need proof.

## Core principle
Prefer **behavioral interpretation** over **bit-level inspection**.

- Good questions: “Did this handshake complete?”, “Was data stable while valid was asserted?”, “Did SPI transactions end cleanly?”, “Did reset release in the right order?”, “Did backpressure create a stall?”
- Raw edges and samples are supporting evidence, not the final conclusion.

## When to use
Use WaveQL whenever you need to debug Verilog/SystemVerilog behavior from a waveform:

- protocol and handshake bugs
- ready/valid or request/ack timing issues
- SPI-style serial transfers
- reset, clock, enable, and stall sequencing
- FSM or state-transition checks
- throughput drops, dropped transfers, truncated transactions
- ambiguous or suspicious waveform regions that need evidence-backed interpretation

## Command families

### 1) Raw inspection: `inspect`
Use these when you need to discover signals or zoom in on a suspicious time window.

- `waveql inspect list <file>`
  - Discover all signals in the waveform.
  - Use this first when you do not yet know the relevant paths.

- `waveql inspect changes <file> -s <signals> --from <time> --to <time> --format json`
  - Show value changes over a bounded time window.
  - Best for locating when something started to drift, stall, or glitch.

- `waveql inspect edges <file> -s <signal> -t rising|falling|both --from <time> --to <time> --format json`
  - Detect edge timing precisely.
  - Useful for clocks, strobes, handshakes, and serial sampling.

- `waveql inspect sample <file> -s <signal> --at <time> --format json`
  - Sample a single point in time.
  - Use to confirm a value at a specific event boundary.

- `waveql inspect ascii <file> -s <signals> --from <time> --to <time>`
  - Render a human-readable waveform slice.
  - Use only to visually confirm a region after you already know where to look.

### 2) Protocol discovery and semantic analysis: `protocol`
Use these when you want WaveQL to reason about hardware behavior at the protocol level.

- `waveql protocol list --format text|json|table`
  - List available protocol schemas.
  - Start here when you are unsure what semantic analyzers exist.

- `waveql protocol bind <file> -p <protocol> -b ROLE=SIGNAL -b ROLE=SIGNAL --format json`
  - Bind logical roles to concrete waveform paths.
  - Let WaveQL validate the binding and surface missing roles or candidate matches.
  - Do not guess bindings when the waveform names are unclear; use the binding result as evidence.

- `waveql protocol analyze <file> -p <protocol> -b ROLE=SIGNAL -b ROLE=SIGNAL --from <time> --to <time> --format json`
  - Run a semantic protocol check over a waveform window.
  - Use this to confirm or reject behavior such as handshakes, stalls, payload stability, transaction completion, or protocol violations.

## Output guidance
- Prefer `--format json` when reasoning or when you need structured evidence.
- Use `--format text` for a quick human-readable summary.
- Use `--format table` when you want CSV-like rows for comparison.
- Use `ascii` only as a last-mile visual check.

## Suggested analysis workflow
1. Identify the behavior under test.
2. Run `inspect list` to find the candidate signals.
3. Bind protocol roles if the problem is semantic, not just electrical.
4. Run `protocol analyze` on a tight time window around the symptom.
5. Read the reported violations/handshakes/stalls as behavioral evidence.
6. Drill into the exact time window with `changes`, `edges`, or `sample`.
7. Report the bug in protocol terms first, then mention the raw waveform proof.

## Hardware-debug framing
When interpreting results, prefer statements like:

- “valid stayed high while ready remained low, so the transfer stalled”
- “data changed before the handshake completed, so the payload was unstable”
- “the SPI transaction ended mid-word, so the transfer was truncated”
- “the signal transitioned, but the semantic event did not complete”

Avoid stopping at descriptions such as “the signal rose” unless that edge directly proves the hardware behavior you care about.

## Practical defaults
- Time strings may use units such as `ns`, `ps`, `us`, `ms`, or `s`.
- Default time unit is `ns` when omitted.
- If signal naming is unclear, resolve the protocol binding before you spend time on raw waveform inspection.

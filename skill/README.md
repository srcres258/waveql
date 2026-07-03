# WaveQL Skill for AI Agents

## Install
```bash
cargo install --path .
```

## Use in Agent
Paste `install_prompt.txt` into your Agent session.

## What this skill is for
WaveQL is not just for reading signal levels. It is for turning VCD/FST traces into hardware-behavior evidence:

- raw exploration with `inspect`
- protocol discovery with `protocol list`
- role binding with `protocol bind`
- semantic checks with `protocol analyze`

## Recommended workflow
1. Start with the behavior question, not the signal list.
2. Use `inspect list` to discover candidate signals.
3. Bind roles for the protocol you care about.
4. Run `protocol analyze` on a tight time window.
5. Use raw `changes` / `edges` / `sample` only to prove the semantic conclusion.

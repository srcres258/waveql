# WaveQL

A high-performance CLI for querying VCD/FST waveform files.

Designed for AI Agents (DeepSeek / Claude / GPT) and human engineers alike.

## Features

- **VCD** and **FST** waveform support (via `vcd` and `wellen` crates)
- **JSON output** for Agent reasoning
- **ASCII waveform** rendering for human visual inspection
- **Table output** for CSV/piped workflows
- Wildcard signal matching (`top.*`)
- Edge detection (rising/falling/both)
- Time-based sampling and range queries
- Zero C dependencies, fully cross-platform

## Quick Start

```bash
cargo install --path .
```

## Usage

### List signals
```bash
waveql list my_waveform.vcd
```

### Extract changes
```bash
waveql changes my_waveform.fst --signals top.clk,top.data --from 0ns --to 500ns --format json
```

### Detect edges
```bash
waveql edges my_waveform.vcd --signal top.clk --type rising
```

### Sample at time
```bash
waveql sample my_waveform.fst --signal top.data --at 237ns
```

### ASCII waveform
```bash
waveql ascii my_waveform.vcd --signals top.clk,top.en --from 0ns --to 100ns
```

## Output Formats

| Format | Use case |
|--------|----------|
| `json` | AI Agent reasoning (default) |
| `text` | Human-readable plain text |
| `table` | Pipe-delimited, CSV-friendly |

## AI Agent Skill

See `skill/` directory for:
- `SKILL.md` — Agent instruction card
- `install_prompt.txt` — One-paste installation prompt

## Build

```bash
cargo build --release
```

## License

MIT

# WaveQL

A high-performance CLI for querying VCD/FST waveform files.

Designed for AI agents and RTL engineers alike.

## Features

- **Raw inspection** via `inspect` for signal lists, changes, edges, samples, and ASCII slices
- **Protocol analysis** via `protocol` for schema discovery, role binding, and semantic checks
- Built-in analyzers for **valid/ready** and **SPI** handshakes
- **Wildcard signal matching** (`top.*`) with a searchable signal index
- **JSON / text / table** output for machine and human workflows
- Time-based sampling and range queries
- Zero C dependencies, fully cross-platform

## Quick Start

```bash
cargo install --path .
```

## Usage

### Inspect signals
```bash
waveql inspect list my_waveform.vcd
```

### Analyze protocol behavior
```bash
waveql protocol analyze my_waveform.vcd --protocol valid_ready --set valid=top.valid --set ready=top.ready
```

### Legacy flat commands still work
```bash
waveql changes my_waveform.fst --signals top.clk,top.data --from 0ns --to 500ns --format json
```

### ASCII waveform
```bash
waveql inspect ascii my_waveform.vcd --signals top.clk,top.en --from 0ns --to 100ns
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

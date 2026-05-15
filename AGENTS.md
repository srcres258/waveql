# AGENTS.md — WaveQL

A single-crate Rust CLI for querying VCD/FST hardware waveform files. Designed for both AI agents and human engineers.

## Commands

```bash
cargo build                    # debug build
cargo build --release          # release (also works as: cargo run --release)
cargo check                    # fast type/lint check (no codegen)
cargo test                     # run tests (none exist yet)
cargo fmt                      # format code
cargo clippy                   # lint
cargo install --path .         # install binary as `waveql`
```

## Architecture

```
src/
  main.rs        — clap 4 CLI: list|changes|edges|sample|ascii subcommands, dispatch
  lib.rs         — core types: Waveform, Timescale, SignalData, SignalInfo, TimeUnit,
                   parse_time_str(), FileFormat enum
  loader.rs      — detect format from extension (.vcd / .fst), dispatch to parser
  vcd_impl.rs    — VCD parser using `vcd` crate (v0.6)
  fst_impl.rs    — FST parser using `wellen` crate (v0.20)
  query.rs       — Query enum, output types (ListOutput, ChangesOutput, etc.) with Serialize
  evaluator.rs   — evaluate() all query types; always returns String (JSON)
  output/
    mod.rs
    json.rs      — delegates straight to evaluator::evaluate
    text.rs      — human-readable; round-trips through evaluator JSON, then parses it back
    table.rs     — pipe-delimited CSV; same round-trip pattern as text
  error.rs       — WaveqlError enum via thiserror, includes From<WellenError>
```

## Design quirks (important)

### Evaluator always returns JSON strings

`evaluator::evaluate()` returns `Result<String, WaveqlError>` — always a JSON string.  
The `text` and `table` output modules call `evaluator::evaluate()`, then `serde_json::from_str()` the result to extract values for formatting.  
**When adding a new query type, you must implement rendering in `evaluator`, `output/json`, `output/text`, and `output/table`.**

### Unified internal representation

Both VCD and FST are parsed into the same `Waveform` struct (`lib.rs`). The `FileFormat` enum tracks the source. All queries operate on this unified representation — no per-format code paths in evaluator or output.

### Signal path format

- VCD: scope hierarchy is flattened to dot-separated paths (`top.sub.signal`), with `module:` prefix stripped
- FST: uses `wellen::Var::full_name()` directly
- Wildcard matching supports a single `*` (prefix* suffix pattern), e.g., `top.*` matches all signals under `top.`

### Time format

- CLI accepts: `0ns`, `100ns`, `10us`, `1ms`, `1s`, `500ps`, etc.
- Default unit if omitted: `ns`
- All times stored internally as u64 in the file's timescale units
- Default timescale: `1 ns`

### Output formats

| Format  | CLI flag          | Default? | Description                     |
|---------|-------------------|----------|---------------------------------|
| `json`  | `--format json`   | **yes**  | Structured, for agent reasoning |
| `text`  | `--format text`   | no       | Human-readable plain text       |
| `table` | `--format table`  | no       | Pipe-delimited, CSV-friendly    |

## Key dependencies

| Crate           | Version | Purpose                     |
|-----------------|---------|-----------------------------|
| `clap`          | 4       | CLI (derive mode)           |
| `serde`/`serde_json` | 1 | Serialization / deserialization |
| `vcd`           | 0.6     | VCD waveform parsing         |
| `wellen`        | 0.20    | FST waveform parsing         |
| `thiserror`     | 2       | Error type derivation        |
| `anyhow`        | 1       | (available, not heavily used) |
| `unicode-width` | 0.2     | (available for ASCII output)  |

## Testing

There are **no tests yet**. The `examples/demo.sh` script serves as a smoke test — it creates a VCD file inline and runs all subcommands. To run it:

```bash
cargo build && bash examples/demo.sh
```

New code should include tests. VCD testing is straightforward (create inline VCD strings). FST testing requires binary FST fixture files.

## Style notes

- Uses Rust 2021 edition
- Section headers in source use `// ── Name ──` convention
- `pub mod` declarations are in `lib.rs`; all modules are public
- Error handling: functions return `Result<T, WaveqlError>`, errors printed in `main()` via `eprintln!`
- No `unsafe` code

## Other

- `.deepseek/` directory is ignored by git (project may have been generated/assisted by AI)
- `skill/` directory contains agent-facing documentation, not source code
- Only two commits in repo history; project is in early stage

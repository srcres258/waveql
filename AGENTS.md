# AGENTS.md — WaveQL

Single-crate Rust CLI for querying VCD/FST waveform files.

## Fast commands

```bash
cargo build
cargo build --release
cargo test
cargo fmt --all --check
cargo clippy --all-targets
cargo install --path .
cargo build && bash examples/demo.sh
```

## What matters here

- `src/main.rs` is only argument parsing/dispatch. Real orchestration lives in `src/planner/mod.rs`.
- `Session::open -> plan -> execute` is the main flow; it loads the waveform, resolves time strings and wildcards, then renders output.
- The loader chooses parsers by file extension only: `.vcd` or `.fst`.
- `src/backend/` owns the shared waveform trait/types; `src/vcd_impl.rs` and `src/fst_impl.rs` adapt the file formats into that model.
- `src/output/` and `src/report/` are the rendering layers; protocol-aware commands are handled in `Session`, not `evaluator`.

## CLI quirks

- Both grouped commands (`inspect`, `protocol`) and legacy flat commands still exist.
- `list` is always JSON; `ascii` is always plain text, regardless of `--format`.
- Supported output formats are `json` (default), `text`, and `table`.
- Wildcards are single `*` patterns only; empty signal lists resolve to “all signals”.
- `--set ROLE=SIGNAL` is the binding format for `bind` / `analyze`.
- Protocol analysis currently ships with `valid_ready` and `spi`; unknown protocol names fail at execution time.

## Testing / verification

- Tests live in module files; `cargo test` is the normal verification step now.
- CI runs `cargo fmt --all --check`, `cargo test --verbose`, and `cargo clippy --all-targets` on stable.

## Style

- Rust 2021, no `unsafe`.
- Source files use `// ── Name ──` section headers.
- `lib.rs` re-exports backend types for backward compatibility; prefer existing re-exports over new public aliases.

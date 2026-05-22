use clap::{Parser, Subcommand, ValueEnum};
use std::process;
use waveql::error::WaveqlError;
use waveql::loader;
use waveql::output;
use waveql::query::{EdgeType, OutputFormat, Query, TimeRange};

/// WaveQL — query VCD/FST waveform files like a database.
#[derive(Parser)]
#[command(name = "waveql", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List all signals in a waveform file (JSON output)
    List {
        /// Path to waveform file (.vcd or .fst)
        file: String,
    },

    /// Extract signal changes in a time range
    Changes {
        /// Path to waveform file
        file: String,

        /// Signals to query (comma-separated, supports * wildcard: top.*)
        #[arg(short, long, value_delimiter = ',')]
        signals: Option<Vec<String>>,

        /// Start time (e.g., "0ns", "100ns")
        #[arg(long, default_value = "0ns")]
        from: String,

        /// End time (e.g., "500ns", "10us")
        #[arg(long)]
        to: Option<String>,

        /// Output format
        #[arg(long, default_value = "json")]
        format: FormatArg,
    },

    /// Detect rising/falling edges on a signal
    Edges {
        /// Path to waveform file
        file: String,

        /// Signal to detect edges on
        #[arg(short, long)]
        signal: String,

        /// Edge type: rising, falling, or both
        #[arg(short, long, default_value = "both")]
        r#type: EdgeTypeArg,

        /// Start time
        #[arg(long, default_value = "0ns")]
        from: String,

        /// End time
        #[arg(long)]
        to: Option<String>,

        /// Output format
        #[arg(long, default_value = "json")]
        format: FormatArg,
    },

    /// Sample a signal value at a specific time
    Sample {
        /// Path to waveform file
        file: String,

        /// Signal to sample
        #[arg(short, long)]
        signal: String,

        /// Time to sample at (e.g., "237ns")
        #[arg(long)]
        at: String,

        /// Output format
        #[arg(long, default_value = "json")]
        format: FormatArg,
    },

    /// Render an ASCII waveform view (for humans)
    Ascii {
        /// Path to waveform file
        file: String,

        /// Signals to display (comma-separated, supports * wildcard)
        #[arg(short, long, value_delimiter = ',')]
        signals: Option<Vec<String>>,

        /// Start time
        #[arg(long, default_value = "0ns")]
        from: String,

        /// End time
        #[arg(long)]
        to: Option<String>,
    },
}

#[derive(Clone, ValueEnum)]
enum FormatArg {
    Json,
    Text,
    Table,
}

impl From<FormatArg> for OutputFormat {
    fn from(f: FormatArg) -> Self {
        match f {
            FormatArg::Json => OutputFormat::Json,
            FormatArg::Text => OutputFormat::Text,
            FormatArg::Table => OutputFormat::Table,
        }
    }
}

#[derive(Clone, ValueEnum)]
enum EdgeTypeArg {
    Rising,
    Falling,
    Both,
}

impl From<EdgeTypeArg> for EdgeType {
    fn from(e: EdgeTypeArg) -> Self {
        match e {
            EdgeTypeArg::Rising => EdgeType::Rising,
            EdgeTypeArg::Falling => EdgeType::Falling,
            EdgeTypeArg::Both => EdgeType::Both,
        }
    }
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::List { file } => {
            run_list(&file)
        }
        Commands::Changes {
            file,
            signals,
            from,
            to,
            format,
        } => {
            run_changes(&file, signals.unwrap_or_default(), &from, to.as_deref(), format.into())
        }
        Commands::Edges {
            file,
            signal,
            r#type,
            from,
            to,
            format,
        } => {
            run_edges(&file, &signal, r#type.into(), &from, to.as_deref(), format.into())
        }
        Commands::Sample {
            file,
            signal,
            at,
            format,
        } => {
            run_sample(&file, &signal, &at, format.into())
        }
        Commands::Ascii {
            file,
            signals,
            from,
            to,
        } => {
            run_ascii(&file, signals.unwrap_or_default(), &from, to.as_deref())
        }
    };

    match result {
        Ok(output) => {
            println!("{output}");
        }
        Err(e) => {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    }
}

// ── Command runners ──────────────────────────────────────────────

fn run_list(file: &str) -> Result<String, WaveqlError> {
    let waveform = loader::load(file)?;
    let query = Query::List;
    output::json::render(&waveform, &query, file)
}

fn run_changes(
    file: &str,
    signals: Vec<String>,
    from: &str,
    to: Option<&str>,
    format: OutputFormat,
) -> Result<String, WaveqlError> {
    let mut waveform = loader::load(file)?;
    let from_ts = waveql::parse_time_str(from, &waveform.timescale)?;
    let to_ts = to
        .map(|t| waveql::parse_time_str(t, &waveform.timescale))
        .transpose()?;

    let resolved = waveform.resolve_signals(&signals)?;
    waveform.load_signals(&resolved)?;

    let query = Query::Changes {
        signals,
        range: TimeRange {
            from: Some(from_ts),
            to: to_ts,
        },
    };

    render(&waveform, &query, file, format)
}

fn run_edges(
    file: &str,
    signal: &str,
    edge_type: EdgeType,
    from: &str,
    to: Option<&str>,
    format: OutputFormat,
) -> Result<String, WaveqlError> {
    let mut waveform = loader::load(file)?;
    let from_ts = waveql::parse_time_str(from, &waveform.timescale)?;
    let to_ts = to
        .map(|t| waveql::parse_time_str(t, &waveform.timescale))
        .transpose()?;

    waveform.load_signal(signal)?;

    let query = Query::Edges {
        signal: signal.to_string(),
        edge_type,
        range: TimeRange {
            from: Some(from_ts),
            to: to_ts,
        },
    };

    render(&waveform, &query, file, format)
}

fn run_sample(
    file: &str,
    signal: &str,
    at: &str,
    format: OutputFormat,
) -> Result<String, WaveqlError> {
    let mut waveform = loader::load(file)?;
    let at_ts = waveql::parse_time_str(at, &waveform.timescale)?;

    waveform.load_signal(signal)?;

    let query = Query::Sample {
        signal: signal.to_string(),
        at: at_ts,
    };

    render(&waveform, &query, file, format)
}

fn run_ascii(
    file: &str,
    signals: Vec<String>,
    from: &str,
    to: Option<&str>,
) -> Result<String, WaveqlError> {
    let mut waveform = loader::load(file)?;
    let from_ts = waveql::parse_time_str(from, &waveform.timescale)?;
    let to_ts = to
        .map(|t| waveql::parse_time_str(t, &waveform.timescale))
        .transpose()?;

    let resolved = waveform.resolve_signals(&signals)?;
    waveform.load_signals(&resolved)?;

    let query = Query::Ascii {
        signals,
        range: TimeRange {
            from: Some(from_ts),
            to: to_ts,
        },
    };

    output::text::render(&waveform, &query)
}

fn render(
    waveform: &waveql::Waveform,
    query: &Query,
    file: &str,
    format: OutputFormat,
) -> Result<String, WaveqlError> {
    match format {
        OutputFormat::Json => output::json::render(waveform, query, file),
        OutputFormat::Text => output::text::render(waveform, query),
        OutputFormat::Table => output::table::render(waveform, query),
    }
}

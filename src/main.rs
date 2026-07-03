use clap::{Parser, Subcommand, ValueEnum};
use std::process;
use waveql::planner::{
    format_protocols_table, format_protocols_text, CommandSpec, PlanRequest, Session,
};
use waveql::protocol::spi::SpiAnalyzer;
use waveql::protocol::valid_ready::ValidReadyAnalyzer;
use waveql::protocol::ProtocolCatalog;
use waveql::query::{EdgeType, OutputFormat};

// ── Top-level CLI ──────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "waveql", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

// ── Subcommands ────────────────────────────────────────────────────

#[derive(Subcommand)]
enum Commands {
    // ── Responsibility-based grouped commands ──
    /// Inspect signals: list, changes, edges, sample, ascii
    #[command(alias = "i")]
    Inspect {
        #[command(subcommand)]
        cmd: InspectCommands,
    },

    /// Protocol discovery, binding, and analysis
    #[command(alias = "proto")]
    Protocol {
        #[command(subcommand)]
        cmd: ProtocolCommands,
    },

    // ── Legacy flat commands (preserved for backward compatibility) ──
    #[command(alias = "ls")]
    List { file: String },

    Changes {
        file: String,
        #[arg(short, long, value_delimiter = ',')]
        signals: Option<Vec<String>>,
        #[arg(long, default_value = "0ns")]
        from: String,
        #[arg(long)]
        to: Option<String>,
        #[arg(long, default_value = "json")]
        format: FormatArg,
    },

    Edges {
        file: String,
        #[arg(short, long)]
        signal: String,
        #[arg(short, long, default_value = "both")]
        r#type: EdgeTypeArg,
        #[arg(long, default_value = "0ns")]
        from: String,
        #[arg(long)]
        to: Option<String>,
        #[arg(long, default_value = "json")]
        format: FormatArg,
    },

    Sample {
        file: String,
        #[arg(short, long)]
        signal: String,
        #[arg(long)]
        at: String,
        #[arg(long, default_value = "json")]
        format: FormatArg,
    },

    Ascii {
        file: String,
        #[arg(short, long, value_delimiter = ',')]
        signals: Option<Vec<String>>,
        #[arg(long, default_value = "0ns")]
        from: String,
        #[arg(long)]
        to: Option<String>,
    },

    /// List available protocol schemas.
    Protocols {
        #[arg(long, default_value = "text")]
        format: FormatArg,
    },

    /// Bind logical protocol roles to concrete signal paths.
    Bind {
        file: String,

        /// Protocol name (e.g., "valid_ready", "spi").
        #[arg(short, long)]
        protocol: String,

        /// Role-to-signal binding in ROLE=SIGNAL form (repeatable).
        #[arg(short, long = "set", value_parser = parse_binding)]
        bindings: Vec<(String, String)>,

        #[arg(long, default_value = "json")]
        format: FormatArg,
    },

    /// Run a protocol analyzer against a waveform.
    Analyze {
        file: String,

        /// Protocol name (e.g., "valid_ready").
        #[arg(short, long)]
        protocol: String,

        /// Role-to-signal binding in ROLE=SIGNAL form (repeatable).
        #[arg(short, long = "set", value_parser = parse_binding)]
        bindings: Vec<(String, String)>,

        #[arg(long, default_value = "0ns")]
        from: String,

        #[arg(long)]
        to: Option<String>,

        #[arg(long, default_value = "json")]
        format: FormatArg,
    },
}

// ── Grouped subcommand enums ───────────────────────────────────────

#[derive(Subcommand)]
enum InspectCommands {
    /// List all signals in the waveform file.
    List { file: String },

    /// Show value changes for signals within a time range.
    Changes {
        file: String,
        #[arg(short, long, value_delimiter = ',')]
        signals: Option<Vec<String>>,
        #[arg(long, default_value = "0ns")]
        from: String,
        #[arg(long)]
        to: Option<String>,
        #[arg(long, default_value = "json")]
        format: FormatArg,
    },

    /// Detect rising/falling/both edges for a signal.
    Edges {
        file: String,
        #[arg(short, long)]
        signal: String,
        #[arg(short, long, default_value = "both")]
        r#type: EdgeTypeArg,
        #[arg(long, default_value = "0ns")]
        from: String,
        #[arg(long)]
        to: Option<String>,
        #[arg(long, default_value = "json")]
        format: FormatArg,
    },

    /// Sample a signal at a single point in time.
    Sample {
        file: String,
        #[arg(short, long)]
        signal: String,
        #[arg(long)]
        at: String,
        #[arg(long, default_value = "json")]
        format: FormatArg,
    },

    /// Render an ASCII waveform for signals.
    Ascii {
        file: String,
        #[arg(short, long, value_delimiter = ',')]
        signals: Option<Vec<String>>,
        #[arg(long, default_value = "0ns")]
        from: String,
        #[arg(long)]
        to: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProtocolCommands {
    /// List available protocol schemas.
    List {
        #[arg(long, default_value = "text")]
        format: FormatArg,
    },

    /// Bind logical protocol roles to concrete signal paths.
    Bind {
        file: String,

        /// Protocol name (e.g., "valid_ready", "spi").
        #[arg(short, long)]
        protocol: String,

        /// Role-to-signal binding in ROLE=SIGNAL form (repeatable).
        #[arg(short, long = "set", value_parser = parse_binding)]
        bindings: Vec<(String, String)>,

        #[arg(long, default_value = "json")]
        format: FormatArg,
    },

    /// Run a protocol analyzer against a waveform.
    Analyze {
        file: String,

        /// Protocol name (e.g., "valid_ready").
        #[arg(short, long)]
        protocol: String,

        /// Role-to-signal binding in ROLE=SIGNAL form (repeatable).
        #[arg(short, long = "set", value_parser = parse_binding)]
        bindings: Vec<(String, String)>,

        #[arg(long, default_value = "0ns")]
        from: String,

        #[arg(long)]
        to: Option<String>,

        #[arg(long, default_value = "json")]
        format: FormatArg,
    },
}

// ── Helpers ────────────────────────────────────────────────────────

fn parse_binding(s: &str) -> Result<(String, String), String> {
    if let Some((role, signal)) = s.split_once('=') {
        let role = role.trim();
        let signal = signal.trim();
        if role.is_empty() || signal.is_empty() {
            return Err("binding must be ROLE=SIGNAL (both non-empty)".into());
        }
        Ok((role.to_string(), signal.to_string()))
    } else {
        Err("binding must be ROLE=SIGNAL form".into())
    }
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

// ── Dispatch ───────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        // ── Grouped: inspect ──
        Commands::Inspect { cmd } => {
            let (file, command, format) = dispatch_inspect(cmd);
            let request = PlanRequest::new(file, command, format);
            execute_or_exit(&request);
        }

        // ── Grouped: protocol ──
        Commands::Protocol { cmd } => match cmd {
            ProtocolCommands::List { format } => {
                let fmt: OutputFormat = format.clone().into();
                match run_protocols(fmt) {
                    Ok(output) => println!("{output}"),
                    Err(e) => {
                        eprintln!("Error: {e}");
                        process::exit(1);
                    }
                }
            }
            ProtocolCommands::Bind {
                file,
                protocol,
                bindings,
                format,
            } => {
                let request = PlanRequest::new(
                    file.clone(),
                    CommandSpec::Bind {
                        protocol_name: protocol.clone(),
                        bindings: bindings.clone(),
                    },
                    format.clone().into(),
                );
                execute_or_exit(&request);
            }
            ProtocolCommands::Analyze {
                file,
                protocol,
                bindings,
                from,
                to,
                format,
            } => {
                let request = PlanRequest::new(
                    file.clone(),
                    CommandSpec::Analyze {
                        protocol_name: protocol.clone(),
                        bindings: bindings.clone(),
                        from: from.clone(),
                        to: to.clone(),
                    },
                    format.clone().into(),
                );
                execute_or_exit(&request);
            }
        },

        // ── Legacy flat commands ──
        Commands::Protocols { format } => {
            let fmt: OutputFormat = format.clone().into();
            match run_protocols(fmt) {
                Ok(output) => println!("{output}"),
                Err(e) => {
                    eprintln!("Error: {e}");
                    process::exit(1);
                }
            }
        }
        _ => {
            let (file, command, format) = dispatch_legacy(&cli.command);
            let request = PlanRequest::new(file, command, format);
            execute_or_exit(&request);
        }
    }
}

fn execute_or_exit(request: &PlanRequest) {
    match run(request) {
        Ok(output) => println!("{output}"),
        Err(e) => {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    }
}

// ── Dispatch helpers ───────────────────────────────────────────────

/// Convert inspect subcommand to the canonical (file, CommandSpec, OutputFormat) triplet.
fn dispatch_inspect(cmd: &InspectCommands) -> (String, CommandSpec, OutputFormat) {
    match cmd {
        InspectCommands::List { file } => (file.clone(), CommandSpec::List, OutputFormat::Json),
        InspectCommands::Changes {
            file,
            signals,
            from,
            to,
            format,
        } => (
            file.clone(),
            CommandSpec::Changes {
                signals: signals.clone().unwrap_or_default(),
                from: from.clone(),
                to: to.clone(),
            },
            format.clone().into(),
        ),
        InspectCommands::Edges {
            file,
            signal,
            r#type,
            from,
            to,
            format,
        } => (
            file.clone(),
            CommandSpec::Edges {
                signal: signal.clone(),
                edge_type: r#type.clone().into(),
                from: from.clone(),
                to: to.clone(),
            },
            format.clone().into(),
        ),
        InspectCommands::Sample {
            file,
            signal,
            at,
            format,
        } => (
            file.clone(),
            CommandSpec::Sample {
                signal: signal.clone(),
                at: at.clone(),
            },
            format.clone().into(),
        ),
        InspectCommands::Ascii {
            file,
            signals,
            from,
            to,
        } => (
            file.clone(),
            CommandSpec::Ascii {
                signals: signals.clone().unwrap_or_default(),
                from: from.clone(),
                to: to.clone(),
            },
            OutputFormat::Text,
        ),
    }
}

/// Convert legacy flat command to the canonical (file, CommandSpec, OutputFormat) triplet.
fn dispatch_legacy(cmd: &Commands) -> (String, CommandSpec, OutputFormat) {
    match cmd {
        Commands::List { file } => (file.clone(), CommandSpec::List, OutputFormat::Json),
        Commands::Changes {
            file,
            signals,
            from,
            to,
            format,
        } => (
            file.clone(),
            CommandSpec::Changes {
                signals: signals.clone().unwrap_or_default(),
                from: from.clone(),
                to: to.clone(),
            },
            format.clone().into(),
        ),
        Commands::Edges {
            file,
            signal,
            r#type,
            from,
            to,
            format,
        } => (
            file.clone(),
            CommandSpec::Edges {
                signal: signal.clone(),
                edge_type: r#type.clone().into(),
                from: from.clone(),
                to: to.clone(),
            },
            format.clone().into(),
        ),
        Commands::Sample {
            file,
            signal,
            at,
            format,
        } => (
            file.clone(),
            CommandSpec::Sample {
                signal: signal.clone(),
                at: at.clone(),
            },
            format.clone().into(),
        ),
        Commands::Ascii {
            file,
            signals,
            from,
            to,
        } => (
            file.clone(),
            CommandSpec::Ascii {
                signals: signals.clone().unwrap_or_default(),
                from: from.clone(),
                to: to.clone(),
            },
            OutputFormat::Text,
        ),
        Commands::Bind {
            file,
            protocol,
            bindings,
            format,
        } => (
            file.clone(),
            CommandSpec::Bind {
                protocol_name: protocol.clone(),
                bindings: bindings.clone(),
            },
            format.clone().into(),
        ),
        Commands::Analyze {
            file,
            protocol,
            bindings,
            from,
            to,
            format,
        } => (
            file.clone(),
            CommandSpec::Analyze {
                protocol_name: protocol.clone(),
                bindings: bindings.clone(),
                from: from.clone(),
                to: to.clone(),
            },
            format.clone().into(),
        ),
        Commands::Protocols { .. } | Commands::Inspect { .. } | Commands::Protocol { .. } => {
            unreachable!()
        }
    }
}

fn run_protocols(format: OutputFormat) -> Result<String, waveql::error::WaveqlError> {
    let mut catalog = ProtocolCatalog::new();
    catalog.register(Box::new(ValidReadyAnalyzer::new()));
    catalog.register(Box::new(SpiAnalyzer::new()));
    let output = waveql::evaluator::evaluate_protocols(&catalog)?;
    match format {
        OutputFormat::Json => Ok(serde_json::to_string_pretty(&output)?),
        OutputFormat::Text => format_protocols_text(&output),
        OutputFormat::Table => format_protocols_table(&output),
    }
}

fn run(request: &PlanRequest) -> Result<String, waveql::error::WaveqlError> {
    let mut session = Session::open(&request.file)?;
    let plan = session.plan(request)?;
    session.execute(&plan, request.format)
}

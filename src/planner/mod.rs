// ── Planner Module ─────────────────────────────────────────────────
//
// Query planning and command normalization.
//
// The planner is intentionally simple — no SQL-style optimizer, no
// cost-based plan selection.  It normalises raw CLI requests into
// explicit, executable [`Plan`] objects and provides a single
// `Session` entry point that hides backend loading, signal resolution,
// time parsing, and output-format dispatch from the CLI layer.
//
// # Entry points
//
// 1.  [`Session::open`] — loads a waveform file and builds the signal
//     index.
// 2.  [`Session::plan`] — normalises a [`PlanRequest`] by resolving
//     time strings, signal patterns (including wildcards), and loading
//     the required signal data.
// 3.  [`Session::execute`] — runs the resolved [`Plan`] through the
//     existing evaluator/output pipeline.
//
// After this module was introduced the CLI (`main.rs`) became a pure
// argument-parser — all orchestration lives here.

use crate::backend::WaveformBackend;
use crate::error::WaveqlError;
use crate::index::SignalIndex;
use crate::protocol::spi::SpiAnalyzer;
use crate::protocol::valid_ready::ValidReadyAnalyzer;
use crate::protocol::{BindingKind, ProtocolCatalog, RoleBinding};
use crate::query::{
    AnalyzeOutput, BindOutput, BindingCandidateOutput, BindingSuggestionOutput, BoundRoleInfo,
    DerivationKind, EdgePolarity, EdgeType, OutputFormat, ProtocolsOutput, Query, TimeRange,
};

// ── PlanRequest ────────────────────────────────────────────────────

/// Raw CLI intent before the session resolves signals and times.
///
/// All string fields (signal names, time specifications) are exactly
/// what the user typed.  [`Session::plan`] will parse and validate them.
#[derive(Debug, Clone)]
pub struct PlanRequest {
    /// Path to the waveform file.
    pub file: String,

    /// Which subcommand was invoked and with what arguments.
    pub command: CommandSpec,

    /// Output format requested by the user.
    ///
    /// For `List` and `Ascii` this is ignored at execution time
    /// (List is always JSON, Ascii is always plain text), but the
    /// field is carried through so that all requests share the same
    /// request envelope.
    pub format: OutputFormat,
}

impl PlanRequest {
    pub fn new(file: String, command: CommandSpec, format: OutputFormat) -> Self {
        PlanRequest {
            file,
            command,
            format,
        }
    }
}

/// Per-command payload.
///
/// Variants carry raw, unresolved strings — the session handles all
/// parsing, resolution, and loading.
#[derive(Debug, Clone)]
pub enum CommandSpec {
    /// List all signals in the file.
    List,

    /// Show value changes for one or more signals within a time range.
    Changes {
        signals: Vec<String>,
        from: String,
        to: Option<String>,
    },

    /// Detect rising/falling/both edges for a single signal.
    Edges {
        signal: String,
        edge_type: EdgeType,
        from: String,
        to: Option<String>,
    },

    /// Sample a signal at a single point in time.
    Sample { signal: String, at: String },

    /// Render an ASCII waveform for one or more signals.
    Ascii {
        signals: Vec<String>,
        from: String,
        to: Option<String>,
    },

    /// Derive protocol-agnostic temporal events.
    Events {
        derivation: EventsDerivationSpec,
        from: String,
        to: Option<String>,
    },

    /// List available protocol schemas.
    Protocols,

    /// Bind logical protocol roles to concrete signal paths.
    Bind {
        protocol_name: String,
        bindings: Vec<(String, String)>,
    },

    /// Run a protocol analyzer against the waveform.
    Analyze {
        protocol_name: String,
        bindings: Vec<(String, String)>,
        from: String,
        to: Option<String>,
    },
}

/// Raw specification for which derivation to perform.
#[derive(Debug, Clone)]
pub enum EventsDerivationSpec {
    Edges {
        signals: Vec<String>,
        polarity: EdgePolarity,
    },
    Handshake {
        signal_a: String,
        signal_b: String,
    },
    Stalls {
        signals: Vec<String>,
        min_duration: u64,
    },
    StateTransitions {
        signal: String,
    },
}

// ── Plan ───────────────────────────────────────────────────────────

/// Normalized, fully resolved execution plan.
///
/// Produced by [`Session::plan`] from a [`PlanRequest`].  All time
/// values are in waveform-native units and signal patterns have been
/// resolved into concrete paths.  The plan carries no references into
/// the session — it is pure data and can be inspected, logged, or
/// serialized independently.
#[derive(Debug, Clone)]
pub enum Plan {
    List,

    Changes {
        signals: Vec<String>,
        range: TimeRange,
    },

    Edges {
        signal: String,
        edge_type: EdgeType,
        range: TimeRange,
    },

    Sample {
        signal: String,
        at: u64,
    },

    Ascii {
        signals: Vec<String>,
        range: TimeRange,
    },

    Events {
        derivation: DerivationKind,
        range: TimeRange,
    },

    Protocols,

    Bind {
        protocol_name: String,
        bindings: Vec<(String, String)>,
    },

    Analyze {
        protocol_name: String,
        bindings: Vec<(String, String)>,
        range: TimeRange,
    },
}

// ── Session ────────────────────────────────────────────────────────

/// Thin orchestration layer that owns a loaded waveform and its signal
/// index.
///
/// `Session` is the single entry point from the CLI.  It encapsulates
/// backend loading, signal-index construction, time-parsing, signal
/// resolution/loading, and output-format dispatch.
///
/// # Example (conceptual)
///
/// ```ignore
/// let mut session = Session::open("fixture.vcd")?;
/// let plan = session.plan(&request)?;
/// let output = session.execute(&plan, OutputFormat::Json)?;
/// println!("{output}");
/// ```
pub struct Session {
    /// The loaded waveform backend (owned, boxed).
    pub waveform: Box<dyn WaveformBackend>,

    /// Searchable signal index built from the backend at open time.
    pub index: SignalIndex,

    /// Original file path (used for metadata in output messages).
    pub file_name: String,

    /// Runtime registry of available protocol plugins.
    pub catalog: ProtocolCatalog,
}

impl Session {
    // ── Construction ──────────────────────────────────────────

    /// Open a waveform file, build the signal index, and return a
    /// ready-to-use session.
    ///
    /// The file extension (`.vcd` / `.fst`) determines the backend.
    /// No signal data is loaded — only metadata and the index are
    /// populated.  Data loading happens lazily in [`Session::plan`].
    pub fn open(file: &str) -> Result<Self, WaveqlError> {
        let waveform = crate::loader::load(file)?;
        let index = SignalIndex::build(&*waveform);
        let mut catalog = ProtocolCatalog::new();
        catalog.register(Box::new(ValidReadyAnalyzer::new()));
        catalog.register(Box::new(SpiAnalyzer::new()));
        Ok(Session {
            waveform,
            index,
            file_name: file.to_string(),
            catalog,
        })
    }

    // ── Request normalisation ─────────────────────────────────

    /// Normalise a [`PlanRequest`] into an executable [`Plan`].
    ///
    /// This method:
    /// 1. Parses time strings against the backend timescale.
    /// 2. Resolves signal patterns (wildcards, empty list → all).
    /// 3. Calls `load_signals` on the backend to ensure the data is
    ///    available for evaluation.
    ///
    /// After this call the `Plan` is fully resolved and ready for
    /// [`Session::execute`].
    pub fn plan(&mut self, request: &PlanRequest) -> Result<Plan, WaveqlError> {
        match &request.command {
            CommandSpec::List => Ok(Plan::List),

            CommandSpec::Changes { signals, from, to } => {
                let ts = self.waveform.timescale().clone();
                let from_ts = crate::parse_time_str(from, &ts)?;
                let to_ts = to
                    .as_ref()
                    .map(|t| crate::parse_time_str(t, &ts))
                    .transpose()?;
                let resolved = self.waveform.resolve_signals(signals)?;
                self.waveform.load_signals(&resolved)?;
                Ok(Plan::Changes {
                    signals: signals.clone(),
                    range: TimeRange {
                        from: Some(from_ts),
                        to: to_ts,
                    },
                })
            }

            CommandSpec::Edges {
                signal,
                edge_type,
                from,
                to,
            } => {
                let ts = self.waveform.timescale().clone();
                let from_ts = crate::parse_time_str(from, &ts)?;
                let to_ts = to
                    .as_ref()
                    .map(|t| crate::parse_time_str(t, &ts))
                    .transpose()?;
                self.waveform.load_signals(&[signal.to_string()])?;
                Ok(Plan::Edges {
                    signal: signal.clone(),
                    edge_type: *edge_type,
                    range: TimeRange {
                        from: Some(from_ts),
                        to: to_ts,
                    },
                })
            }

            CommandSpec::Sample { signal, at } => {
                let ts = self.waveform.timescale().clone();
                let at_ts = crate::parse_time_str(at, &ts)?;
                self.waveform.load_signals(&[signal.to_string()])?;
                Ok(Plan::Sample {
                    signal: signal.clone(),
                    at: at_ts,
                })
            }

            CommandSpec::Ascii { signals, from, to } => {
                let ts = self.waveform.timescale().clone();
                let from_ts = crate::parse_time_str(from, &ts)?;
                let to_ts = to
                    .as_ref()
                    .map(|t| crate::parse_time_str(t, &ts))
                    .transpose()?;
                let resolved = self.waveform.resolve_signals(signals)?;
                self.waveform.load_signals(&resolved)?;
                Ok(Plan::Ascii {
                    signals: signals.clone(),
                    range: TimeRange {
                        from: Some(from_ts),
                        to: to_ts,
                    },
                })
            }

            CommandSpec::Events {
                derivation,
                from,
                to,
            } => {
                let ts = self.waveform.timescale().clone();
                let from_ts = crate::parse_time_str(from, &ts)?;
                let to_ts = to
                    .as_ref()
                    .map(|t| crate::parse_time_str(t, &ts))
                    .transpose()?;
                let dk = match derivation {
                    EventsDerivationSpec::Edges { signals, polarity } => {
                        let resolved = self.waveform.resolve_signals(signals)?;
                        self.waveform.load_signals(&resolved)?;
                        DerivationKind::Edges {
                            signals: signals.clone(),
                            polarity: *polarity,
                        }
                    }
                    EventsDerivationSpec::Handshake { signal_a, signal_b } => {
                        self.waveform
                            .load_signals(&[signal_a.clone(), signal_b.clone()])?;
                        DerivationKind::Handshake {
                            signal_a: signal_a.clone(),
                            signal_b: signal_b.clone(),
                        }
                    }
                    EventsDerivationSpec::Stalls {
                        signals,
                        min_duration,
                    } => {
                        let resolved = self.waveform.resolve_signals(signals)?;
                        self.waveform.load_signals(&resolved)?;
                        DerivationKind::Stalls {
                            signals: signals.clone(),
                            min_duration: *min_duration,
                        }
                    }
                    EventsDerivationSpec::StateTransitions { signal } => {
                        self.waveform.load_signals(std::slice::from_ref(signal))?;
                        DerivationKind::StateTransitions {
                            signal: signal.clone(),
                        }
                    }
                };
                Ok(Plan::Events {
                    derivation: dk,
                    range: TimeRange {
                        from: Some(from_ts),
                        to: to_ts,
                    },
                })
            }

            CommandSpec::Protocols => Ok(Plan::Protocols),

            CommandSpec::Bind {
                protocol_name,
                bindings,
            } => Ok(Plan::Bind {
                protocol_name: protocol_name.clone(),
                bindings: bindings.clone(),
            }),

            CommandSpec::Analyze {
                protocol_name,
                bindings,
                from,
                to,
            } => {
                let ts = self.waveform.timescale().clone();
                let from_ts = crate::parse_time_str(from, &ts)?;
                let to_ts = to
                    .as_ref()
                    .map(|t| crate::parse_time_str(t, &ts))
                    .transpose()?;
                // Load all bound signals for analysis
                let signal_paths: Vec<String> = bindings.iter().map(|(_, s)| s.clone()).collect();
                if !signal_paths.is_empty() {
                    self.waveform.load_signals(&signal_paths)?;
                }
                Ok(Plan::Analyze {
                    protocol_name: protocol_name.clone(),
                    bindings: bindings.clone(),
                    range: TimeRange {
                        from: Some(from_ts),
                        to: to_ts,
                    },
                })
            }
        }
    }

    // ── Execution ─────────────────────────────────────────────

    /// Execute a resolved [`Plan`] through the evaluator and output
    /// pipeline.
    ///
    /// The `format` parameter controls JSON / text / table output for
    /// commands that support it.  `List` is always JSON and `Ascii` is
    /// always plain text, regardless of `format`.
    pub fn execute(&self, plan: &Plan, format: OutputFormat) -> Result<String, WaveqlError> {
        match plan {
            Plan::List => {
                // List is always JSON — preserved from before the planner.
                crate::output::json::render(&*self.waveform, &Query::List, &self.file_name)
            }

            Plan::Changes { signals, range } => {
                let q = Query::Changes {
                    signals: signals.clone(),
                    range: *range,
                };
                render_by_format(&*self.waveform, &q, &self.file_name, format)
            }

            Plan::Edges {
                signal,
                edge_type,
                range,
            } => {
                let q = Query::Edges {
                    signal: signal.clone(),
                    edge_type: *edge_type,
                    range: *range,
                };
                render_by_format(&*self.waveform, &q, &self.file_name, format)
            }

            Plan::Sample { signal, at } => {
                let q = Query::Sample {
                    signal: signal.clone(),
                    at: *at,
                };
                render_by_format(&*self.waveform, &q, &self.file_name, format)
            }

            Plan::Ascii { signals, range } => {
                // ASCII is always plain text — format flag is ignored.
                let q = Query::Ascii {
                    signals: signals.clone(),
                    range: *range,
                };
                crate::output::text::render(&*self.waveform, &q)
            }

            Plan::Events { derivation, range } => {
                let q = Query::Events {
                    derivation: derivation.clone(),
                    range: *range,
                };
                render_by_format(&*self.waveform, &q, &self.file_name, format)
            }

            Plan::Protocols => {
                let output = crate::evaluator::evaluate_protocols(&self.catalog)?;
                match format {
                    OutputFormat::Json => Ok(serde_json::to_string_pretty(&output)?),
                    OutputFormat::Text => format_protocols_text(&output),
                    OutputFormat::Table => format_protocols_table(&output),
                }
            }

            Plan::Bind {
                protocol_name,
                bindings,
            } => {
                let plugin = self
                    .catalog
                    .get(protocol_name)
                    .ok_or_else(|| WaveqlError::ProtocolNotFound(protocol_name.clone()))?;

                let role_bindings: Vec<RoleBinding> = bindings
                    .iter()
                    .map(|(role, signal)| RoleBinding::user_specified(role, signal))
                    .collect();

                let validation = plugin.validate_bindings(&role_bindings, &self.index);

                let bound_roles: Vec<BoundRoleInfo> = role_bindings
                    .iter()
                    .map(|b| BoundRoleInfo {
                        role: b.role.clone(),
                        signal: b.signal_path.clone(),
                        binding_kind: match &b.kind {
                            BindingKind::UserSpecified => "user_specified".into(),
                            BindingKind::PatternMatched => "pattern_matched".into(),
                            BindingKind::Suggested { .. } => "suggested".into(),
                        },
                    })
                    .collect();

                let suggestion_outputs: Vec<BindingSuggestionOutput> = validation
                    .suggestions
                    .iter()
                    .map(|s| BindingSuggestionOutput {
                        role: s.role.clone(),
                        candidates: s
                            .candidates
                            .iter()
                            .map(|c| BindingCandidateOutput {
                                signal_path: c.signal_path.clone(),
                                confidence: c.confidence,
                                reason: c.reason.clone(),
                            })
                            .collect(),
                    })
                    .collect();

                let output = BindOutput {
                    protocol: protocol_name.clone(),
                    is_valid: validation.is_valid,
                    missing_roles: validation.missing_roles.clone(),
                    bindings: bound_roles,
                    suggestions: suggestion_outputs,
                    warnings: validation.warnings.clone(),
                };
                match format {
                    OutputFormat::Json => Ok(serde_json::to_string_pretty(&output)?),
                    OutputFormat::Text => format_bind_text(&output),
                    OutputFormat::Table => format_bind_table(&output),
                }
            }

            Plan::Analyze {
                protocol_name,
                bindings,
                range,
            } => {
                let plugin = self
                    .catalog
                    .get(protocol_name)
                    .ok_or_else(|| WaveqlError::ProtocolNotFound(protocol_name.clone()))?;

                let role_bindings: Vec<RoleBinding> = bindings
                    .iter()
                    .map(|(role, signal)| RoleBinding::user_specified(role, signal))
                    .collect();

                // Validate bindings before analysis
                let validation = plugin.validate_bindings(&role_bindings, &self.index);
                if !validation.is_valid {
                    let missing = validation.missing_roles.join(", ");
                    return Err(WaveqlError::BindingError(format!(
                        "Cannot analyze: missing required roles: {missing}"
                    )));
                }

                let from = range.from.unwrap_or(0);
                let to = range.to.unwrap_or(u64::MAX);
                let bounds = crate::trace::TimeBound::new(from, to)
                    .map_err(|e| WaveqlError::Other(format!("invalid time range: {e}")))?;

                let signal_paths: Vec<String> = bindings.iter().map(|(_, s)| s.clone()).collect();
                let slice_req =
                    crate::trace::TraceSliceRequest::new(&*self.waveform, signal_paths, bounds);
                let slice = slice_req.build()?;

                // Run protocol-specific analysis via the analyzer
                let output = if protocol_name == "valid_ready" {
                    let analyzer = ValidReadyAnalyzer::new();
                    let report = analyzer.analyze(&slice, &role_bindings);
                    AnalyzeOutput {
                        protocol: report.protocol,
                        summary: crate::query::AnalyzeSummary {
                            total_handshakes: report.summary.total_handshakes,
                            total_violations: report.summary.total_violations,
                            total_stalls: report.summary.total_stalls,
                            pass: report.summary.pass,
                        },
                        handshakes: report.handshakes,
                        violations: report.violations,
                        stalls: report.stalls,
                        transfers: vec![],
                        cs_windows: vec![],
                    }
                } else if protocol_name == "spi" {
                    let analyzer = SpiAnalyzer::new();
                    let report = analyzer.analyze(&slice, &role_bindings);
                    AnalyzeOutput {
                        protocol: report.protocol,
                        summary: crate::query::AnalyzeSummary {
                            total_handshakes: report.summary.total_transfers,
                            total_violations: report.summary.total_violations,
                            total_stalls: report.summary.total_bits,
                            pass: report.summary.pass,
                        },
                        handshakes: vec![],
                        violations: report.violations,
                        stalls: vec![],
                        transfers: report.transfers,
                        cs_windows: report.cs_windows,
                    }
                } else {
                    return Err(WaveqlError::Other(format!(
                        "analyzer for protocol '{}' not implemented",
                        protocol_name
                    )));
                };

                match format {
                    OutputFormat::Json => Ok(serde_json::to_string_pretty(&output)?),
                    OutputFormat::Text => format_analyze_text(&output),
                    OutputFormat::Table => format_analyze_table(&output),
                }
            }
        }
    }
}

// ── Private helper ─────────────────────────────────────────────────

fn render_by_format(
    waveform: &dyn WaveformBackend,
    query: &Query,
    file_name: &str,
    format: OutputFormat,
) -> Result<String, WaveqlError> {
    match format {
        OutputFormat::Json => crate::output::json::render(waveform, query, file_name),
        OutputFormat::Text => crate::output::text::render(waveform, query),
        OutputFormat::Table => crate::output::table::render(waveform, query),
    }
}

pub fn format_protocols_text(output: &ProtocolsOutput) -> Result<String, WaveqlError> {
    let mut out = String::new();
    if output.protocols.is_empty() {
        out.push_str("No protocol plugins registered.\n");
    } else {
        out.push_str(&format!(
            "{:<20} {:<8} {:<8} {}\n",
            "Protocol", "Req", "Opt", "Description"
        ));
        out.push_str(&format!("{:-<20} {:-<8} {:-<8} {:-<30}\n", "", "", "", ""));
        for p in &output.protocols {
            out.push_str(&format!(
                "{:<20} {:<8} {:<8} {}\n",
                p.name, p.required_role_count, p.optional_role_count, p.description,
            ));
        }
    }
    Ok(out)
}

pub fn format_protocols_table(output: &ProtocolsOutput) -> Result<String, WaveqlError> {
    let mut out = String::new();
    out.push_str("name|required_role_count|optional_role_count|description\n");
    for p in &output.protocols {
        out.push_str(&format!(
            "{}|{}|{}|{}\n",
            p.name, p.required_role_count, p.optional_role_count, p.description,
        ));
    }
    Ok(out)
}

pub(crate) fn format_bind_text(output: &BindOutput) -> Result<String, WaveqlError> {
    let mut out = String::new();
    out.push_str(&format!("Protocol: {}\n", output.protocol));
    out.push_str(&format!(
        "Validation: {}\n\n",
        if output.is_valid { "PASS" } else { "FAIL" }
    ));

    if !output.bindings.is_empty() {
        out.push_str(&format!("{:<15} {:<30} {:<15}\n", "Role", "Signal", "Kind"));
        out.push_str(&format!("{:-<15} {:-<30} {:-<15}\n", "", "", ""));
        for b in &output.bindings {
            out.push_str(&format!(
                "{:<15} {:<30} {:<15}\n",
                b.role, b.signal, b.binding_kind
            ));
        }
        out.push('\n');
    }

    if !output.missing_roles.is_empty() {
        out.push_str(&format!(
            "Missing roles: {}\n",
            output.missing_roles.join(", ")
        ));
    }

    if !output.warnings.is_empty() {
        out.push_str("\nWarnings:\n");
        for w in &output.warnings {
            out.push_str(&format!("  - {}\n", w));
        }
    }

    if !output.suggestions.is_empty() {
        out.push_str("\nSuggestions:\n");
        for s in &output.suggestions {
            out.push_str(&format!("  [{}]\n", s.role));
            for c in &s.candidates {
                out.push_str(&format!(
                    "    {} (confidence: {:.2}) — {}\n",
                    c.signal_path, c.confidence, c.reason
                ));
            }
        }
    }
    Ok(out)
}

pub(crate) fn format_bind_table(output: &BindOutput) -> Result<String, WaveqlError> {
    let mut out = String::new();
    out.push_str("protocol|is_valid|role|signal|binding_kind\n");
    if output.bindings.is_empty() {
        out.push_str(&format!("{}|{}|||\n", output.protocol, output.is_valid));
    } else {
        for b in &output.bindings {
            out.push_str(&format!(
                "{}|{}|{}|{}|{}\n",
                output.protocol, output.is_valid, b.role, b.signal, b.binding_kind
            ));
        }
    }
    Ok(out)
}

fn format_analyze_text(output: &AnalyzeOutput) -> Result<String, WaveqlError> {
    if output.protocol == "spi" {
        return format_spi_text(output);
    }

    let mut out = String::new();
    out.push_str(&format!("Protocol: {}\n", output.protocol));
    out.push_str(&format!(
        "Verdict: {}\n",
        if output.summary.pass { "PASS" } else { "FAIL" }
    ));
    out.push_str(&format!(
        "Handshakes: {}  Violations: {}  Stalls: {}\n\n",
        output.summary.total_handshakes,
        output.summary.total_violations,
        output.summary.total_stalls,
    ));

    if !output.handshakes.is_empty() {
        out.push_str("Handshake Timeline:\n");
        out.push_str(&format!(
            "{:<10} {:<14} {:<20} {:<20}\n",
            "Time", "Phase", "Valid", "Ready"
        ));
        out.push_str(&format!(
            "{:-<10} {:-<14} {:-<20} {:-<20}\n",
            "", "", "", ""
        ));
        for h in &output.handshakes {
            out.push_str(&format!(
                "{:<10} {:<14} {:<20} {:<20}\n",
                h.time, h.phase, h.signal_a, h.signal_b,
            ));
        }
        out.push('\n');
    }

    if !output.violations.is_empty() {
        out.push_str("Violations:\n");
        for v in &output.violations {
            let sev = match v.severity.as_str() {
                "error" => "ERROR",
                "warning" => "WARN",
                _ => "INFO",
            };
            out.push_str(&format!(
                "  [{sev}] {kind}: {desc}\n",
                sev = sev,
                kind = v.kind,
                desc = v.description,
            ));
            out.push_str(&format!(
                "    Evidence: [{start}, {end}] signals: {signals}\n",
                start = v.evidence_start,
                end = v
                    .evidence_end
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "?".into()),
                signals = v.related_signals.join(", "),
            ));
        }
        out.push('\n');
    }

    if !output.stalls.is_empty() {
        out.push_str("Stalls:\n");
        for s in &output.stalls {
            out.push_str(&format!(
                "  {} held at {} for {} time units (since {})\n",
                s.signal, s.value, s.duration, s.since_time,
            ));
        }
    }

    if output.handshakes.is_empty() && output.violations.is_empty() && output.stalls.is_empty() {
        out.push_str("No events detected in the analysis window.\n");
    }

    Ok(out)
}

fn format_spi_text(output: &AnalyzeOutput) -> Result<String, WaveqlError> {
    let mut out = String::new();
    out.push_str(&format!("Protocol: {}\n", output.protocol));
    out.push_str(&format!(
        "Verdict: {}\n",
        if output.summary.pass { "PASS" } else { "FAIL" }
    ));
    out.push_str(&format!(
        "Transfers: {}  Bits: {}  Violations: {}\n\n",
        output.summary.total_handshakes,
        output.summary.total_stalls,
        output.summary.total_violations,
    ));

    if !output.transfers.is_empty() {
        out.push_str("SPI Transfers:\n");
        out.push_str(&format!(
            "{:<10} {:<10} {:<8} {:<20} {:<20}\n",
            "CS Start", "CS End", "Edges", "MOSI Words", "MISO Words"
        ));
        out.push_str(&format!(
            "{:-<10} {:-<10} {:-<8} {:-<20} {:-<20}\n",
            "", "", "", "", ""
        ));
        for t in &output.transfers {
            out.push_str(&format!(
                "{:<10} {:<10} {:<8} {:<20} {:<20}\n",
                t.cs_start,
                t.cs_end,
                t.sclk_edges,
                t.mosi_words.join(", "),
                t.miso_words.join(", "),
            ));
        }
        out.push('\n');
    }

    if !output.cs_windows.is_empty() {
        out.push_str("CS Windows:\n");
        for w in &output.cs_windows {
            out.push_str(&format!("  [{}, {}]\n", w.start, w.end));
        }
        out.push('\n');
    }

    if !output.violations.is_empty() {
        out.push_str("Violations:\n");
        for v in &output.violations {
            let sev = match v.severity.as_str() {
                "error" => "ERROR",
                "warning" => "WARN",
                _ => "INFO",
            };
            out.push_str(&format!(
                "  [{sev}] {kind}: {desc}\n",
                sev = sev,
                kind = v.kind,
                desc = v.description,
            ));
            out.push_str(&format!(
                "    Evidence: [{start}, {end}] signals: {signals}\n",
                start = v.evidence_start,
                end = v
                    .evidence_end
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "?".into()),
                signals = v.related_signals.join(", "),
            ));
        }
        out.push('\n');
    }

    if output.transfers.is_empty() && output.violations.is_empty() {
        out.push_str("No SPI transfers detected in the analysis window.\n");
    }

    Ok(out)
}

fn format_analyze_table(output: &AnalyzeOutput) -> Result<String, WaveqlError> {
    if output.protocol == "spi" {
        return format_spi_table(output);
    }

    let mut out = String::new();
    out.push_str("protocol|verdict|handshake_count|violation_count|stall_count\n");
    out.push_str(&format!(
        "{}|{}|{}|{}|{}\n",
        output.protocol,
        if output.summary.pass { "PASS" } else { "FAIL" },
        output.summary.total_handshakes,
        output.summary.total_violations,
        output.summary.total_stalls,
    ));
    out.push('\n');

    if !output.handshakes.is_empty() {
        out.push_str("kind|time|phase|signal_a|signal_b\n");
        for h in &output.handshakes {
            out.push_str(&format!(
                "handshake|{}|{}|{}|{}\n",
                h.time, h.phase, h.signal_a, h.signal_b,
            ));
        }
        out.push('\n');
    }

    if !output.violations.is_empty() {
        out.push_str("kind|severity|description|evidence_start|evidence_end|signals\n");
        for v in &output.violations {
            out.push_str(&format!(
                "violation|{}|{}|{}|{}|{}\n",
                v.severity,
                v.description,
                v.evidence_start,
                v.evidence_end.map(|e| e.to_string()).unwrap_or_default(),
                v.related_signals.join(","),
            ));
        }
        out.push('\n');
    }

    if !output.stalls.is_empty() {
        out.push_str("kind|signal|value|duration|since_time\n");
        for s in &output.stalls {
            out.push_str(&format!(
                "stall|{}|{}|{}|{}\n",
                s.signal, s.value, s.duration, s.since_time,
            ));
        }
    }

    Ok(out)
}

fn format_spi_table(output: &AnalyzeOutput) -> Result<String, WaveqlError> {
    let mut out = String::new();
    out.push_str("protocol|verdict|transfers|bits|violations\n");
    out.push_str(&format!(
        "{}|{}|{}|{}|{}\n",
        output.protocol,
        if output.summary.pass { "PASS" } else { "FAIL" },
        output.summary.total_handshakes,
        output.summary.total_stalls,
        output.summary.total_violations,
    ));
    out.push('\n');

    if !output.transfers.is_empty() {
        out.push_str("kind|cs_start|cs_end|sclk_edges|mosi_words|miso_words\n");
        for t in &output.transfers {
            out.push_str(&format!(
                "transfer|{}|{}|{}|{}|{}\n",
                t.cs_start,
                t.cs_end,
                t.sclk_edges,
                t.mosi_words.join(","),
                t.miso_words.join(","),
            ));
        }
        out.push('\n');
    }

    if !output.violations.is_empty() {
        out.push_str("kind|severity|description|evidence_start|evidence_end|signals\n");
        for v in &output.violations {
            out.push_str(&format!(
                "violation|{}|{}|{}|{}|{}\n",
                v.severity,
                v.description,
                v.evidence_start,
                v.evidence_end.map(|e| e.to_string()).unwrap_or_default(),
                v.related_signals.join(","),
            ));
        }
    }

    Ok(out)
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn make_session() -> Session {
        // Use a unique temp file so tests can run in parallel
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = format!("/tmp/waveql_planner_test_{id}.vcd");

        let vcd = r#"$date Today $end
$version test $end
$timescale 1ns $end
$scope module top $end
$var wire 1 ! clk $end
$var wire 1 " en $end
$var wire 8 # data $end
$upscope $end
$enddefinitions $end
#0 $dumpvars 0! 0" b00000000 # $end
#10 1!
#20 1"
#30 b10100011 #
#40 0!
#50 b01000010 #
#60 1!
#70 0"
#80 0!
#90 b00000000 #
#100 1! 1"
"#;
        fs::write(&path, vcd).unwrap();
        Session::open(&path).unwrap()
    }

    // ── PlanRequest / Plan types ──────────────────────────────

    #[test]
    fn test_plan_request_construction() {
        let req = PlanRequest::new("test.vcd".into(), CommandSpec::List, OutputFormat::Json);
        assert_eq!(req.file, "test.vcd");
        assert_eq!(req.format, OutputFormat::Json);
        match req.command {
            CommandSpec::List => {}
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn test_plan_debug_derives() {
        let plan = Plan::List;
        assert_eq!(format!("{:?}", plan), "List");

        let plan = Plan::Changes {
            signals: vec!["a".into()],
            range: TimeRange {
                from: Some(0),
                to: Some(100),
            },
        };
        let debug = format!("{:?}", plan);
        assert!(debug.contains("Changes"));
        assert!(debug.contains("a"));
    }

    // ── Session::open / plan / execute ────────────────────────

    #[test]
    fn test_session_open_builds_index() {
        let session = make_session();
        assert!(!session.index.is_empty());
        assert!(session.index.lookup_exact("top.clk").is_some());
    }

    #[test]
    fn test_plan_list_returns_plan_list() {
        let mut session = make_session();
        let req = PlanRequest::new(
            session.file_name.clone(),
            CommandSpec::List,
            OutputFormat::Json,
        );
        let plan = session.plan(&req).unwrap();
        assert!(matches!(&plan, Plan::List));
    }

    #[test]
    fn test_plan_changes_normalizes_times() {
        let mut session = make_session();
        let req = PlanRequest::new(
            session.file_name.clone(),
            CommandSpec::Changes {
                signals: vec!["top.clk".into()],
                from: "10ns".into(),
                to: Some("60ns".into()),
            },
            OutputFormat::Json,
        );

        let plan = session.plan(&req).unwrap();
        match plan {
            Plan::Changes { signals, range } => {
                assert_eq!(signals, vec!["top.clk"]);
                assert_eq!(range.from, Some(10));
                assert_eq!(range.to, Some(60));
            }
            _ => panic!("expected Plan::Changes"),
        }
    }

    #[test]
    fn test_plan_changes_invalid_signal_fails() {
        let mut session = make_session();
        let req = PlanRequest::new(
            session.file_name.clone(),
            CommandSpec::Changes {
                signals: vec!["top.nonexistent".into()],
                from: "0ns".into(),
                to: None,
            },
            OutputFormat::Json,
        );
        assert!(session.plan(&req).is_err());
    }

    #[test]
    fn test_plan_invalid_time_fails() {
        let mut session = make_session();
        let req = PlanRequest::new(
            session.file_name.clone(),
            CommandSpec::Changes {
                signals: vec!["top.clk".into()],
                from: "not_a_time".into(),
                to: None,
            },
            OutputFormat::Json,
        );
        assert!(session.plan(&req).is_err());
    }

    #[test]
    fn test_execute_list_produces_json() {
        let mut session = make_session();
        let req = PlanRequest::new(
            session.file_name.clone(),
            CommandSpec::List,
            OutputFormat::Json,
        );
        let plan = session.plan(&req).unwrap();
        let output = session.execute(&plan, OutputFormat::Json).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["total_signals"], 3);
        assert!(parsed["signals"].is_array());
    }

    #[test]
    fn test_execute_changes_json_produces_expected_structure() {
        let mut session = make_session();
        let req = PlanRequest::new(
            session.file_name.clone(),
            CommandSpec::Changes {
                signals: vec!["top.clk".into()],
                from: "0ns".into(),
                to: Some("100ns".into()),
            },
            OutputFormat::Json,
        );
        let plan = session.plan(&req).unwrap();
        let output = session.execute(&plan, OutputFormat::Json).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["query_type"], "changes");
        assert_eq!(parsed["signal_count"], 1);
        assert!(!parsed["events"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_execute_edges_json_produces_expected_edges() {
        let mut session = make_session();
        let req = PlanRequest::new(
            session.file_name.clone(),
            CommandSpec::Edges {
                signal: "top.clk".into(),
                edge_type: EdgeType::Rising,
                from: "0ns".into(),
                to: Some("100ns".into()),
            },
            OutputFormat::Json,
        );
        let plan = session.plan(&req).unwrap();
        let output = session.execute(&plan, OutputFormat::Json).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["edge_type"], "rising");
        assert_eq!(parsed["edge_count"], 3);
    }

    #[test]
    fn test_execute_sample_produces_expected_value() {
        let mut session = make_session();
        let req = PlanRequest::new(
            session.file_name.clone(),
            CommandSpec::Sample {
                signal: "top.clk".into(),
                at: "35ns".into(),
            },
            OutputFormat::Json,
        );
        let plan = session.plan(&req).unwrap();
        let output = session.execute(&plan, OutputFormat::Json).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["at"], 35);
        assert_eq!(parsed["value"], "1");
    }

    #[test]
    fn test_execute_ascii_produces_plain_text() {
        let mut session = make_session();
        let req = PlanRequest::new(
            session.file_name.clone(),
            CommandSpec::Ascii {
                signals: vec!["top.clk".into()],
                from: "0ns".into(),
                to: Some("100ns".into()),
            },
            OutputFormat::Text,
        );
        let plan = session.plan(&req).unwrap();
        let output = session.execute(&plan, OutputFormat::Text).unwrap();
        assert!(output.contains("Time"));
        assert!(output.contains("top.clk"));
        assert!(
            serde_json::from_str::<serde_json::Value>(&output).is_err(),
            "ASCII output should be plain text"
        );
    }

    #[test]
    fn test_execute_changes_table_format() {
        let mut session = make_session();
        let req = PlanRequest::new(
            session.file_name.clone(),
            CommandSpec::Changes {
                signals: vec!["top.clk".into()],
                from: "0ns".into(),
                to: Some("100ns".into()),
            },
            OutputFormat::Table,
        );
        let plan = session.plan(&req).unwrap();
        let output = session.execute(&plan, OutputFormat::Table).unwrap();
        assert!(output.contains("time|signal|value"));
    }

    #[test]
    fn test_execute_changes_text_format() {
        let mut session = make_session();
        let req = PlanRequest::new(
            session.file_name.clone(),
            CommandSpec::Changes {
                signals: vec!["top.clk".into()],
                from: "0ns".into(),
                to: Some("100ns".into()),
            },
            OutputFormat::Text,
        );
        let plan = session.plan(&req).unwrap();
        let output = session.execute(&plan, OutputFormat::Text).unwrap();
        assert!(output.contains("Time"));
        assert!(output.contains("Signal"));
        assert!(output.contains("Value"));
        assert!(output.contains("top.clk"));
    }

    #[test]
    fn test_session_file_name_stored() {
        let session = make_session();
        assert!(session.file_name.contains("waveql_planner_test"));
    }

    // ── Events derivation ─────────────────────────────────────

    #[test]
    fn test_plan_events_edges_normalizes() {
        let mut session = make_session();
        let req = PlanRequest::new(
            session.file_name.clone(),
            CommandSpec::Events {
                derivation: EventsDerivationSpec::Edges {
                    signals: vec!["top.clk".into()],
                    polarity: EdgePolarity::Rise,
                },
                from: "0ns".into(),
                to: Some("100ns".into()),
            },
            OutputFormat::Json,
        );
        let plan = session.plan(&req).unwrap();
        match plan {
            Plan::Events { derivation, range } => {
                assert_eq!(range.from, Some(0));
                assert_eq!(range.to, Some(100));
                assert!(matches!(derivation, DerivationKind::Edges { .. }));
            }
            _ => panic!("expected Plan::Events"),
        }
    }

    #[test]
    fn test_execute_events_edges_json() {
        let mut session = make_session();
        let req = PlanRequest::new(
            session.file_name.clone(),
            CommandSpec::Events {
                derivation: EventsDerivationSpec::Edges {
                    signals: vec!["top.clk".into()],
                    polarity: EdgePolarity::Rise,
                },
                from: "0ns".into(),
                to: Some("100ns".into()),
            },
            OutputFormat::Json,
        );
        let plan = session.plan(&req).unwrap();
        let output = session.execute(&plan, OutputFormat::Json).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["query_type"], "events");
        assert_eq!(parsed["derivation"], "edges");
        assert_eq!(parsed["event_count"], 3);
        for ev in parsed["events"].as_array().unwrap() {
            assert_eq!(ev["kind"], "edge");
            assert_eq!(ev["signal"], "top.clk");
        }
    }

    #[test]
    fn test_execute_events_handshake_json() {
        let mut session = make_session();
        let req = PlanRequest::new(
            session.file_name.clone(),
            CommandSpec::Events {
                derivation: EventsDerivationSpec::Handshake {
                    signal_a: "top.clk".into(),
                    signal_b: "top.en".into(),
                },
                from: "0ns".into(),
                to: Some("100ns".into()),
            },
            OutputFormat::Json,
        );
        let plan = session.plan(&req).unwrap();
        let output = session.execute(&plan, OutputFormat::Json).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["query_type"], "events");
        assert_eq!(parsed["derivation"], "handshake");
        // clk rises at 10 (initiated), en rises at 20 (acknowledged)
        assert!(parsed["event_count"].as_u64().unwrap() >= 2);
    }

    #[test]
    fn test_execute_events_invalid_signal_fails() {
        let mut session = make_session();
        let req = PlanRequest::new(
            session.file_name.clone(),
            CommandSpec::Events {
                derivation: EventsDerivationSpec::Edges {
                    signals: vec!["top.nonexistent".into()],
                    polarity: EdgePolarity::Rise,
                },
                from: "0ns".into(),
                to: None,
            },
            OutputFormat::Json,
        );
        assert!(session.plan(&req).is_err());
    }
}

use serde::Serialize;

/// A parsed query from CLI arguments.
#[derive(Debug, Clone)]
pub enum Query {
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
    Bind(BindRequest),
    Analyze(AnalyzeRequest),
}

/// Request to bind logical protocol roles to concrete signal paths.
#[derive(Debug, Clone)]
pub struct BindRequest {
    /// Protocol name (e.g., `"valid_ready"`, `"spi"`).
    pub protocol_name: String,

    /// Explicit (role, signal_path) pairs from the user.
    pub bindings: Vec<(String, String)>,
}

/// Request to analyze a protocol against a waveform.
#[derive(Debug, Clone)]
pub struct AnalyzeRequest {
    /// Protocol name (e.g., `"valid_ready"`).
    pub protocol_name: String,

    /// Explicit (role, signal_path) bindings.
    pub bindings: Vec<(String, String)>,

    /// Time range for analysis.
    pub range: TimeRange,
}

/// Which protocol-agnostic derivation to perform.
#[derive(Debug, Clone)]
pub enum DerivationKind {
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

/// Edge polarity filter for derived events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EdgePolarity {
    Rise,
    Fall,
    Toggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeType {
    Rising,
    Falling,
    Both,
}

#[derive(Debug, Clone, Copy)]
pub struct TimeRange {
    pub from: Option<u64>,
    pub to: Option<u64>,
}

/// Output format requested by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Json,
    Text,
    Table,
}

// ── Output types ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ListOutput {
    pub file: String,
    pub format: String,
    pub timescale: String,
    pub total_signals: usize,
    pub signals: Vec<crate::SignalInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChangesOutput {
    pub query_type: String,
    pub signal_count: usize,
    pub range: RangeInfo,
    pub events: Vec<ChangeEvent>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct RangeInfo {
    pub from: Option<u64>,
    pub to: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChangeEvent {
    pub time: u64,
    pub signal: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EdgesOutput {
    pub signal: String,
    pub edge_type: String,
    pub edge_count: usize,
    pub edges: Vec<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SampleOutput {
    pub signal: String,
    pub at: u64,
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventsOutput {
    pub query_type: String,
    pub derivation: String,
    pub event_count: usize,
    pub range: RangeInfo,
    pub events: Vec<DerivedEventOutput>,
}

#[derive(Debug, Serialize)]
pub struct ProtocolsOutput {
    pub protocols: Vec<ProtocolSchemaInfo>,
}

#[derive(Debug, Serialize)]
pub struct ProtocolSchemaInfo {
    pub name: String,
    pub description: String,
    pub required_role_count: usize,
    pub optional_role_count: usize,
    pub required_roles: Vec<String>,
    pub optional_roles: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct BindOutput {
    pub protocol: String,
    pub is_valid: bool,
    pub missing_roles: Vec<String>,
    pub bindings: Vec<BoundRoleInfo>,
    pub suggestions: Vec<BindingSuggestionOutput>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct BoundRoleInfo {
    pub role: String,
    pub signal: String,
    pub binding_kind: String,
}

#[derive(Debug, Serialize)]
pub struct BindingSuggestionOutput {
    pub role: String,
    pub candidates: Vec<BindingCandidateOutput>,
}

#[derive(Debug, Serialize)]
pub struct BindingCandidateOutput {
    pub signal_path: String,
    pub confidence: f64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DerivedEventOutput {
    Edge {
        time: u64,
        signal: String,
        polarity: String,
        prev_value: String,
        next_value: String,
    },
    Handshake {
        time: u64,
        phase: String,
        signal_a: String,
        signal_b: String,
    },
    Stall {
        signal: String,
        value: String,
        since_time: u64,
        duration: u64,
    },
    Timeout {
        description: String,
        deadline: u64,
        last_event_time: u64,
        signal: String,
    },
    StateTransition {
        time: u64,
        signal: String,
        from: String,
        to: String,
    },
    SampleOnEdge {
        time: u64,
        clock: String,
        target: String,
        value: String,
        edge: String,
    },
}

// ── Analysis output types ────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct AnalyzeOutput {
    pub protocol: String,
    pub summary: AnalyzeSummary,
    pub handshakes: Vec<HandshakeInfo>,
    pub violations: Vec<ViolationInfo>,
    pub stalls: Vec<StallInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub transfers: Vec<SpiTransferInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cs_windows: Vec<SpiCsWindowInfo>,
}

#[derive(Debug, Serialize)]
pub struct AnalyzeSummary {
    pub total_handshakes: usize,
    pub total_violations: usize,
    pub total_stalls: usize,
    pub pass: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct HandshakeInfo {
    pub time: u64,
    pub phase: String,
    pub signal_a: String,
    pub signal_b: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StallInfo {
    pub signal: String,
    pub value: String,
    pub since_time: u64,
    pub duration: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ViolationInfo {
    pub kind: String,
    pub severity: String,
    pub description: String,
    pub related_roles: Vec<String>,
    pub related_signals: Vec<String>,
    pub evidence_start: u64,
    pub evidence_end: Option<u64>,
}

// ── SPI-specific output types ──────────────────────────────────────

/// Reconstructed SPI transfer within a single CS window.
#[derive(Debug, Clone, Serialize)]
pub struct SpiTransferInfo {
    pub cs_start: u64,
    pub cs_end: u64,
    pub sclk_edges: usize,
    pub mosi_bits: Vec<String>,
    pub miso_bits: Vec<String>,
    pub mosi_words: Vec<String>,
    pub miso_words: Vec<String>,
}

/// Chip-select window boundaries.
#[derive(Debug, Clone, Serialize)]
pub struct SpiCsWindowInfo {
    pub start: u64,
    pub end: u64,
}

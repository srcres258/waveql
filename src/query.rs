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

#[derive(Debug, Serialize)]
pub struct ListOutput {
    pub file: String,
    pub format: String,
    pub timescale: String,
    pub total_signals: usize,
    pub signals: Vec<crate::SignalInfo>,
}

#[derive(Debug, Serialize)]
pub struct ChangesOutput {
    pub query_type: String,
    pub signal_count: usize,
    pub range: RangeInfo,
    pub events: Vec<ChangeEvent>,
}

#[derive(Debug, Serialize)]
pub struct RangeInfo {
    pub from: Option<u64>,
    pub to: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct ChangeEvent {
    pub time: u64,
    pub signal: String,
    pub value: String,
}

#[derive(Debug, Serialize)]
pub struct EdgesOutput {
    pub signal: String,
    pub edge_type: String,
    pub edge_count: usize,
    pub edges: Vec<u64>,
}

#[derive(Debug, Serialize)]
pub struct SampleOutput {
    pub signal: String,
    pub at: u64,
    pub value: Option<String>,
}

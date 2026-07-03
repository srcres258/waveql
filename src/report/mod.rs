// ── Report Layer ───────────────────────────────────────────────────
//
// Typed output from the evaluator, consumed directly by all renderers.
//
// The [`Report`] enum is the single return type of
// [`crate::evaluator::evaluate`].  Each variant carries the fully-resolved
// structure for one query type.  JSON, text, and table renderers all
// consume the same [`Report`] — no serialization round-trips between the
// engine and output layers.
//
// Protocol-analysis reports (`Analyze`, `Bind`, `Protocols`) are handled
// by [`crate::planner::Session`] and are not included here.
//
// [`EvidenceWindow`] and [`EvidenceBundle`] carry traceable evidence
// windows for protocol violations and semantic events.

use crate::query::{
    ChangeEvent, ChangesOutput, DerivedEventOutput, EdgesOutput, EventsOutput, ListOutput,
    RangeInfo, SampleOutput,
};
use serde::Serialize;

// ── Report ─────────────────────────────────────────────────────────

/// Unified typed report produced by the evaluator.
///
/// Each variant holds the fully-computed output structure.  Renderers
/// consume this directly — no serde round-trips between the engine and
/// the output layer.
///
/// Protocol analysis reports (`Analyze`, `Bind`, `Protocols`) are
/// handled directly by the session layer and are not included here.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Report {
    /// Signal listing — always rendered as JSON or table.
    List(ListOutput),

    /// Value changes for one or more signals within a time window.
    Changes(ChangesOutput),

    /// Rising/falling/toggle edge times for a single signal.
    Edges(EdgesOutput),

    /// Sampled value at a single point in time.
    Sample(SampleOutput),

    /// Plain-text ASCII waveform (never serialized as JSON).
    Ascii(String),

    /// Protocol-agnostic derived temporal events.
    Events(EventsOutput),
}

// ── Evidence Window (report-level) ─────────────────────────────────

/// A time window that provides supporting evidence for a semantic event
/// or violation.  Carries the window boundaries, the signals involved,
/// and a human-readable label explaining what the window represents.
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceWindow {
    /// Start time (inclusive) in waveform-native time units.
    pub start: u64,

    /// End time (exclusive) in waveform-native time units.  `None`
    /// means the window extends to the end of the available trace.
    pub end: Option<u64>,

    /// Signal paths that participate in this evidence window.
    pub signals: Vec<String>,

    /// Human-readable label describing what this window captures
    /// (e.g. "payload stability check between [10, 30]").
    pub label: String,
}

impl EvidenceWindow {
    /// Create a new evidence window.
    pub fn new(start: u64, end: Option<u64>, signals: Vec<String>, label: String) -> Self {
        EvidenceWindow {
            start,
            end,
            signals,
            label,
        }
    }
}

// ── Evidence Bundle ────────────────────────────────────────────────

/// An evidence bundle captures a semantic event, protocol violation,
/// or derived insight together with its supporting evidence window.
///
/// Protocol analyzers produce these during analysis and they flow
/// directly into the typed report structures consumed by renderers.
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceBundle {
    /// Human-readable category (e.g. "handshake", "violation", "stall").
    pub kind: String,

    /// Severity level: "error", "warning", or "info".
    pub severity: String,

    /// Human-readable description of what was observed.
    pub description: String,

    /// Time window that supports this finding.
    pub evidence: EvidenceWindow,

    /// Related protocol roles (e.g. `["valid", "ready"]`).
    pub related_roles: Vec<String>,
}

// ── Report convenience constructors ────────────────────────────────

/// Build a [`Report::Changes`] from its components.
pub fn changes_report(signal_count: usize, range: RangeInfo, events: Vec<ChangeEvent>) -> Report {
    Report::Changes(ChangesOutput {
        query_type: "changes".into(),
        signal_count,
        range,
        events,
    })
}

/// Build a [`Report::Edges`] from its components.
pub fn edges_report(signal: String, edge_type: String, edges: Vec<u64>) -> Report {
    let count = edges.len();
    Report::Edges(EdgesOutput {
        signal,
        edge_type,
        edge_count: count,
        edges,
    })
}

/// Build a [`Report::Events`] from its components.
pub fn events_report(
    derivation: String,
    range: RangeInfo,
    events: Vec<DerivedEventOutput>,
) -> Report {
    let count = events.len();
    Report::Events(EventsOutput {
        query_type: "events".into(),
        derivation,
        event_count: count,
        range,
        events,
    })
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evidence_window_construction() {
        let window = EvidenceWindow::new(10, Some(30), vec!["top.clk".into()], "test".into());
        assert_eq!(window.start, 10);
        assert_eq!(window.end, Some(30));
        assert_eq!(window.signals, vec!["top.clk"]);
        assert_eq!(window.label, "test");
    }

    #[test]
    fn test_evidence_window_open_ended() {
        let window = EvidenceWindow::new(100, None, vec![], "open".into());
        assert_eq!(window.start, 100);
        assert_eq!(window.end, None);
        assert_eq!(window.label, "open");
    }

    #[test]
    fn test_evidence_bundle_construction() {
        let bundle = EvidenceBundle {
            kind: "violation".into(),
            severity: "error".into(),
            description: "payload unstable".into(),
            evidence: EvidenceWindow::new(10, Some(30), vec!["top.data".into()], "check".into()),
            related_roles: vec!["data".into()],
        };
        assert_eq!(bundle.kind, "violation");
        assert_eq!(bundle.severity, "error");
    }

    #[test]
    fn test_report_changes_constructor() {
        let report = changes_report(
            1,
            RangeInfo {
                from: Some(0),
                to: Some(100),
            },
            vec![ChangeEvent {
                time: 10,
                signal: "top.clk".into(),
                value: "1".into(),
            }],
        );
        match report {
            Report::Changes(c) => {
                assert_eq!(c.query_type, "changes");
                assert_eq!(c.signal_count, 1);
                assert_eq!(c.events.len(), 1);
            }
            _ => panic!("expected Changes"),
        }
    }

    #[test]
    fn test_report_edges_constructor() {
        let report = edges_report("top.clk".into(), "rising".into(), vec![10, 50]);
        match report {
            Report::Edges(e) => {
                assert_eq!(e.signal, "top.clk");
                assert_eq!(e.edge_type, "rising");
                assert_eq!(e.edge_count, 2);
            }
            _ => panic!("expected Edges"),
        }
    }

    #[test]
    fn test_report_serialization_round_trip() {
        let report = Report::Sample(SampleOutput {
            signal: "top.clk".into(),
            at: 42,
            value: Some("1".into()),
        });
        let json = serde_json::to_string(&report).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["signal"], "top.clk");
        assert_eq!(parsed["at"], 42);
    }

    #[test]
    fn test_report_ascii_serializes_as_string() {
        let report = Report::Ascii("Time | clk\n0    | 0".into());
        let json = serde_json::to_string(&report).unwrap();
        // With untagged serialization, Ascii produces a plain JSON string
        assert_eq!(json, "\"Time | clk\\n0    | 0\"");
    }
}

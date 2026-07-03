// ── Derived Temporal Events ────────────────────────────────────────
//
// Protocol-agnostic temporal primitives built on top of trace slices.
//
// This module provides pure, deterministic derivation functions that
// produce typed events from raw signal data.  Protocol analyzers
// (valid/ready, SPI, etc.) compose these primitives without baking
// protocol-specific semantics into the core.
//
// Every event carries an evidence window so that downstream reporters
// and verifiers can point back at the raw data that produced a
// conclusion — the engine never invents semantics.
//
// # Design principles
//
// 1. **Protocol-neutral** — no signal names, bus families, or
//    protocol-specific assumption lives here.
// 2. **Deterministic** — same input → same events.  No llm inference,
//    no randomness.
// 3. **Streaming-friendly** — derivation functions return `Vec` for
//    simplicity, but each event is independently meaningful so callers
//    can process incrementally.
// 4. **X/Z/U awareness** — ambiguous or unknown values are reported
//    explicitly via the `ValueClass` discriminator so downstream
//    analyzers never silently coerce `x` into `0`.
// 5. **Evidence-preserving** — every event includes the time window
//    and signal set that was inspected when the conclusion was reached.

use crate::backend::types::CompactValue;
use crate::backend::types::SignalData;
use crate::trace::{TimeBound, TraceSlice};
use serde::Serialize;

// ── Value classification ───────────────────────────────────────────

/// How the engine interprets a value for edge/state logic.
///
/// Protocol analyzers should match on `Unknown` instead of silently
/// treating `x`/`z` as low or high.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ValueClass {
    Low,
    High,
    Unknown,
}

impl ValueClass {
    /// Classify a [`CompactValue`].
    ///
    /// Single-bit values: `0` → Low, `1` → High, anything else → Unknown.
    /// Multi-bit values:  known numeric → High, all `0` → Low, otherwise
    /// (including `x`/`z`/`u` at any position) → Unknown.
    pub fn classify(v: &CompactValue) -> Self {
        match v {
            CompactValue::Bit(b'0') => ValueClass::Low,
            CompactValue::Bit(b'1') => ValueClass::High,
            CompactValue::Bit(_) => ValueClass::Unknown,
            CompactValue::Str(s) => {
                let mut all_zero = true;
                let mut has_unknown = false;
                for c in s.chars() {
                    match c {
                        'x' | 'X' | 'z' | 'Z' | 'u' | 'U' => has_unknown = true,
                        '0' => {}
                        _ => all_zero = false,
                    }
                }
                if has_unknown {
                    ValueClass::Unknown
                } else if all_zero {
                    ValueClass::Low
                } else {
                    ValueClass::High
                }
            }
        }
    }

    pub fn is_known(&self) -> bool {
        !matches!(self, ValueClass::Unknown)
    }

    pub fn is_low(&self) -> bool {
        matches!(self, ValueClass::Low)
    }

    pub fn is_high(&self) -> bool {
        matches!(self, ValueClass::High)
    }
}

// ── Evidence Window ────────────────────────────────────────────────

/// A bounded time window that supports a derived event.
///
/// The window records *when* the engine looked and *which signals* it
/// inspected.  Every [`DerivedEvent`] carries at least one window so
/// that violations can cite their supporting raw data.
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceWindow {
    /// Start of the observation window (inclusive, native time units).
    pub start: u64,

    /// End of the observation window (inclusive, native time units).
    /// `None` when the window is still open (e.g., a timeout that
    /// hasn't been satisfied yet).
    pub end: Option<u64>,

    /// Signal paths that were inspected.
    pub signals: Vec<String>,

    /// Human-readable note about what was examined.
    pub context: String,
}

impl EvidenceWindow {
    pub fn new(start: u64, end: Option<u64>, signals: Vec<String>, context: String) -> Self {
        EvidenceWindow {
            start,
            end,
            signals,
            context,
        }
    }
}

// ── Event kinds ────────────────────────────────────────────────────

/// Edge direction for edge-based event derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EdgePolarity {
    /// 0→1 or low→high transition.
    Rise,
    /// 1→0 or high→low transition.
    Fall,
    /// Any logic-level change (including into/out-of unknown).
    Toggle,
}

/// Phase in a two-signal handshake interaction.
///
/// The phases are protocol-agnostic — they describe the abstract
/// signalling sequence without naming req/ack/valid/ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HandshakePhase {
    /// Signal-A asserted (low→high transition), awaiting Signal-B.
    Initiated,
    /// Signal-B asserted — handshake rendezvous.
    Acknowledged,
    /// Signal-A deasserted, awaiting Signal-B deassert.
    Released,
    /// Signal-B deasserted — cycle complete.
    Completed,
}

/// A single unified derived event.
///
/// The top-level `DerivedEvent` enum is what callers collect.  Protocol
/// analyzers match on variants, reporters serialize the whole list.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DerivedEvent {
    /// A rising, falling, or toggle edge on a signal.
    Edge {
        time: u64,
        signal: String,
        polarity: EdgePolarity,
        prev_class: ValueClass,
        next_class: ValueClass,
        prev_value: String,
        next_value: String,
        window: EvidenceWindow,
    },

    /// Sample a target signal at the instant of an edge on a clock.
    SampleOnEdge {
        time: u64,
        clock: String,
        edge_polarity: EdgePolarity,
        target: String,
        target_value: String,
        target_class: ValueClass,
        window: EvidenceWindow,
    },

    /// A detected handshake phase change between two signals.
    Handshake {
        time: u64,
        phase: HandshakePhase,
        signal_a: String,
        signal_b: String,
        window: EvidenceWindow,
    },

    /// A signal held at a value longer than `min_duration`.
    Stall {
        signal: String,
        value: String,
        class: ValueClass,
        since_time: u64,
        duration: u64,
        window: EvidenceWindow,
    },

    /// An expected event did not occur by its deadline.
    Timeout {
        description: String,
        deadline: u64,
        last_event_time: u64,
        signal: String,
        window: EvidenceWindow,
    },

    /// A signal transitioned from one logical state to another.
    StateTransition {
        time: u64,
        signal: String,
        from_value: String,
        to_value: String,
        window: EvidenceWindow,
    },
}

// ── Derivation functions ───────────────────────────────────────────

/// Detect edges on a single signal within `bounds`.
///
/// Returns all rising, falling, and/or toggle edges depending on
/// `polarity`.  Values classified as `Unknown` are **not** silently
/// treated as high or low — toggles involving unknown values are
/// emitted with `Edge { polarity: Toggle, ... }` when `Toggle` is
/// requested, and omitted when only `Rise`/`Fall` is requested.
pub fn detect_edges(
    signal_path: &str,
    data: &SignalData,
    polarity: EdgePolarity,
    bounds: TimeBound,
) -> Vec<DerivedEvent> {
    let filtered: Vec<(u64, &CompactValue)> = data
        .changes
        .iter()
        .skip_while(|(t, _)| *t < bounds.from)
        .take_while(|(t, _)| *t <= bounds.to)
        .map(|(t, v)| (*t, v))
        .collect();

    if filtered.is_empty() {
        return vec![];
    }

    let mut events = Vec::new();

    // Seed the first-in-window change with the pre-window sample so
    // that an edge occurring exactly at bounds.from is not missed.
    let pre_val = data.sample(bounds.from.saturating_sub(1));

    // Compare first filtered element against pre-window value (if any).
    let first = &filtered[0];
    if let Some(pv) = pre_val {
        let (time, val) = (&first.0, first.1);
        let prev_class = ValueClass::classify(pv);
        let next_class = ValueClass::classify(val);
        let emit = match polarity {
            EdgePolarity::Rise => prev_class.is_low() && next_class.is_high(),
            EdgePolarity::Fall => prev_class.is_high() && next_class.is_low(),
            EdgePolarity::Toggle => pv.as_str() != val.as_str(),
        };
        if emit {
            events.push(make_edge_event(
                signal_path,
                *time,
                (pv, prev_class),
                (val, next_class),
                bounds.from.saturating_sub(1),
                *time,
            ));
        }
    }

    // Compare subsequent adjacent pairs within the filtered window.
    for i in 1..filtered.len() {
        let (prev_time, prev_val) = (&filtered[i - 1].0, filtered[i - 1].1);
        let (time, val) = (&filtered[i].0, filtered[i].1);

        let prev_class = ValueClass::classify(prev_val);
        let next_class = ValueClass::classify(val);
        let emit = match polarity {
            EdgePolarity::Rise => prev_class.is_low() && next_class.is_high(),
            EdgePolarity::Fall => prev_class.is_high() && next_class.is_low(),
            EdgePolarity::Toggle => prev_val.as_str() != val.as_str(),
        };
        if emit {
            events.push(make_edge_event(
                signal_path,
                *time,
                (prev_val, prev_class),
                (val, next_class),
                *prev_time,
                *time,
            ));
        }
    }
    events
}

/// Build a [`DerivedEvent::Edge`] with the correct polarity tag and
/// evidence window.
fn make_edge_event(
    signal: &str,
    time: u64,
    prev: (&CompactValue, ValueClass),
    next: (&CompactValue, ValueClass),
    window_start: u64,
    window_end: u64,
) -> DerivedEvent {
    let (prev_val, prev_class) = prev;
    let (next_val, next_class) = next;
    let polarity = match (prev_class, next_class) {
        (ValueClass::Low, ValueClass::High) => EdgePolarity::Rise,
        (ValueClass::High, ValueClass::Low) => EdgePolarity::Fall,
        _ => EdgePolarity::Toggle,
    };
    DerivedEvent::Edge {
        time,
        signal: signal.to_string(),
        polarity,
        prev_class,
        next_class,
        prev_value: prev_val.as_str().to_string(),
        next_value: next_val.as_str().to_string(),
        window: EvidenceWindow::new(
            window_start,
            Some(window_end),
            vec![signal.to_string()],
            format!(
                "edge detection on {}: {}→{} at {}",
                signal,
                prev_val.as_str(),
                next_val.as_str(),
                time
            ),
        ),
    }
}

/// Detect edges on multiple signals in a slice.
///
/// Convenience wrapper — delegates to [`detect_edges`] per signal.
pub fn detect_edges_multi(slice: &TraceSlice, polarity: EdgePolarity) -> Vec<DerivedEvent> {
    let mut events = Vec::new();
    for (i, sig) in slice.signals.iter().enumerate() {
        if let Some(data) = slice.data.get(i) {
            let mut sig_events = detect_edges(sig, data, polarity, slice.bounds);
            events.append(&mut sig_events);
        }
    }
    // Maintain time order across signals
    sort_by_time(&mut events);
    events
}

/// Sample one or more target signals at every edge of a clock signal
/// within `bounds`.
///
/// Returns a [`DerivedEvent::SampleOnEdge`] for each clock edge,
/// containing the target's value at that instant.
pub fn sample_on_edges(
    clock_path: &str,
    clock_data: &SignalData,
    targets: &[(String, &SignalData)],
    edge_polarity: EdgePolarity,
    bounds: TimeBound,
) -> Vec<DerivedEvent> {
    let edges = detect_edges(clock_path, clock_data, edge_polarity, bounds);
    if edges.is_empty() {
        return vec![];
    }

    let mut events = Vec::new();
    for edge in &edges {
        if let DerivedEvent::Edge {
            time,
            signal: ref clock,
            polarity,
            ..
        } = edge
        {
            for (target_path, target_data) in targets {
                let val = target_data.sample(*time);
                let (val_str, val_class) = match val {
                    Some(v) => (v.as_str().to_string(), ValueClass::classify(v)),
                    None => ("?".to_string(), ValueClass::Unknown),
                };
                events.push(DerivedEvent::SampleOnEdge {
                    time: *time,
                    clock: clock.clone(),
                    edge_polarity: *polarity,
                    target: target_path.clone(),
                    target_value: val_str,
                    target_class: val_class,
                    window: EvidenceWindow::new(
                        *time,
                        None,
                        vec![clock_path.to_string(), target_path.clone()],
                        format!(
                            "sampled {} on {} edge of {}",
                            target_path,
                            match polarity {
                                EdgePolarity::Rise => "rising",
                                EdgePolarity::Fall => "falling",
                                EdgePolarity::Toggle => "toggle",
                            },
                            clock_path
                        ),
                    ),
                });
            }
        }
    }
    sort_by_time(&mut events);
    events
}

/// Detect handshake phases between two signals over `bounds`.
///
/// The handshake model is an abstract two-phase sequence:
/// 1. Signal-A asserts (any logic change → High) — *Initiated*
/// 2. Signal-B asserts while A is still high — *Acknowledged*
/// 3. Signal-A deasserts — *Released*
/// 4. Signal-B deasserts — *Completed* (cycle restarts from step 1)
///
/// Each phase transition produces a [`DerivedEvent::Handshake`].
/// The model does NOT assume signal A/B are named "valid"/"ready" —
/// the caller provides whatever two signals it wants to check.
pub fn detect_handshakes(
    signal_a: &str,
    data_a: &SignalData,
    signal_b: &str,
    data_b: &SignalData,
    bounds: TimeBound,
) -> Vec<DerivedEvent> {
    let changes_a: Vec<(u64, &CompactValue)> = data_a
        .changes
        .iter()
        .skip_while(|(t, _)| *t < bounds.from)
        .take_while(|(t, _)| *t <= bounds.to)
        .map(|(t, v)| (*t, v))
        .collect();

    let changes_b: Vec<(u64, &CompactValue)> = data_b
        .changes
        .iter()
        .skip_while(|(t, _)| *t < bounds.from)
        .take_while(|(t, _)| *t <= bounds.to)
        .map(|(t, v)| (*t, v))
        .collect();

    // Merge both change streams by time for phase tracking.
    #[derive(Clone, Copy)]
    struct Change<'a> {
        time: u64,
        signal: &'a str,
        value: &'a CompactValue,
    }

    let mut merged: Vec<Change> = Vec::new();
    for (t, v) in &changes_a {
        merged.push(Change {
            time: *t,
            signal: signal_a,
            value: v,
        });
    }
    for (t, v) in &changes_b {
        merged.push(Change {
            time: *t,
            signal: signal_b,
            value: v,
        });
    }
    merged.sort_by_key(|c| c.time);

    let mut events = Vec::new();

    // Phase state tracking. We need the last known value of each
    // signal *before* the window to seed the state machine, so look
    // at the sample just before the window start.
    let prev_a = data_a.sample(bounds.from.saturating_sub(1));
    let prev_b = data_b.sample(bounds.from.saturating_sub(1));
    let mut a_high = prev_a
        .map(|v| ValueClass::classify(v).is_high())
        .unwrap_or(false);
    let mut b_high = prev_b
        .map(|v| ValueClass::classify(v).is_high())
        .unwrap_or(false);

    // State machine phases
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Phase {
        Idle,         // Both low (or starting state)
        Initiated,    // A high, B low
        Acknowledged, // A high, B high
        Released,     // A low, B high
    }

    let phase_from = |a: bool, b: bool| -> Phase {
        match (a, b) {
            (false, false) => Phase::Idle,
            (true, false) => Phase::Initiated,
            (true, true) => Phase::Acknowledged,
            (false, true) => Phase::Released,
        }
    };

    let mut current_phase = phase_from(a_high, b_high);

    for ch in &merged {
        let val_class = ValueClass::classify(ch.value);
        let is_high = val_class.is_high();
        let is_low = val_class.is_low();

        if ch.signal == signal_a {
            if is_high {
                a_high = true;
            } else if is_low {
                a_high = false;
            }
            // Unknown value → don't change A's state, emit an unknown-aware
            // observation but continue tracking with last known value.
            if !is_high && !is_low {
                // Ambiguous transition — record as state transition
                // with unknown but don't alter phase tracking.
                events.push(DerivedEvent::StateTransition {
                    time: ch.time,
                    signal: ch.signal.to_string(),
                    from_value: format!("{}", a_high as u8),
                    to_value: ch.value.as_str().to_string(),
                    window: EvidenceWindow::new(
                        ch.time,
                        None,
                        vec![signal_a.to_string()],
                        format!(
                            "{} changed to ambiguous value {}",
                            signal_a,
                            ch.value.as_str()
                        ),
                    ),
                });
                continue;
            }
        } else if ch.signal == signal_b {
            if is_high {
                b_high = true;
            } else if is_low {
                b_high = false;
            }
            if !is_high && !is_low {
                events.push(DerivedEvent::StateTransition {
                    time: ch.time,
                    signal: ch.signal.to_string(),
                    from_value: format!("{}", b_high as u8),
                    to_value: ch.value.as_str().to_string(),
                    window: EvidenceWindow::new(
                        ch.time,
                        None,
                        vec![signal_b.to_string()],
                        format!(
                            "{} changed to ambiguous value {}",
                            signal_b,
                            ch.value.as_str()
                        ),
                    ),
                });
                continue;
            }
        }

        let new_phase = phase_from(a_high, b_high);
        if new_phase != current_phase {
            let handshake_phase = match new_phase {
                Phase::Idle => HandshakePhase::Completed,
                Phase::Initiated => HandshakePhase::Initiated,
                Phase::Acknowledged => HandshakePhase::Acknowledged,
                Phase::Released => HandshakePhase::Released,
            };
            events.push(DerivedEvent::Handshake {
                time: ch.time,
                phase: handshake_phase,
                signal_a: signal_a.to_string(),
                signal_b: signal_b.to_string(),
                window: EvidenceWindow::new(
                    ch.time,
                    None,
                    vec![signal_a.to_string(), signal_b.to_string()],
                    format!(
                        "handshake phase {:?} at {} (A={}, B={})",
                        handshake_phase,
                        ch.time,
                        if a_high { "high" } else { "low" },
                        if b_high { "high" } else { "low" },
                    ),
                ),
            });
            current_phase = new_phase;
        }
    }

    sort_by_time(&mut events);
    events
}

/// Detect stall periods where a signal holds steady for at least
/// `min_duration` within `bounds`.
///
/// A stall is reported once the signal has been constant for longer
/// than `min_duration`.  If the signal never changes within the whole
/// window, one stall event is emitted with `duration` = window length.
pub fn detect_stalls(
    signal_path: &str,
    data: &SignalData,
    min_duration: u64,
    bounds: TimeBound,
) -> Vec<DerivedEvent> {
    let filtered: Vec<(u64, &CompactValue)> = data
        .changes
        .iter()
        .skip_while(|(t, _)| *t < bounds.from)
        .take_while(|(t, _)| *t <= bounds.to)
        .map(|(t, v)| (*t, v))
        .collect();

    let mut events = Vec::new();

    if filtered.is_empty() {
        // No changes in window — check if the pre-window value held
        // for the whole window.
        let window_duration = bounds.to.saturating_sub(bounds.from);
        if window_duration >= min_duration {
            if let Some(prev) = data.sample(bounds.from.saturating_sub(1)) {
                events.push(DerivedEvent::Stall {
                    signal: signal_path.to_string(),
                    value: prev.as_str().to_string(),
                    class: ValueClass::classify(prev),
                    since_time: bounds.from,
                    duration: window_duration,
                    window: EvidenceWindow::new(
                        bounds.from,
                        Some(bounds.to),
                        vec![signal_path.to_string()],
                        format!(
                            "{} held at {} for {} time units (entire window)",
                            signal_path,
                            prev.as_str(),
                            window_duration
                        ),
                    ),
                });
            }
        }
        return events;
    }

    // The first change in the window establishes the start of the
    // first sticky interval.
    let mut hold_start = bounds.from;
    let mut hold_value: Option<&CompactValue> = data.sample(bounds.from.saturating_sub(1));

    for (t, v) in &filtered {
        if let Some(hv) = hold_value {
            let duration = t.saturating_sub(hold_start);
            if duration >= min_duration {
                events.push(DerivedEvent::Stall {
                    signal: signal_path.to_string(),
                    value: hv.as_str().to_string(),
                    class: ValueClass::classify(hv),
                    since_time: hold_start,
                    duration,
                    window: EvidenceWindow::new(
                        hold_start,
                        Some(*t),
                        vec![signal_path.to_string()],
                        format!(
                            "{} held at {} for {} time units",
                            signal_path,
                            hv.as_str(),
                            duration
                        ),
                    ),
                });
            }
        }
        hold_start = *t;
        hold_value = Some(v);
    }

    // Check the final interval: from last change to end of window.
    if let Some(hv) = hold_value {
        let duration = bounds.to.saturating_sub(hold_start);
        if duration >= min_duration {
            events.push(DerivedEvent::Stall {
                signal: signal_path.to_string(),
                value: hv.as_str().to_string(),
                class: ValueClass::classify(hv),
                since_time: hold_start,
                duration,
                window: EvidenceWindow::new(
                    hold_start,
                    Some(bounds.to),
                    vec![signal_path.to_string()],
                    format!(
                        "{} held at {} for {} time units (end of window)",
                        signal_path,
                        hv.as_str(),
                        duration
                    ),
                ),
            });
        }
    }

    sort_by_time(&mut events);
    events
}

/// Check whether an expected edge occurred within [`start`, `deadline`].
///
/// Returns a [`DerivedEvent::Timeout`] if no qualifying edge was found.
pub fn check_timeout(
    signal_path: &str,
    data: &SignalData,
    expected_polarity: EdgePolarity,
    start: u64,
    deadline: u64,
) -> Vec<DerivedEvent> {
    // Find the last edge within [start, deadline]
    let edges = detect_edges(
        signal_path,
        data,
        expected_polarity,
        TimeBound::new(start, deadline).unwrap_or(TimeBound {
            from: start,
            to: start,
        }),
    );

    if edges.is_empty() {
        // No qualifying edge — emit timeout
        // Find the last-time-anything-happened for context
        let last_event_time = data
            .changes
            .iter()
            .rev()
            .find(|(t, _)| *t <= deadline)
            .map(|(t, _)| *t)
            .unwrap_or(0);

        vec![DerivedEvent::Timeout {
            description: format!(
                "expected {:?} edge on {} within [{}, {}], but none occurred",
                expected_polarity, signal_path, start, deadline
            ),
            deadline,
            last_event_time,
            signal: signal_path.to_string(),
            window: EvidenceWindow::new(
                start,
                Some(deadline),
                vec![signal_path.to_string()],
                format!(
                    "timeout check for {:?} edge on {}: deadline {} with last change at {}",
                    expected_polarity, signal_path, deadline, last_event_time
                ),
            ),
        }]
    } else {
        vec![]
    }
}

/// Detect state transitions on a multi-bit signal within `bounds`.
///
/// A state transition is any value change (including into/out-of
/// unknown).  Single-bit signals produce edge events instead —
/// prefer [`detect_edges`] for those.
pub fn detect_state_transitions(
    signal_path: &str,
    data: &SignalData,
    bounds: TimeBound,
) -> Vec<DerivedEvent> {
    let filtered: Vec<(u64, &CompactValue)> = data
        .changes
        .iter()
        .skip_while(|(t, _)| *t < bounds.from)
        .take_while(|(t, _)| *t <= bounds.to)
        .map(|(t, v)| (*t, v))
        .collect();

    if filtered.is_empty() {
        return vec![];
    }

    let mut events = Vec::new();

    // Seed the first-in-window change with the pre-window sample so
    // that a transition at bounds.from is not missed.
    let pre_val = data.sample(bounds.from.saturating_sub(1));
    let first = &filtered[0];
    if let Some(pv) = pre_val {
        if pv.as_str() != first.1.as_str() {
            events.push(DerivedEvent::StateTransition {
                time: first.0,
                signal: signal_path.to_string(),
                from_value: pv.as_str().to_string(),
                to_value: first.1.as_str().to_string(),
                window: EvidenceWindow::new(
                    bounds.from.saturating_sub(1),
                    Some(first.0),
                    vec![signal_path.to_string()],
                    format!("{}: {} → {}", signal_path, pv.as_str(), first.1.as_str()),
                ),
            });
        }
    }

    for i in 1..filtered.len() {
        let (prev_time, prev_val) = (&filtered[i - 1].0, filtered[i - 1].1);
        let (time, val) = (&filtered[i].0, filtered[i].1);

        if prev_val.as_str() != val.as_str() {
            events.push(DerivedEvent::StateTransition {
                time: *time,
                signal: signal_path.to_string(),
                from_value: prev_val.as_str().to_string(),
                to_value: val.as_str().to_string(),
                window: EvidenceWindow::new(
                    *prev_time,
                    Some(*time),
                    vec![signal_path.to_string()],
                    format!("{}: {} → {}", signal_path, prev_val.as_str(), val.as_str()),
                ),
            });
        }
    }
    events
}

// ── Top-level dispatch ─────────────────────────────────────────────

/// Which derivation to perform.
///
/// This enum is the request vocabulary for the unified `derive_events`
/// entry point.  Protocol analyzers and the CLI both use it.
#[derive(Debug, Clone)]
pub enum DerivationRequest {
    Edges {
        signals: Vec<String>,
        polarity: EdgePolarity,
    },
    SampleOnEdge {
        clock: String,
        targets: Vec<String>,
        edge: EdgePolarity,
    },
    Handshake {
        signal_a: String,
        signal_b: String,
    },
    Stalls {
        signals: Vec<String>,
        min_duration: u64,
    },
    Timeout {
        signal: String,
        expected_edge: EdgePolarity,
        start: u64,
        deadline: u64,
    },
    StateTransitions {
        signal: String,
    },
}

/// Run a derivation request against a pre-built [`TraceSlice`].
///
/// This is the main entry point for callers that already have a slice.
/// All events are returned in time order.
pub fn derive_events(slice: &TraceSlice, request: &DerivationRequest) -> Vec<DerivedEvent> {
    match request {
        DerivationRequest::Edges { signals, polarity } => {
            let mut events = Vec::new();
            for sig in signals {
                if let Some(idx) = slice.signal_index(sig) {
                    if let Some(data) = slice.data.get(idx) {
                        let mut e = detect_edges(sig, data, *polarity, slice.bounds);
                        events.append(&mut e);
                    }
                }
            }
            sort_by_time(&mut events);
            events
        }

        DerivationRequest::SampleOnEdge {
            clock,
            targets,
            edge,
        } => {
            let clock_idx = slice.signal_index(clock);
            let clock_data = clock_idx.and_then(|i| slice.data.get(i));
            if clock_data.is_none() {
                return vec![];
            }
            let clock_data = clock_data.unwrap();

            let target_pairs: Vec<(String, &SignalData)> = targets
                .iter()
                .filter_map(|t| {
                    slice
                        .signal_index(t)
                        .and_then(|i| slice.data.get(i))
                        .map(|d| (t.clone(), *d))
                })
                .collect();

            sample_on_edges(clock, clock_data, &target_pairs, *edge, slice.bounds)
        }

        DerivationRequest::Handshake { signal_a, signal_b } => {
            let idx_a = slice.signal_index(signal_a);
            let idx_b = slice.signal_index(signal_b);
            match (idx_a, idx_b) {
                (Some(ia), Some(ib)) => {
                    let data_a = slice.data.get(ia);
                    let data_b = slice.data.get(ib);
                    match (data_a, data_b) {
                        (Some(da), Some(db)) => {
                            detect_handshakes(signal_a, da, signal_b, db, slice.bounds)
                        }
                        _ => vec![],
                    }
                }
                _ => vec![],
            }
        }

        DerivationRequest::Stalls {
            signals,
            min_duration,
        } => {
            let mut events = Vec::new();
            for sig in signals {
                if let Some(idx) = slice.signal_index(sig) {
                    if let Some(data) = slice.data.get(idx) {
                        let mut e = detect_stalls(sig, data, *min_duration, slice.bounds);
                        events.append(&mut e);
                    }
                }
            }
            sort_by_time(&mut events);
            events
        }

        DerivationRequest::Timeout {
            signal,
            expected_edge,
            start,
            deadline,
        } => {
            if let Some(idx) = slice.signal_index(signal) {
                if let Some(data) = slice.data.get(idx) {
                    return check_timeout(signal, data, *expected_edge, *start, *deadline);
                }
            }
            vec![]
        }

        DerivationRequest::StateTransitions { signal } => {
            if let Some(idx) = slice.signal_index(signal) {
                if let Some(data) = slice.data.get(idx) {
                    return detect_state_transitions(signal, data, slice.bounds);
                }
            }
            vec![]
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────

fn sort_by_time(events: &mut [DerivedEvent]) {
    events.sort_by(|a, b| {
        let ta = a.time();
        let tb = b.time();
        ta.cmp(&tb)
            .then_with(|| a.signal_name().cmp(b.signal_name()))
    });
}

impl DerivedEvent {
    fn time(&self) -> u64 {
        match self {
            DerivedEvent::Edge { time, .. } => *time,
            DerivedEvent::SampleOnEdge { time, .. } => *time,
            DerivedEvent::Handshake { time, .. } => *time,
            DerivedEvent::Stall { since_time, .. } => *since_time,
            DerivedEvent::Timeout { deadline, .. } => *deadline,
            DerivedEvent::StateTransition { time, .. } => *time,
        }
    }

    fn signal_name(&self) -> &str {
        match self {
            DerivedEvent::Edge { signal, .. } => signal,
            DerivedEvent::SampleOnEdge { target, .. } => target,
            DerivedEvent::Handshake { signal_a, .. } => signal_a,
            DerivedEvent::Stall { signal, .. } => signal,
            DerivedEvent::Timeout { signal, .. } => signal,
            DerivedEvent::StateTransition { signal, .. } => signal,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::types::SignalData;

    fn make_signal_data(changes: Vec<(u64, &str)>) -> SignalData {
        SignalData {
            changes: changes
                .into_iter()
                .map(|(t, v)| (t, CompactValue::new(v)))
                .collect(),
        }
    }

    fn clk_data() -> SignalData {
        make_signal_data(vec![
            (0, "0"),
            (10, "1"),
            (40, "0"),
            (60, "1"),
            (80, "0"),
            (100, "1"),
        ])
    }

    fn en_data() -> SignalData {
        make_signal_data(vec![(0, "0"), (20, "1"), (70, "0")])
    }

    fn data_8bit() -> SignalData {
        make_signal_data(vec![
            (0, "00000000"),
            (30, "10100011"),
            (50, "01000010"),
            (90, "00000000"),
        ])
    }

    fn full_bounds() -> TimeBound {
        TimeBound::new(0, 100).unwrap()
    }

    // ── ValueClass ──────────────────────────────────────────────

    #[test]
    fn test_value_class_bit_0_is_low() {
        assert_eq!(
            ValueClass::classify(&CompactValue::new("0")),
            ValueClass::Low
        );
    }

    #[test]
    fn test_value_class_bit_1_is_high() {
        assert_eq!(
            ValueClass::classify(&CompactValue::new("1")),
            ValueClass::High
        );
    }

    #[test]
    fn test_value_class_x_is_unknown() {
        assert_eq!(
            ValueClass::classify(&CompactValue::new("x")),
            ValueClass::Unknown
        );
    }

    #[test]
    fn test_value_class_z_is_unknown() {
        assert_eq!(
            ValueClass::classify(&CompactValue::new("Z")),
            ValueClass::Unknown
        );
    }

    #[test]
    fn test_value_class_all_zeros_is_low() {
        assert_eq!(
            ValueClass::classify(&CompactValue::new("00000000")),
            ValueClass::Low
        );
    }

    #[test]
    fn test_value_class_nonzero_vector_is_high() {
        assert_eq!(
            ValueClass::classify(&CompactValue::new("10100011")),
            ValueClass::High
        );
    }

    #[test]
    fn test_value_class_vector_with_x_is_unknown() {
        assert_eq!(
            ValueClass::classify(&CompactValue::new("0x01")),
            ValueClass::Unknown
        );
    }

    // ── Edge detection ─────────────────────────────────────────

    #[test]
    fn test_detect_rising_edges() {
        let clk = clk_data();
        let events = detect_edges("top.clk", &clk, EdgePolarity::Rise, full_bounds());
        // clk: 0→1 at 10, 40→1 at 60, 80→1 at 100
        assert_eq!(events.len(), 3);
        let times: Vec<u64> = events.iter().map(|e| e.time()).collect();
        assert_eq!(times, vec![10, 60, 100]);
    }

    #[test]
    fn test_detect_falling_edges() {
        let clk = clk_data();
        let events = detect_edges("top.clk", &clk, EdgePolarity::Fall, full_bounds());
        // clk: 1→0 at 40, 1→0 at 80
        assert_eq!(events.len(), 2);
        let times: Vec<u64> = events.iter().map(|e| e.time()).collect();
        assert_eq!(times, vec![40, 80]);
    }

    #[test]
    fn test_detect_toggle_edges() {
        let clk = clk_data();
        let events = detect_edges("top.clk", &clk, EdgePolarity::Toggle, full_bounds());
        // Every change: 10, 40, 60, 80, 100
        assert_eq!(events.len(), 5);
    }

    #[test]
    fn test_detect_edges_respects_bounds() {
        let clk = clk_data();
        let bounds = TimeBound::new(20, 80).unwrap();
        let events = detect_edges("top.clk", &clk, EdgePolarity::Rise, bounds);
        // Within [20,80]: only the 40→1 edge at 60
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].time(), 60);
    }

    #[test]
    fn test_detect_edges_with_unknown_values() {
        let data = make_signal_data(vec![(0, "0"), (10, "x"), (20, "1"), (30, "z")]);
        let bounds = TimeBound::new(0, 30).unwrap();
        // Rise: x→1 at 20 is from unknown to high → not a rise
        let rising = detect_edges("sig", &data, EdgePolarity::Rise, bounds);
        assert!(rising.is_empty(), "x→1 should not count as a rise");

        // Toggle should catch all changes
        let toggles = detect_edges("sig", &data, EdgePolarity::Toggle, bounds);
        assert_eq!(toggles.len(), 3, "every value change is a toggle");
    }

    #[test]
    fn test_detect_edges_at_left_boundary() {
        // clk: (0,"0"),(10,"1"),(40,"0"),(60,"1"),(80,"0"),(100,"1")
        // Window [10, 100]: first change at 10 *is* a rising edge
        // because pre-window value at t<10 is "0".
        let clk = clk_data();
        let bounds = TimeBound::new(10, 100).unwrap();
        let events = detect_edges("top.clk", &clk, EdgePolarity::Rise, bounds);
        // Should detect: 10 (pre=0→1), 60 (pre=0→1), 100 (pre=0→1)
        assert_eq!(
            events.len(),
            3,
            "edge at exact window start must be detected"
        );
        assert_eq!(events[0].time(), 10);
        assert_eq!(events[1].time(), 60);
        assert_eq!(events[2].time(), 100);

        verify_rising_edge(&events[0], 10, "0", "1");
        verify_rising_edge(&events[1], 60, "0", "1");
        verify_rising_edge(&events[2], 100, "0", "1");
    }

    #[test]
    fn test_detect_edges_at_left_boundary_falling() {
        // Window [40, 100]: first change at 40 is a falling edge
        // because pre-window value at t<40 is "1".
        let clk = clk_data();
        let bounds = TimeBound::new(40, 100).unwrap();
        let events = detect_edges("top.clk", &clk, EdgePolarity::Fall, bounds);
        assert_eq!(events.len(), 2, "fall at 40 (pre=1→0) and fall at 80 (1→0)");
        assert_eq!(events[0].time(), 40);
        assert_eq!(events[1].time(), 80);
        verify_falling_edge(&events[0], 40, "1", "0");
    }

    #[test]
    fn test_detect_edges_at_left_boundary_toggle() {
        let clk = clk_data();
        let bounds = TimeBound::new(10, 100).unwrap();
        let events = detect_edges("top.clk", &clk, EdgePolarity::Toggle, bounds);
        // All five changes in [10,100] — including 10 as a toggle
        // (pre=0→1 at 10, then 1→0 at 40, 0→1 at 60, 1→0 at 80, 0→1 at 100)
        assert_eq!(events.len(), 5, "toggle at 10 must be included");
        assert_eq!(events[0].time(), 10);
        assert_eq!(events[4].time(), 100);
    }

    fn verify_rising_edge(
        ev: &DerivedEvent,
        expected_time: u64,
        expected_prev: &str,
        expected_next: &str,
    ) {
        match ev {
            DerivedEvent::Edge {
                time,
                polarity,
                prev_value,
                next_value,
                prev_class,
                next_class,
                ..
            } => {
                assert_eq!(*time, expected_time);
                assert_eq!(*polarity, EdgePolarity::Rise);
                assert_eq!(prev_value, expected_prev);
                assert_eq!(next_value, expected_next);
                assert_eq!(*prev_class, ValueClass::Low);
                assert_eq!(*next_class, ValueClass::High);
            }
            _ => panic!("expected Edge variant"),
        }
    }

    fn verify_falling_edge(
        ev: &DerivedEvent,
        expected_time: u64,
        expected_prev: &str,
        expected_next: &str,
    ) {
        match ev {
            DerivedEvent::Edge {
                time,
                polarity,
                prev_value,
                next_value,
                ..
            } => {
                assert_eq!(*time, expected_time);
                assert_eq!(*polarity, EdgePolarity::Fall);
                assert_eq!(prev_value, expected_prev);
                assert_eq!(next_value, expected_next);
            }
            _ => panic!("expected Edge variant"),
        }
    }

    #[test]
    fn test_detect_edges_single_change_no_edges() {
        let data = make_signal_data(vec![(0, "0")]);
        let bounds = TimeBound::new(0, 100).unwrap();
        let events = detect_edges("sig", &data, EdgePolarity::Rise, bounds);
        assert!(events.is_empty());
    }

    #[test]
    fn test_detect_edges_multi_signal() {
        use crate::backend::capabilities::BackendCapabilities;
        use crate::backend::metadata::WaveformMetadata;
        use crate::backend::types::{FileFormat, SignalInfo, Timescale};
        use crate::backend::WaveformBackend;
        use crate::error::WaveqlError;
        use crate::trace::TraceSliceRequest;
        use std::collections::HashMap;

        struct MockBackend {
            metadata: WaveformMetadata,
            signals: Vec<SignalInfo>,
            data: HashMap<String, SignalData>,
            capabilities: BackendCapabilities,
        }
        impl WaveformBackend for MockBackend {
            fn metadata(&self) -> &WaveformMetadata {
                &self.metadata
            }
            fn capabilities(&self) -> &BackendCapabilities {
                &self.capabilities
            }
            fn signal_info(&self, p: &str) -> Result<&SignalInfo, WaveqlError> {
                self.signals
                    .iter()
                    .find(|s| s.path == p)
                    .ok_or_else(|| WaveqlError::SignalNotFound(p.into()))
            }
            fn signal_iter(&self) -> Box<dyn Iterator<Item = &SignalInfo> + '_> {
                Box::new(self.signals.iter())
            }
            fn load_signals(&mut self, _: &[String]) -> Result<(), WaveqlError> {
                Ok(())
            }
            fn signal_data(&self, p: &str) -> Result<&SignalData, WaveqlError> {
                self.data
                    .get(p)
                    .ok_or_else(|| WaveqlError::SignalNotFound(p.into()))
            }
        }

        let backend = MockBackend {
            metadata: WaveformMetadata {
                timescale: Timescale::default(),
                date: None,
                version: None,
                signal_count: 2,
                format: FileFormat::Vcd,
            },
            signals: vec![
                SignalInfo {
                    path: "top.clk".into(),
                    width: 1,
                },
                SignalInfo {
                    path: "top.en".into(),
                    width: 1,
                },
            ],
            data: {
                let mut m = HashMap::new();
                m.insert("top.clk".into(), clk_data());
                m.insert("top.en".into(), en_data());
                m
            },
            capabilities: BackendCapabilities {
                supports_lazy_load: true,
                supports_slice: true,
                supports_incremental: false,
                format: FileFormat::Vcd,
                description: "mock",
            },
        };

        let bounds = full_bounds();
        let req = TraceSliceRequest::new(&backend, vec!["top.clk".into(), "top.en".into()], bounds);
        let slice = req.build().unwrap();

        let events = detect_edges_multi(&slice, EdgePolarity::Rise);
        // clk: 3 rises, en: 1 rise → 4 total
        assert_eq!(events.len(), 4);

        // Verify time order
        for w in events.windows(2) {
            assert!(w[0].time() <= w[1].time());
        }
    }

    // ── Sample-on-edge ─────────────────────────────────────────

    #[test]
    fn test_sample_on_rising_edges() {
        let clk = clk_data();
        let data = data_8bit();
        let targets = vec![("top.data".to_string(), &data)];
        let events = sample_on_edges("top.clk", &clk, &targets, EdgePolarity::Rise, full_bounds());
        // Rising edges at 10, 60, 100
        assert_eq!(events.len(), 3);
        if let DerivedEvent::SampleOnEdge {
            time, target_value, ..
        } = &events[0]
        {
            assert_eq!(*time, 10);
            // At time 10, data was last set to 00000000 at time 0
            assert_eq!(target_value, "00000000");
        }
        if let DerivedEvent::SampleOnEdge {
            time, target_value, ..
        } = &events[1]
        {
            assert_eq!(*time, 60);
            // At time 60, data was last set to 01000010 at time 50
            assert_eq!(target_value, "01000010");
        }
    }

    #[test]
    fn test_sample_on_edges_at_left_boundary() {
        // clk rises at 10, 60, 100. Window [10, 100]: first rise at 10
        // must be detected thanks to the detect_edges left-boundary fix.
        let clk = clk_data();
        let data = data_8bit();
        let targets = vec![("top.data".to_string(), &data)];
        let bounds = TimeBound::new(10, 100).unwrap();
        let events = sample_on_edges("top.clk", &clk, &targets, EdgePolarity::Rise, bounds);
        assert_eq!(
            events.len(),
            3,
            "sample-on-edge must include edge at window start"
        );
        if let DerivedEvent::SampleOnEdge {
            time, target_value, ..
        } = &events[0]
        {
            assert_eq!(*time, 10);
            assert_eq!(target_value, "00000000");
        } else {
            panic!("expected SampleOnEdge at time 10");
        }
    }

    // ── Handshake detection ────────────────────────────────────

    #[test]
    fn test_handshake_basic_cycle() {
        // Simulate a classic req/ack (valid/ready) handshake:
        // 0: both low
        // 10: A rises (initiated)
        // 30: B rises (acknowledged)
        // 50: A falls (released)
        // 70: B falls (completed)
        let a = make_signal_data(vec![(0, "0"), (10, "1"), (50, "0")]);
        let b = make_signal_data(vec![(0, "0"), (30, "1"), (70, "0")]);
        let events = detect_handshakes("top.valid", &a, "top.ready", &b, full_bounds());

        assert_eq!(events.len(), 4);
        match &events[0] {
            DerivedEvent::Handshake { time, phase, .. } => {
                assert_eq!(*time, 10);
                assert_eq!(*phase, HandshakePhase::Initiated);
            }
            _ => panic!("expected Handshake"),
        }
        match &events[1] {
            DerivedEvent::Handshake { time, phase, .. } => {
                assert_eq!(*time, 30);
                assert_eq!(*phase, HandshakePhase::Acknowledged);
            }
            _ => panic!("expected Handshake"),
        }
    }

    #[test]
    fn test_handshake_multiple_cycles() {
        // Two complete handshake cycles
        let a = make_signal_data(vec![(0, "0"), (10, "1"), (20, "0"), (40, "1"), (50, "0")]);
        let b = make_signal_data(vec![(0, "0"), (15, "1"), (25, "0"), (45, "1"), (55, "0")]);
        let events = detect_handshakes("top.valid", &a, "top.ready", &b, full_bounds());

        let phases: Vec<HandshakePhase> = events
            .iter()
            .filter_map(|e| {
                if let DerivedEvent::Handshake { phase, .. } = e {
                    Some(*phase)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            phases,
            vec![
                HandshakePhase::Initiated,
                HandshakePhase::Acknowledged,
                HandshakePhase::Released,
                HandshakePhase::Completed,
                HandshakePhase::Initiated,
                HandshakePhase::Acknowledged,
                HandshakePhase::Released,
                HandshakePhase::Completed,
            ]
        );
    }

    #[test]
    fn test_handshake_ambiguous_values_emitted_as_state_transition() {
        // Signal B goes to X — should produce a StateTransition event,
        // not silently be treated as a handshake.
        let a = make_signal_data(vec![(0, "0"), (10, "1"), (50, "0")]);
        let b = make_signal_data(vec![(0, "0"), (30, "x"), (70, "0")]);
        let events = detect_handshakes("top.valid", &a, "top.ready", &b, full_bounds());

        // Should have Initiated at 10, then StateTransition at 30 for B
        let has_state_transition = events
            .iter()
            .any(|e| matches!(e, DerivedEvent::StateTransition { .. }));
        assert!(
            has_state_transition,
            "x value on B should produce StateTransition"
        );
    }

    // ── Stall detection ────────────────────────────────────────

    #[test]
    fn test_detect_stalls_holds_long_enough() {
        let data = make_signal_data(vec![(0, "0"), (50, "1"), (60, "0")]);
        let bounds = TimeBound::new(0, 100).unwrap();
        let events = detect_stalls("top.en", &data, 30, bounds);

        // 0→50 is 50 time units ≥ 30 → stall on "0"
        // 60→100 is 40 time units ≥ 30 → stall on "0"
        assert_eq!(events.len(), 2);
        if let DerivedEvent::Stall {
            value, duration, ..
        } = &events[0]
        {
            assert_eq!(value, "0");
            assert_eq!(*duration, 50);
        }
        if let DerivedEvent::Stall {
            value, duration, ..
        } = &events[1]
        {
            assert_eq!(value, "0");
            assert_eq!(*duration, 40);
        }
    }

    #[test]
    fn test_detect_stalls_short_intervals_filtered() {
        let data = make_signal_data(vec![(0, "0"), (10, "1"), (20, "0")]);
        let bounds = TimeBound::new(0, 100).unwrap();
        let events = detect_stalls("top.en", &data, 30, bounds);

        // 20→100 is 80 ≥ 30 → one stall
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_detect_stalls_no_changes_in_window() {
        let data = make_signal_data(vec![(0, "0")]);
        let bounds = TimeBound::new(10, 100).unwrap();
        let events = detect_stalls("top.en", &data, 30, bounds);

        // No changes in [10,100]; pre-window value "0" held for 90 units → stall
        assert_eq!(events.len(), 1);
        if let DerivedEvent::Stall { duration, .. } = &events[0] {
            assert_eq!(*duration, 90);
        }
    }

    // ── Timeout detection ──────────────────────────────────────

    #[test]
    fn test_timeout_when_edge_missing() {
        let data = make_signal_data(vec![(0, "0")]);
        let events = check_timeout("top.clk", &data, EdgePolarity::Rise, 10, 100);

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], DerivedEvent::Timeout { .. }));
    }

    #[test]
    fn test_timeout_when_edge_present() {
        let clk = clk_data();
        let events = check_timeout("top.clk", &clk, EdgePolarity::Rise, 0, 20);
        // Rising edge at 10 is within [0,20]
        assert!(events.is_empty());
    }

    // ── State transitions ──────────────────────────────────────

    #[test]
    fn test_state_transitions_multi_bit() {
        let data = data_8bit();
        let events = detect_state_transitions("top.data", &data, full_bounds());
        // 00000000→10100011 at 30, 10100011→01000010 at 50, 01000010→00000000 at 90
        assert_eq!(events.len(), 3);

        if let DerivedEvent::StateTransition {
            time,
            from_value,
            to_value,
            ..
        } = &events[0]
        {
            assert_eq!(*time, 30);
            assert_eq!(from_value, "00000000");
            assert_eq!(to_value, "10100011");
        }
    }

    #[test]
    fn test_state_transitions_single_change_no_events() {
        let data = make_signal_data(vec![(0, "00000000")]);
        let events = detect_state_transitions("top.data", &data, full_bounds());
        assert!(events.is_empty());
    }

    #[test]
    fn test_state_transitions_at_left_boundary() {
        // data_8bit: (0,"00000000"),(30,"10100011"),(50,"01000010"),(90,"00000000")
        // Window [30, 100]: first change at 30 IS a transition
        // because pre-window value at t<30 is "00000000".
        let data = data_8bit();
        let bounds = TimeBound::new(30, 100).unwrap();
        let events = detect_state_transitions("top.data", &data, bounds);
        assert_eq!(
            events.len(),
            3,
            "transition at exact window start must be detected"
        );
        if let DerivedEvent::StateTransition {
            time,
            from_value,
            to_value,
            ..
        } = &events[0]
        {
            assert_eq!(*time, 30);
            assert_eq!(from_value, "00000000");
            assert_eq!(to_value, "10100011");
        } else {
            panic!("expected StateTransition");
        }
    }

    // ── Unified derive_events dispatch ─────────────────────────

    #[test]
    fn test_derive_events_edges() {
        use crate::backend::capabilities::BackendCapabilities;
        use crate::backend::metadata::WaveformMetadata;
        use crate::backend::types::{FileFormat, SignalInfo, Timescale};
        use crate::backend::WaveformBackend;
        use crate::error::WaveqlError;
        use crate::trace::TraceSliceRequest;
        use std::collections::HashMap;

        struct MB {
            m: WaveformMetadata,
            s: Vec<SignalInfo>,
            d: HashMap<String, SignalData>,
            c: BackendCapabilities,
        }
        impl WaveformBackend for MB {
            fn metadata(&self) -> &WaveformMetadata {
                &self.m
            }
            fn capabilities(&self) -> &BackendCapabilities {
                &self.c
            }
            fn signal_info(&self, p: &str) -> Result<&SignalInfo, WaveqlError> {
                self.s
                    .iter()
                    .find(|x| x.path == p)
                    .ok_or_else(|| WaveqlError::SignalNotFound(p.into()))
            }
            fn signal_iter(&self) -> Box<dyn Iterator<Item = &SignalInfo> + '_> {
                Box::new(self.s.iter())
            }
            fn load_signals(&mut self, _: &[String]) -> Result<(), WaveqlError> {
                Ok(())
            }
            fn signal_data(&self, p: &str) -> Result<&SignalData, WaveqlError> {
                self.d
                    .get(p)
                    .ok_or_else(|| WaveqlError::SignalNotFound(p.into()))
            }
        }

        let backend = MB {
            m: WaveformMetadata {
                timescale: Timescale::default(),
                date: None,
                version: None,
                signal_count: 1,
                format: FileFormat::Vcd,
            },
            s: vec![SignalInfo {
                path: "top.clk".into(),
                width: 1,
            }],
            d: {
                let mut m = HashMap::new();
                m.insert("top.clk".into(), clk_data());
                m
            },
            c: BackendCapabilities {
                supports_lazy_load: true,
                supports_slice: true,
                supports_incremental: false,
                format: FileFormat::Vcd,
                description: "mock",
            },
        };

        let req = TraceSliceRequest::new(&backend, vec!["top.clk".into()], full_bounds());
        let slice = req.build().unwrap();

        let events = derive_events(
            &slice,
            &DerivationRequest::Edges {
                signals: vec!["top.clk".into()],
                polarity: EdgePolarity::Rise,
            },
        );
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn test_derive_events_stalls() {
        use crate::backend::capabilities::BackendCapabilities;
        use crate::backend::metadata::WaveformMetadata;
        use crate::backend::types::{FileFormat, SignalInfo, Timescale};
        use crate::backend::WaveformBackend;
        use crate::error::WaveqlError;
        use crate::trace::TraceSliceRequest;
        use std::collections::HashMap;

        struct MB {
            m: WaveformMetadata,
            s: Vec<SignalInfo>,
            d: HashMap<String, SignalData>,
            c: BackendCapabilities,
        }
        impl WaveformBackend for MB {
            fn metadata(&self) -> &WaveformMetadata {
                &self.m
            }
            fn capabilities(&self) -> &BackendCapabilities {
                &self.c
            }
            fn signal_info(&self, p: &str) -> Result<&SignalInfo, WaveqlError> {
                self.s
                    .iter()
                    .find(|x| x.path == p)
                    .ok_or_else(|| WaveqlError::SignalNotFound(p.into()))
            }
            fn signal_iter(&self) -> Box<dyn Iterator<Item = &SignalInfo> + '_> {
                Box::new(self.s.iter())
            }
            fn load_signals(&mut self, _: &[String]) -> Result<(), WaveqlError> {
                Ok(())
            }
            fn signal_data(&self, p: &str) -> Result<&SignalData, WaveqlError> {
                self.d
                    .get(p)
                    .ok_or_else(|| WaveqlError::SignalNotFound(p.into()))
            }
        }

        let backend = MB {
            m: WaveformMetadata {
                timescale: Timescale::default(),
                date: None,
                version: None,
                signal_count: 1,
                format: FileFormat::Vcd,
            },
            s: vec![SignalInfo {
                path: "top.en".into(),
                width: 1,
            }],
            d: {
                let mut m = HashMap::new();
                m.insert("top.en".into(), en_data());
                m
            },
            c: BackendCapabilities {
                supports_lazy_load: true,
                supports_slice: true,
                supports_incremental: false,
                format: FileFormat::Vcd,
                description: "mock",
            },
        };

        let req = TraceSliceRequest::new(&backend, vec!["top.en".into()], full_bounds());
        let slice = req.build().unwrap();

        let events = derive_events(
            &slice,
            &DerivationRequest::Stalls {
                signals: vec!["top.en".into()],
                min_duration: 30,
            },
        );
        // en: 0→20 held at 0 for 20 (<30), 20→70 held at 1 for 50 (≥30), 70→100 held at 0 for 30 (≥30)
        assert_eq!(events.len(), 2);
    }
}

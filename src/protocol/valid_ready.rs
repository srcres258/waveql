// ── Generic Valid/Ready Handshake Analyzer ─────────────────────────
//
// Implements [`ProtocolPlugin`] for the classic two-signal handshake
// (valid / ready).  The analyzer is generic — it does not assume AXI
// sidebands, burst lengths, byte-enables, or other bus-family details.
//
// # Analysis
//
// Given a bound trace slice, the analyzer:
//
// 1. Derives handshake phases (Initiated / Acknowledged / Released /
//    Completed) via the existing derived-event engine.
// 2. Checks payload stability: between INITIATED and ACKNOWLEDGED the
//    optional `data` bus must not change.
// 3. Detects stalls: a handshake signal held at a value longer than
//    a configured threshold.
// 4. Identifies protocol violations:
//    - Spurious ready (ready asserted while valid is low)
//    - Premature valid deassertion (valid dropped before ready asserted)
//    - Unknown / ambiguous values on either control signal
//    - Payload instability during the valid-waiting-for-ready window
//
// Every finding carries an [`EvidenceWindow`] and a human-readable
// explanation.  The analyzer returns both a machine-readable
// [`AnalysisReport`] and a verdict (pass / fail).

use crate::events::{
    derive_events, DerivationRequest, DerivedEvent, EvidenceWindow, HandshakePhase,
};
use crate::index::SignalIndex;
use crate::protocol::{
    Anomaly, AnomalyKind, AnomalySeverity, BindingSuggestion, BindingValidation, ProtocolPlugin,
    RoleBinding, RoleDescriptor, RoleSchema,
};
use crate::query::{HandshakeInfo, StallInfo, ViolationInfo};
use crate::trace::TraceSlice;
use serde::Serialize;

// ── Schema ─────────────────────────────────────────────────────────

/// Role names used by this analyzer.
pub const VALID_ROLE: &str = "valid";
pub const READY_ROLE: &str = "ready";
pub const DATA_ROLE: &str = "data";
pub const CLK_ROLE: &str = "clk";

/// Built-in threshold (in native time units) for stall warnings.
/// 1000 time units is arbitrary — users should be able to override this
/// if the analyzer grows configuration support.
const DEFAULT_STALL_THRESHOLD: u64 = 1000;

// ── Analysis Configuration ─────────────────────────────────────────

/// Tunable knobs for the analyzer.
///
/// Everything has a sensible default so callers only override what
/// they care about.
#[derive(Debug, Clone)]
pub struct ValidReadyConfig {
    /// Minimum stall duration (native time units) that triggers a
    /// stall warning.  Default: 1000.
    pub stall_threshold: u64,

    /// When true, unknown values on `valid` or `ready` are treated as
    /// errors.  When false (default), they produce warnings.
    pub strict_unknown: bool,
}

impl Default for ValidReadyConfig {
    fn default() -> Self {
        ValidReadyConfig {
            stall_threshold: DEFAULT_STALL_THRESHOLD,
            strict_unknown: false,
        }
    }
}

// ── Analyzer ───────────────────────────────────────────────────────

/// Generic valid/ready handshake analyzer.
///
/// The analyzer is a protocol plugin: it declares its role schema,
/// validates bindings, and can be registered in a [`ProtocolCatalog`].
///
/// # Example
///
/// ```ignore
/// let mut catalog = ProtocolCatalog::new();
/// catalog.register(Box::new(ValidReadyAnalyzer::new()));
/// ```
pub struct ValidReadyAnalyzer {
    schema: RoleSchema,
    config: ValidReadyConfig,
}

impl ValidReadyAnalyzer {
    pub fn new() -> Self {
        ValidReadyAnalyzer {
            schema: RoleSchema {
                name: "valid_ready".into(),
                description: "Generic valid/ready handshake".into(),
                required_roles: vec![
                    RoleDescriptor {
                        name: VALID_ROLE.into(),
                        description: "Source asserts when data is available".into(),
                        width_hint: Some(1),
                    },
                    RoleDescriptor {
                        name: READY_ROLE.into(),
                        description: "Sink asserts when it can accept data".into(),
                        width_hint: Some(1),
                    },
                ],
                optional_roles: vec![
                    RoleDescriptor {
                        name: DATA_ROLE.into(),
                        description: "Payload bus — checked for stability during handshake".into(),
                        width_hint: None,
                    },
                    RoleDescriptor {
                        name: CLK_ROLE.into(),
                        description: "Clock reference (used for edge-aligned stall measurement)"
                            .into(),
                        width_hint: Some(1),
                    },
                ],
            },
            config: ValidReadyConfig::default(),
        }
    }

    pub fn with_config(config: ValidReadyConfig) -> Self {
        ValidReadyAnalyzer {
            schema: RoleSchema {
                name: "valid_ready".into(),
                description: "Generic valid/ready handshake".into(),
                required_roles: vec![
                    RoleDescriptor {
                        name: VALID_ROLE.into(),
                        description: "Source asserts when data is available".into(),
                        width_hint: Some(1),
                    },
                    RoleDescriptor {
                        name: READY_ROLE.into(),
                        description: "Sink asserts when it can accept data".into(),
                        width_hint: Some(1),
                    },
                ],
                optional_roles: vec![
                    RoleDescriptor {
                        name: DATA_ROLE.into(),
                        description: "Payload bus".into(),
                        width_hint: None,
                    },
                    RoleDescriptor {
                        name: CLK_ROLE.into(),
                        description: "Clock reference".into(),
                        width_hint: Some(1),
                    },
                ],
            },
            config,
        }
    }

    // ── Public analysis entry point ────────────────────────────

    /// Run the full valid/ready analysis over a pre-built trace slice.
    ///
    /// The `bindings` map provides the concrete signal paths for each
    /// role.  At minimum `valid` and `ready` must be present;
    /// `data` and `clk` are optional and enhance the analysis when
    /// provided.
    ///
    /// Returns a machine-readable analysis report.
    pub fn analyze(&self, slice: &TraceSlice, bindings: &[RoleBinding]) -> AnalysisReport {
        let valid_path = get_binding(bindings, VALID_ROLE);
        let ready_path = get_binding(bindings, READY_ROLE);
        let data_path = get_binding(bindings, DATA_ROLE);
        // _clk_path is reserved for future edge-aligned analysis
        let _clk_path = get_binding(bindings, CLK_ROLE);

        let mut violations: Vec<Anomaly> = Vec::new();
        let mut stalls: Vec<DerivedEvent> = Vec::new();

        // Step 1: Derive handshake phases
        let handshakes = match (&valid_path, &ready_path) {
            (Some(v), Some(r)) => {
                let req = DerivationRequest::Handshake {
                    signal_a: v.clone(),
                    signal_b: r.clone(),
                };
                derive_events(slice, &req)
            }
            _ => {
                violations.push(Anomaly::error(
                    AnomalyKind::BindingError,
                    "valid and ready roles must be bound".into(),
                    EvidenceWindow::new(
                        slice.bounds.from,
                        Some(slice.bounds.to),
                        slice.signals.clone(),
                        "missing required bindings".into(),
                    ),
                ));
                return AnalysisReport {
                    protocol: "valid_ready".into(),
                    summary: AnalysisSummary {
                        total_handshakes: 0,
                        total_violations: 1,
                        total_stalls: 0,
                        pass: false,
                    },
                    handshakes: vec![],
                    violations: violations.iter().map(violation_info_from_anomaly).collect(),
                    stalls: vec![],
                };
            }
        };

        let handshake_events: Vec<&DerivedEvent> = handshakes
            .iter()
            .filter(|e| matches!(e, DerivedEvent::Handshake { .. }))
            .collect();

        // Step 2: Check handshake ordering and detect protocol violations
        if let (Some(valid_path), Some(ready_path)) = (&valid_path, &ready_path) {
            let all_events = &handshakes;

            for (i, curr) in all_events.iter().enumerate() {
                match curr {
                    DerivedEvent::Handshake {
                        time,
                        phase,
                        signal_a: _,
                        signal_b: _,
                        window,
                    } => {
                        match phase {
                            HandshakePhase::Initiated => {
                                // Check if the previous cycle completed cleanly.
                                // Check: data must be stable between now and
                                // the next Acknowledged phase.
                                if let Some(data_path) = &data_path {
                                    let ack_time = find_next_phase(
                                        all_events,
                                        i,
                                        HandshakePhase::Acknowledged,
                                    );
                                    if let Some(end) = ack_time {
                                        let instability = check_payload_stability(
                                            slice,
                                            data_path,
                                            *time,
                                            end,
                                            valid_path.clone(),
                                        );
                                        violations.extend(instability);
                                    }
                                }
                            }
                            HandshakePhase::Acknowledged => {
                                // Check: was valid held throughout Initiated→Acknowledged?
                                let init_time =
                                    find_prev_phase(all_events, i, HandshakePhase::Initiated);
                                if init_time.is_none() {
                                    violations.push(Anomaly::error(
                                        AnomalyKind::HandshakeOrderViolation,
                                        format!(
                                            "ready asserted at {} without preceding valid assertion",
                                            window.start
                                        ),
                                        window.clone(),
                                    )
                                    .with_roles(vec![
                                        VALID_ROLE.into(),
                                        READY_ROLE.into(),
                                    ]));
                                }
                            }
                            HandshakePhase::Released => {
                                // Check: was ready still asserted before release?
                                let ack_time =
                                    find_prev_phase(all_events, i, HandshakePhase::Acknowledged);
                                if ack_time.is_none() {
                                    violations.push(
                                        Anomaly::warning(
                                            AnomalyKind::HandshakeOrderViolation,
                                            format!(
                                            "valid deasserted at {} without prior acknowledgment",
                                            window.start
                                        ),
                                            window.clone(),
                                        )
                                        .with_roles(vec![VALID_ROLE.into(), READY_ROLE.into()]),
                                    );
                                }
                            }
                            HandshakePhase::Completed => {
                                // Cycle complete — nothing to flag.
                            }
                        }
                    }
                    DerivedEvent::StateTransition {
                        time,
                        signal,
                        from_value,
                        to_value,
                        window,
                    } => {
                        let role = if signal == valid_path.as_str() {
                            VALID_ROLE
                        } else {
                            READY_ROLE
                        };
                        let severity = if self.config.strict_unknown {
                            AnomalySeverity::Error
                        } else {
                            AnomalySeverity::Warning
                        };
                        violations.push(Anomaly {
                            kind: AnomalyKind::AmbiguousValue,
                            severity,
                            description: format!(
                                "{} changed to unknown value at {}: {} → {}",
                                signal, time, from_value, to_value,
                            ),
                            related_roles: vec![role.into()],
                            related_signals: vec![signal.clone()],
                            evidence: window.clone(),
                        });
                    }
                    _ => {}
                }
            }

            // Check for spurious ready: ready asserted with valid=0
            check_spurious_ready(slice, valid_path, ready_path, &mut violations);

            // Check for premature deassert: valid dropped before ready
            check_premature_deassert(all_events, valid_path, ready_path, &mut violations);
        }

        // Step 3: Stall detection on both control signals
        let stall_signals: Vec<String> = {
            let mut sigs = Vec::new();
            if let Some(v) = &valid_path {
                sigs.push(v.clone());
            }
            if let Some(r) = &ready_path {
                sigs.push(r.clone());
            }
            sigs
        };

        if !stall_signals.is_empty() {
            let stall_req = DerivationRequest::Stalls {
                signals: stall_signals,
                min_duration: self.config.stall_threshold,
            };
            let raw_stalls = derive_events(slice, &stall_req);
            for s in &raw_stalls {
                if let DerivedEvent::Stall {
                    signal,
                    duration,
                    window,
                    ..
                } = s
                {
                    let role = if valid_path.as_deref() == Some(signal.as_str()) {
                        VALID_ROLE
                    } else {
                        READY_ROLE
                    };
                    let severity = if *duration > self.config.stall_threshold * 2 {
                        AnomalySeverity::Warning
                    } else {
                        AnomalySeverity::Info
                    };
                    violations.push(Anomaly {
                        kind: AnomalyKind::Stall,
                        severity,
                        description: format!("{} stalled for {} time units", signal, duration,),
                        related_roles: vec![role.into()],
                        related_signals: vec![signal.clone()],
                        evidence: window.clone(),
                    });
                }
            }
            stalls = raw_stalls;
        }

        // Step 4: Build the report
        let total_handshakes = handshake_events.len() / 4; // 4 phases per cycle
        let total_violations = violations
            .iter()
            .filter(|v| v.severity == AnomalySeverity::Error)
            .count();
        let has_errors = violations
            .iter()
            .any(|v| v.severity == AnomalySeverity::Error);

        AnalysisReport {
            protocol: "valid_ready".into(),
            summary: AnalysisSummary {
                total_handshakes,
                total_violations,
                total_stalls: stalls.len(),
                pass: !has_errors,
            },
            handshakes: handshake_events
                .iter()
                .filter_map(|e| {
                    if let DerivedEvent::Handshake {
                        time,
                        phase,
                        signal_a,
                        signal_b,
                        ..
                    } = e
                    {
                        Some(HandshakeInfo {
                            time: *time,
                            phase: format!("{:?}", phase).to_lowercase(),
                            signal_a: signal_a.clone(),
                            signal_b: signal_b.clone(),
                        })
                    } else {
                        None
                    }
                })
                .collect(),
            violations: violations.iter().map(violation_info_from_anomaly).collect(),
            stalls: stalls
                .iter()
                .filter_map(|e| {
                    if let DerivedEvent::Stall {
                        signal,
                        value,
                        since_time,
                        duration,
                        ..
                    } = e
                    {
                        Some(StallInfo {
                            signal: signal.clone(),
                            value: value.clone(),
                            since_time: *since_time,
                            duration: *duration,
                        })
                    } else {
                        None
                    }
                })
                .collect(),
        }
    }
}

impl Default for ValidReadyAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolPlugin for ValidReadyAnalyzer {
    fn schema(&self) -> &RoleSchema {
        &self.schema
    }

    fn validate_bindings(
        &self,
        bindings: &[RoleBinding],
        index: &SignalIndex,
    ) -> BindingValidation {
        let mut missing_roles = Vec::new();
        let mut warnings = Vec::new();
        let mut suggestions = Vec::new();

        for role in &self.schema.required_roles {
            let has_binding = bindings
                .iter()
                .any(|b| b.role.to_lowercase() == role.name.to_lowercase());
            if !has_binding {
                missing_roles.push(role.name.clone());
                // Generate suggestions for unbound required roles
                let candidates = self.suggest_candidates_for_role(&role.name, index);
                if !candidates.is_empty() {
                    suggestions.push(BindingSuggestion {
                        role: role.name.clone(),
                        candidates,
                    });
                }
            }
        }

        // Check bound signals exist in the index
        for binding in bindings {
            if index.lookup_exact(&binding.signal_path).is_none() {
                warnings.push(format!(
                    "bound signal '{}' (role '{}') not found in waveform index",
                    binding.signal_path, binding.role,
                ));
            }
        }

        // Check optional role warnings
        for role in &self.schema.optional_roles {
            let has_binding = bindings
                .iter()
                .any(|b| b.role.to_lowercase() == role.name.to_lowercase());
            if !has_binding {
                warnings.push(format!(
                    "optional role '{}' is not bound — analysis will be limited",
                    role.name,
                ));
            }
        }

        if missing_roles.is_empty() {
            BindingValidation {
                is_valid: true,
                missing_roles,
                warnings,
                suggestions,
            }
        } else {
            BindingValidation {
                is_valid: false,
                missing_roles,
                warnings,
                suggestions,
            }
        }
    }

    fn suggest_bindings(&self, index: &SignalIndex) -> Vec<BindingSuggestion> {
        let role_names: Vec<&str> = self
            .schema
            .required_roles
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        role_names
            .iter()
            .filter_map(|role| {
                let candidates = self.suggest_candidates_for_role(role, index);
                if candidates.is_empty() {
                    None
                } else {
                    Some(BindingSuggestion {
                        role: role.to_string(),
                        candidates,
                    })
                }
            })
            .collect()
    }

    fn name(&self) -> &str {
        &self.schema.name
    }

    fn description(&self) -> &str {
        &self.schema.description
    }
}

// ── Suggestion helpers ─────────────────────────────────────────────

impl ValidReadyAnalyzer {
    fn suggest_candidates_for_role(
        &self,
        role: &str,
        index: &SignalIndex,
    ) -> Vec<crate::protocol::BindingCandidate> {
        let role_lower = role.to_lowercase();
        let mut candidates = Vec::new();

        for entry in index.entries() {
            let name_lower = entry.short_name.to_lowercase();
            let mut confidence: f64 = 0.0;
            let mut reason = String::new();

            if name_lower == role_lower {
                confidence = 0.95;
                reason = format!("short name matches '{}' exactly", role);
            } else if name_lower.contains(&role_lower) {
                confidence = 0.6;
                reason = format!("short name '{}' contains '{}'", entry.short_name, role);
            } else if role_lower.contains(&name_lower) {
                confidence = 0.5;
                reason = format!("role '{}' contains short name '{}'", role, entry.short_name);
            } else if name_lower.starts_with(&role_lower) {
                confidence = 0.7;
                reason = format!("short name '{}' starts with '{}'", entry.short_name, role);
            }

            if confidence > 0.0 {
                candidates.push(crate::protocol::BindingCandidate {
                    signal_path: entry.path.clone(),
                    confidence,
                    reason,
                });
            }
        }

        candidates.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        candidates.truncate(5);
        candidates
    }
}

// ── Analysis output types ──────────────────────────────────────────

/// Machine-readable report produced by [`ValidReadyAnalyzer::analyze`].
///
/// The report is serialized for JSON output and consumed by the
/// text/table formatters in the planner module.
#[derive(Debug, Clone, Serialize)]
pub struct AnalysisReport {
    /// Protocol name (always `"valid_ready"` for this analyzer).
    pub protocol: String,

    /// One-line summary for quick inspection.
    pub summary: AnalysisSummary,

    /// Detected handshake phases in time order.
    pub handshakes: Vec<HandshakeInfo>,

    /// Protocol violations with evidence windows.
    pub violations: Vec<ViolationInfo>,

    /// Stall events that exceeded the configured threshold.
    pub stalls: Vec<StallInfo>,
}

/// High-level summary of analysis results.
#[derive(Debug, Clone, Serialize)]
pub struct AnalysisSummary {
    /// Number of complete handshake cycles detected (each cycle has
    /// 4 phases: Initiated → Acknowledged → Released → Completed).
    pub total_handshakes: usize,

    /// Number of errors (severity ≤ Error).
    pub total_violations: usize,

    /// Number of stall events exceeding the threshold.
    pub total_stalls: usize,

    /// Overall verdict: `true` when no protocol errors were found.
    pub pass: bool,
}

// ── Internal helpers ───────────────────────────────────────────────

/// Get the bound signal path for a given role name.
fn get_binding(bindings: &[RoleBinding], role: &str) -> Option<String> {
    let lower = role.to_lowercase();
    bindings
        .iter()
        .find(|b| b.role.to_lowercase() == lower)
        .map(|b| b.signal_path.clone())
}

/// Find the time of the next phase of a given kind after `start_idx`.
fn find_next_phase(
    events: &[DerivedEvent],
    start_idx: usize,
    phase: HandshakePhase,
) -> Option<u64> {
    events.iter().skip(start_idx + 1).find_map(|e| {
        if let DerivedEvent::Handshake { time, phase: p, .. } = e {
            if *p == phase {
                return Some(*time);
            }
        }
        None
    })
}

/// Find the time of the previous phase of a given kind before `end_idx`.
fn find_prev_phase(events: &[DerivedEvent], end_idx: usize, phase: HandshakePhase) -> Option<u64> {
    events[..end_idx].iter().rev().find_map(|e| {
        if let DerivedEvent::Handshake { time, phase: p, .. } = e {
            if *p == phase {
                return Some(*time);
            }
        }
        None
    })
}

/// Check that the data signal is stable between `start` and `end`.
/// Returns violations for any data changes detected within the window.
fn check_payload_stability(
    slice: &TraceSlice,
    data_path: &str,
    start: u64,
    end: u64,
    valid_path: String,
) -> Vec<Anomaly> {
    let mut anomalies = Vec::new();

    let data_idx = match slice.signal_index(data_path) {
        Some(i) => i,
        None => return anomalies,
    };

    let data = match slice.data.get(data_idx) {
        Some(d) => d,
        None => return anomalies,
    };

    let start_value = data.sample(start);

    for (t, v) in &data.changes {
        if *t > start && *t <= end {
            if let Some(sv) = start_value {
                if sv.as_str() != v.as_str() {
                    anomalies.push(
                        Anomaly::error(
                            AnomalyKind::PayloadInstability,
                            format!(
                                "payload {} changed at {} from {} to {} while valid asserted \
                             (valid window [{}, {}])",
                                data_path,
                                t,
                                sv.as_str(),
                                v.as_str(),
                                start,
                                end,
                            ),
                            EvidenceWindow::new(
                                start,
                                Some(end),
                                vec![valid_path.clone(), data_path.to_string()],
                                format!(
                                    "payload stability check: {} between [{}, {}]",
                                    data_path, start, end,
                                ),
                            ),
                        )
                        .with_roles(vec!["valid".into(), "data".into()]),
                    );
                    break; // Only report first violation in each window
                }
            }
        }
    }

    anomalies
}

/// Detect ready asserted while valid is low (spurious ready).
fn check_spurious_ready(
    slice: &TraceSlice,
    valid_path: &str,
    ready_path: &str,
    violations: &mut Vec<Anomaly>,
) {
    let v_idx = slice.signal_index(valid_path);
    let r_idx = slice.signal_index(ready_path);
    let (v_data, r_data) = match (v_idx, r_idx) {
        (Some(vi), Some(ri)) => match (slice.data.get(vi), slice.data.get(ri)) {
            (Some(vd), Some(rd)) => (vd, rd),
            _ => return,
        },
        _ => return,
    };

    for (t, v) in &r_data.changes {
        if *t < slice.bounds.from || *t > slice.bounds.to {
            continue;
        }
        let prev_time = t.saturating_sub(1);
        let prev_val = r_data.sample(prev_time);
        // Detect ready rising edge
        if let Some(prev) = prev_val {
            let prev_high = crate::events::ValueClass::classify(prev).is_high();
            let new_high = crate::events::ValueClass::classify(v).is_high();
            if !prev_high && new_high {
                // Ready rose — check valid at this time
                let valid_val = v_data.sample(*t);
                let valid_high = valid_val
                    .map(|vv| crate::events::ValueClass::classify(vv).is_high())
                    .unwrap_or(false);
                if !valid_high {
                    violations.push(
                        Anomaly::warning(
                            AnomalyKind::HandshakeOrderViolation,
                            format!(
                                "ready asserted at {} while valid is low — spurious ready",
                                t,
                            ),
                            EvidenceWindow::new(
                                *t,
                                None,
                                vec![valid_path.to_string(), ready_path.to_string()],
                                "spurious ready check".into(),
                            ),
                        )
                        .with_roles(vec!["valid".into(), "ready".into()]),
                    );
                }
            }
        }
    }
}

/// Detect valid deasserted before ready was asserted.
fn check_premature_deassert(
    events: &[DerivedEvent],
    valid_path: &str,
    ready_path: &str,
    violations: &mut Vec<Anomaly>,
) {
    // Look for Released phases that have no preceding Acknowledged in
    // the same cycle.
    for (i, e) in events.iter().enumerate() {
        if let DerivedEvent::Handshake { phase, window, .. } = e {
            if *phase == HandshakePhase::Released {
                let ack = find_prev_phase(events, i, HandshakePhase::Acknowledged);
                if ack.is_none() {
                    // Valid deasserted without ready having acknowledged
                    let init = find_prev_phase(events, i, HandshakePhase::Initiated);
                    violations.push(
                        Anomaly::error(
                            AnomalyKind::HandshakeOrderViolation,
                            format!(
                                "valid deasserted at {} without ready acknowledging — \
                             premature deassertion",
                                window.start,
                            ),
                            EvidenceWindow::new(
                                init.unwrap_or(window.start),
                                Some(window.start),
                                vec![valid_path.to_string(), ready_path.to_string()],
                                "premature deassert check".into(),
                            ),
                        )
                        .with_roles(vec!["valid".into(), "ready".into()]),
                    );
                }
            }
        }
    }
}

/// Map an [`Anomaly`] to a [`ViolationInfo`] for the analysis report.
fn violation_info_from_anomaly(a: &Anomaly) -> ViolationInfo {
    ViolationInfo {
        kind: format!("{:?}", a.kind).to_lowercase(),
        severity: match a.severity {
            AnomalySeverity::Error => "error".into(),
            AnomalySeverity::Warning => "warning".into(),
            AnomalySeverity::Info => "info".into(),
        },
        description: a.description.clone(),
        related_roles: a.related_roles.clone(),
        related_signals: a.related_signals.clone(),
        evidence_start: a.evidence.start,
        evidence_end: a.evidence.end,
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::capabilities::BackendCapabilities;
    use crate::backend::metadata::WaveformMetadata;
    use crate::backend::types::{CompactValue, FileFormat, SignalData, SignalInfo, Timescale};
    use crate::backend::WaveformBackend;
    use crate::protocol::RoleBinding;
    use crate::trace::{TimeBound, TraceSliceRequest};
    use crate::WaveqlError;
    use std::collections::HashMap;

    fn make_valid_ready_signals() -> (SignalData, SignalData) {
        // Classic valid/ready handshake with two complete cycles:
        // t=0:  valid=0, ready=0
        // t=10: valid=1 (initiated)
        // t=30: ready=1 (acknowledged)
        // t=50: valid=0 (released)
        // t=70: ready=0 (completed)
        // t=90: valid=1 (initiated, cycle 2)
        // t=110: ready=1 (acknowledged)
        // t=130: valid=0 (released)
        // t=150: ready=0 (completed)
        let valid = SignalData {
            changes: vec![
                (0, CompactValue::new("0")),
                (10, CompactValue::new("1")),
                (50, CompactValue::new("0")),
                (90, CompactValue::new("1")),
                (130, CompactValue::new("0")),
            ],
        };
        let ready = SignalData {
            changes: vec![
                (0, CompactValue::new("0")),
                (30, CompactValue::new("1")),
                (70, CompactValue::new("0")),
                (110, CompactValue::new("1")),
                (150, CompactValue::new("0")),
            ],
        };
        (valid, ready)
    }

    struct TestBackend {
        metadata: WaveformMetadata,
        signals: Vec<SignalInfo>,
        data: HashMap<String, SignalData>,
        capabilities: BackendCapabilities,
    }

    impl WaveformBackend for TestBackend {
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

    fn make_backend(
        valid: SignalData,
        ready: SignalData,
        extra_signals: Vec<(&str, SignalData)>,
    ) -> TestBackend {
        let mut signals = vec![
            SignalInfo {
                path: "top.valid".into(),
                width: 1,
            },
            SignalInfo {
                path: "top.ready".into(),
                width: 1,
            },
        ];
        let mut data = HashMap::new();
        data.insert("top.valid".into(), valid);
        data.insert("top.ready".into(), ready);

        for (path, sd) in extra_signals {
            signals.push(SignalInfo {
                path: path.into(),
                width: 8,
            });
            data.insert(path.into(), sd);
        }

        TestBackend {
            metadata: WaveformMetadata {
                timescale: Timescale::default(),
                date: None,
                version: None,
                signal_count: signals.len(),
                format: FileFormat::Vcd,
            },
            signals,
            data,
            capabilities: BackendCapabilities {
                supports_lazy_load: true,
                supports_slice: true,
                supports_incremental: false,
                format: FileFormat::Vcd,
                description: "test",
            },
        }
    }

    fn run_analysis(backend: &TestBackend, bindings: &[RoleBinding]) -> AnalysisReport {
        run_analysis_with_bounds(backend, bindings, TimeBound::new(0, 200).unwrap())
    }

    fn run_analysis_with_bounds(
        backend: &TestBackend,
        bindings: &[RoleBinding],
        bounds: TimeBound,
    ) -> AnalysisReport {
        let analyzer = ValidReadyAnalyzer::new();
        let signal_paths: Vec<String> = bindings.iter().map(|b| b.signal_path.clone()).collect();
        let req = TraceSliceRequest::new(backend, signal_paths, bounds);
        let slice = req.build().expect("slice should build");

        analyzer.analyze(&slice, bindings)
    }

    // ── Happy path tests ─────────────────────────────────────

    #[test]
    fn test_clean_handshake_produces_two_cycles() {
        let (valid, ready) = make_valid_ready_signals();
        let backend = make_backend(valid, ready, vec![]);
        let bindings = vec![
            RoleBinding::user_specified("valid", "top.valid"),
            RoleBinding::user_specified("ready", "top.ready"),
        ];

        let report = run_analysis(&backend, &bindings);
        assert!(report.summary.pass, "clean trace should pass");
        assert_eq!(report.summary.total_handshakes, 2);
        assert_eq!(report.summary.total_violations, 0);
        assert_eq!(report.handshakes.len(), 8); // 4 phases × 2 cycles

        // Verify phase order in first cycle
        let phases: Vec<&str> = report
            .handshakes
            .iter()
            .take(4)
            .map(|h| h.phase.as_str())
            .collect();
        assert_eq!(
            phases,
            vec!["initiated", "acknowledged", "released", "completed"]
        );
    }

    #[test]
    fn test_clean_handshake_produces_no_violations() {
        let (valid, ready) = make_valid_ready_signals();
        let backend = make_backend(valid, ready, vec![]);
        let bindings = vec![
            RoleBinding::user_specified("valid", "top.valid"),
            RoleBinding::user_specified("ready", "top.ready"),
        ];

        let report = run_analysis(&backend, &bindings);
        assert!(report.violations.is_empty(), "clean trace: no violations");
    }

    // ── Payload stability tests ──────────────────────────────

    #[test]
    fn test_payload_instability_detected() {
        let (valid, ready) = make_valid_ready_signals();
        // Data changes at t=20 while valid=1 and ready=0 (between
        // Initiated at 10 and Acknowledged at 30)
        let data = SignalData {
            changes: vec![
                (0, CompactValue::new("00000000")),
                (20, CompactValue::new("11111111")),
                (50, CompactValue::new("00000000")),
            ],
        };
        let backend = make_backend(valid, ready, vec![("top.data", data)]);
        let bindings = vec![
            RoleBinding::user_specified("valid", "top.valid"),
            RoleBinding::user_specified("ready", "top.ready"),
            RoleBinding::user_specified("data", "top.data"),
        ];

        let report = run_analysis(&backend, &bindings);
        assert!(!report.summary.pass, "payload instability should fail");
        assert!(
            report.summary.total_violations >= 1,
            "payload instability should produce violation"
        );
        let inst_violations: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.kind == "payloadinstability")
            .collect();
        assert!(
            !inst_violations.is_empty(),
            "should have payload instability violation"
        );
    }

    #[test]
    fn test_stable_payload_produces_no_violation() {
        let (valid, ready) = make_valid_ready_signals();
        // Data never changes — always stable
        let data = SignalData {
            changes: vec![(0, CompactValue::new("10101010"))],
        };
        let backend = make_backend(valid, ready, vec![("top.data", data)]);
        let bindings = vec![
            RoleBinding::user_specified("valid", "top.valid"),
            RoleBinding::user_specified("ready", "top.ready"),
            RoleBinding::user_specified("data", "top.data"),
        ];

        let report = run_analysis(&backend, &bindings);
        assert!(report.summary.pass, "stable payload should pass");
        let inst_violations: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.kind == "payloadinstability")
            .collect();
        assert!(inst_violations.is_empty(), "no payload instability");
    }

    // ── Stall detection ──────────────────────────────────────

    #[test]
    fn test_long_stall_detected() {
        // valid held high from t=10 to t=50000 without ready responding
        let valid = SignalData {
            changes: vec![
                (0, CompactValue::new("0")),
                (10, CompactValue::new("1")),
                (50000, CompactValue::new("0")),
            ],
        };
        let ready = SignalData {
            changes: vec![
                (0, CompactValue::new("0")),
                (50000, CompactValue::new("1")),
                (50100, CompactValue::new("0")),
            ],
        };
        let backend = make_backend(valid, ready, vec![]);
        let bindings = vec![
            RoleBinding::user_specified("valid", "top.valid"),
            RoleBinding::user_specified("ready", "top.ready"),
        ];

        // Use a wide window to capture the long stall
        let bounds = TimeBound::new(0, 60000).unwrap();
        let report = run_analysis_with_bounds(&backend, &bindings, bounds);
        assert!(
            report.summary.total_stalls > 0 || report.summary.total_violations > 0,
            "long stall should be detected"
        );
    }

    // ── Spurious ready detection ─────────────────────────────

    #[test]
    fn test_spurious_ready_detected() {
        // ready asserted while valid is still low
        let valid = SignalData {
            changes: vec![(0, CompactValue::new("0")), (50, CompactValue::new("1"))],
        };
        let ready = SignalData {
            changes: vec![(0, CompactValue::new("0")), (20, CompactValue::new("1"))],
        };
        let backend = make_backend(valid, ready, vec![]);
        let bindings = vec![
            RoleBinding::user_specified("valid", "top.valid"),
            RoleBinding::user_specified("ready", "top.ready"),
        ];

        let report = run_analysis(&backend, &bindings);
        let has_spurious = report
            .violations
            .iter()
            .any(|v| v.description.contains("spurious"));
        assert!(has_spurious, "spurious ready should be flagged");
    }

    // ── Premature deassert ───────────────────────────────────

    #[test]
    fn test_premature_deassert_detected() {
        // valid deasserted before ready acknowledged
        let valid = SignalData {
            changes: vec![
                (0, CompactValue::new("0")),
                (10, CompactValue::new("1")),
                (20, CompactValue::new("0")),
            ],
        };
        let ready = SignalData {
            changes: vec![(0, CompactValue::new("0")), (50, CompactValue::new("1"))],
        };
        let backend = make_backend(valid, ready, vec![]);
        let bindings = vec![
            RoleBinding::user_specified("valid", "top.valid"),
            RoleBinding::user_specified("ready", "top.ready"),
        ];

        let report = run_analysis(&backend, &bindings);
        let has_premature = report
            .violations
            .iter()
            .any(|v| v.description.contains("premature"));
        assert!(has_premature, "premature deassert should be flagged");
    }

    // ── Unknown values ───────────────────────────────────────

    #[test]
    fn test_unknown_values_produce_warning() {
        let valid = SignalData {
            changes: vec![
                (0, CompactValue::new("0")),
                (10, CompactValue::new("x")),
                (50, CompactValue::new("0")),
            ],
        };
        let ready = SignalData {
            changes: vec![
                (0, CompactValue::new("0")),
                (30, CompactValue::new("1")),
                (70, CompactValue::new("0")),
            ],
        };
        let backend = make_backend(valid, ready, vec![]);
        let bindings = vec![
            RoleBinding::user_specified("valid", "top.valid"),
            RoleBinding::user_specified("ready", "top.ready"),
        ];

        let report = run_analysis(&backend, &bindings);
        // Unknown values should produce at least one StateTransition event
        // which becomes an AmbiguousValue violation (or warning)
        let has_ambiguous = report.violations.iter().any(|v| v.kind == "ambiguousvalue");
        assert!(
            has_ambiguous || !report.summary.pass,
            "unknown values should be flagged"
        );
    }

    // ── Binding validation ───────────────────────────────────

    #[test]
    fn test_missing_required_role_fails_validation() {
        let analyzer = ValidReadyAnalyzer::new();
        let bindings = vec![RoleBinding::user_specified("valid", "top.valid")];
        // Build an index from a simple signal list
        let backend = make_backend(
            SignalData { changes: vec![] },
            SignalData { changes: vec![] },
            vec![],
        );
        let index = crate::index::SignalIndex::build(&backend);

        let validation = analyzer.validate_bindings(&bindings, &index);
        assert!(!validation.is_valid, "missing ready role should fail");
        assert!(validation.missing_roles.contains(&"ready".to_string()));
    }

    #[test]
    fn test_full_binding_passes_validation() {
        let analyzer = ValidReadyAnalyzer::new();
        let bindings = vec![
            RoleBinding::user_specified("valid", "top.valid"),
            RoleBinding::user_specified("ready", "top.ready"),
        ];
        let backend = make_backend(
            SignalData { changes: vec![] },
            SignalData { changes: vec![] },
            vec![],
        );
        let index = crate::index::SignalIndex::build(&backend);

        let validation = analyzer.validate_bindings(&bindings, &index);
        assert!(validation.is_valid, "full binding should pass");
    }

    #[test]
    fn test_binding_signal_not_in_index_warns() {
        let analyzer = ValidReadyAnalyzer::new();
        let bindings = vec![
            RoleBinding::user_specified("valid", "top.valid"),
            RoleBinding::user_specified("ready", "top.nonexistent"),
        ];
        let backend = make_backend(
            SignalData { changes: vec![] },
            SignalData { changes: vec![] },
            vec![],
        );
        let index = crate::index::SignalIndex::build(&backend);

        let validation = analyzer.validate_bindings(&bindings, &index);
        assert!(
            validation
                .warnings
                .iter()
                .any(|w| w.contains("top.nonexistent")),
            "should warn about nonexistent signal"
        );
    }

    // ── Plugin trait compliance ──────────────────────────────

    #[test]
    fn test_plugin_name_and_description() {
        let analyzer = ValidReadyAnalyzer::new();
        assert_eq!(analyzer.name(), "valid_ready");
        assert!(!analyzer.description().is_empty());
    }

    #[test]
    fn test_plugin_schema_has_required_and_optional_roles() {
        let analyzer = ValidReadyAnalyzer::new();
        let schema = analyzer.schema();
        assert_eq!(schema.required_roles.len(), 2);
        assert_eq!(schema.required_roles[0].name, "valid");
        assert_eq!(schema.required_roles[1].name, "ready");
        assert_eq!(schema.optional_roles.len(), 2);
    }

    #[test]
    fn test_suggest_bindings_returns_candidates() {
        let analyzer = ValidReadyAnalyzer::new();
        let backend = make_backend(
            SignalData { changes: vec![] },
            SignalData { changes: vec![] },
            vec![],
        );
        let index = crate::index::SignalIndex::build(&backend);

        let suggestions = analyzer.suggest_bindings(&index);
        // Should have suggestions for valid and ready since their short names
        // match the signal paths (top.valid, top.ready)
        assert_eq!(suggestions.len(), 2);
        assert!(suggestions.iter().any(|s| s.role == "valid"));
        assert!(suggestions.iter().any(|s| s.role == "ready"));
    }

    #[test]
    fn test_analysis_report_serialization() {
        let report = AnalysisReport {
            protocol: "valid_ready".into(),
            summary: AnalysisSummary {
                total_handshakes: 1,
                total_violations: 0,
                total_stalls: 0,
                pass: true,
            },
            handshakes: vec![HandshakeInfo {
                time: 10,
                phase: "initiated".into(),
                signal_a: "top.valid".into(),
                signal_b: "top.ready".into(),
            }],
            violations: vec![],
            stalls: vec![],
        };

        let json = serde_json::to_string_pretty(&report).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["protocol"], "valid_ready");
        assert_eq!(parsed["summary"]["pass"], true);
        assert_eq!(parsed["summary"]["total_handshakes"], 1);
        assert_eq!(parsed["handshakes"].as_array().unwrap().len(), 1);
    }

    // ── Suggestion helpers ───────────────────────────────────

    #[test]
    fn test_suggest_candidates_exact_match_high_confidence() {
        let analyzer = ValidReadyAnalyzer::new();
        let backend = make_backend(
            SignalData { changes: vec![] },
            SignalData { changes: vec![] },
            vec![],
        );
        let index = crate::index::SignalIndex::build(&backend);

        let candidates = analyzer.suggest_candidates_for_role("valid", &index);
        assert!(!candidates.is_empty());
        let top = &candidates[0];
        assert_eq!(top.signal_path, "top.valid");
        assert!(
            top.confidence >= 0.9,
            "exact match should have high confidence"
        );
    }
}

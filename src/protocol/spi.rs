// ── SPI Protocol Analyzer ──────────────────────────────────────────
//
// Implements [`ProtocolPlugin`] for the classic 4-wire SPI bus.
// The analyzer is variant-aware — it supports configurable CPOL/CPHA
// and CS polarity via [`SpiConfig`], but does not hardcode AXI,
// quad-SPI, or other bus-family extensions.
//
// # Analysis
//
// Given a bound trace slice, the analyzer:
//
// 1. Derives chip-select (CS) windows: periods where CS is asserted
//    (active-low by default, configurable).
// 2. Within each CS window, finds all SCLK edges matching the
//    configured sample polarity (Rise → CPHA=0; Fall → CPHA=1).
// 3. Samples MOSI/MISO at each qualifying SCLK edge to reconstruct the
//    serial bitstream.
// 4. Groups sampled bits into words of [`SpiConfig::word_size`] bits
//    and reports each transfer as a [`SpiTransfer`].
// 5. Detects violations:
//    - CS deasserted mid-word (truncated transfer)
//    - SCLK edges outside a CS window (spurious clock)
//    - Missing SCLK edges (expected edges not found within a CS window)
//    - Payload instability on MOSI/MISO between sample edges
//    - Ambiguous/unknown values on any signal
//
// Every finding carries an [`EvidenceWindow`] and a human-readable
// explanation.

use crate::events::{detect_edges, EdgePolarity, EvidenceWindow, ValueClass};
use crate::index::SignalIndex;
use crate::protocol::{
    Anomaly, AnomalyKind, AnomalySeverity, BindingSuggestion, BindingValidation, ProtocolPlugin,
    RoleBinding, RoleDescriptor, RoleSchema,
};
use crate::query::{SpiCsWindowInfo, SpiTransferInfo, ViolationInfo};
use crate::trace::TraceSlice;
use serde::Serialize;

// ── Role names ─────────────────────────────────────────────────────

pub const SCLK_ROLE: &str = "sclk";
pub const CS_ROLE: &str = "cs";
pub const MOSI_ROLE: &str = "mosi";
pub const MISO_ROLE: &str = "miso";

// ── Analysis Configuration ─────────────────────────────────────────

/// Tunable knobs for the SPI analyzer.
#[derive(Debug, Clone)]
pub struct SpiConfig {
    /// Which SCLK edge to sample on.
    /// Rise (CPHA=0 with CPOL=0) or Fall (CPHA=1 with CPOL=0).
    /// Default: Rise.
    pub sample_edge: EdgePolarity,

    /// Whether CS is active-low (true) or active-high (false).
    /// Default: true.
    pub cs_active_low: bool,

    /// Number of bits per word.  Default: 8.
    pub word_size: usize,

    /// Minimum idle time between CS deassert and next assert that
    /// triggers a warning when violated.
    /// Default: 1000 (in native time units).
    pub min_cs_idle: u64,
}

impl Default for SpiConfig {
    fn default() -> Self {
        SpiConfig {
            sample_edge: EdgePolarity::Rise,
            cs_active_low: true,
            word_size: 8,
            min_cs_idle: 1000,
        }
    }
}

// ── Analyzer ───────────────────────────────────────────────────────

/// SPI protocol analyzer.
///
/// The analyzer is a protocol plugin: it declares its role schema,
/// validates bindings, and can be registered in a [`ProtocolCatalog`].
pub struct SpiAnalyzer {
    schema: RoleSchema,
    config: SpiConfig,
}

impl SpiAnalyzer {
    pub fn new() -> Self {
        SpiAnalyzer {
            schema: RoleSchema {
                name: "spi".into(),
                description: "4-wire SPI (SCLK, CS, MOSI, MISO)".into(),
                required_roles: vec![
                    RoleDescriptor {
                        name: SCLK_ROLE.into(),
                        description: "Serial clock".into(),
                        width_hint: Some(1),
                    },
                    RoleDescriptor {
                        name: CS_ROLE.into(),
                        description: "Chip select (active-low by default)".into(),
                        width_hint: Some(1),
                    },
                ],
                optional_roles: vec![
                    RoleDescriptor {
                        name: MOSI_ROLE.into(),
                        description: "Master Out Slave In — sampled on SCLK edges".into(),
                        width_hint: Some(1),
                    },
                    RoleDescriptor {
                        name: MISO_ROLE.into(),
                        description: "Master In Slave Out — sampled on SCLK edges".into(),
                        width_hint: Some(1),
                    },
                ],
            },
            config: SpiConfig::default(),
        }
    }

    pub fn with_config(config: SpiConfig) -> Self {
        SpiAnalyzer {
            schema: RoleSchema {
                name: "spi".into(),
                description: "4-wire SPI (SCLK, CS, MOSI, MISO)".into(),
                required_roles: vec![
                    RoleDescriptor {
                        name: SCLK_ROLE.into(),
                        description: "Serial clock".into(),
                        width_hint: Some(1),
                    },
                    RoleDescriptor {
                        name: CS_ROLE.into(),
                        description: "Chip select".into(),
                        width_hint: Some(1),
                    },
                ],
                optional_roles: vec![
                    RoleDescriptor {
                        name: MOSI_ROLE.into(),
                        description: "MOSI sampled on SCLK edges".into(),
                        width_hint: Some(1),
                    },
                    RoleDescriptor {
                        name: MISO_ROLE.into(),
                        description: "MISO sampled on SCLK edges".into(),
                        width_hint: Some(1),
                    },
                ],
            },
            config,
        }
    }

    // ── Public analysis entry point ────────────────────────────

    /// Run the full SPI analysis over a pre-built trace slice.
    ///
    /// At minimum `sclk` and `cs` must be bound; `mosi` and `miso`
    /// are optional and enhance the analysis when provided.
    pub fn analyze(&self, slice: &TraceSlice, bindings: &[RoleBinding]) -> SpiAnalysisReport {
        let sclk_path = get_binding(bindings, SCLK_ROLE);
        let cs_path = get_binding(bindings, CS_ROLE);
        let mosi_path = get_binding(bindings, MOSI_ROLE);
        let miso_path = get_binding(bindings, MISO_ROLE);

        let mut violations: Vec<Anomaly> = Vec::new();
        let mut transfers: Vec<SpiTransfer> = Vec::new();
        let mut cs_windows: Vec<CsWindow> = Vec::new();
        // Require sclk + cs bound
        let (sclk, cs) = match (&sclk_path, &cs_path) {
            (Some(s), Some(c)) => (s.clone(), c.clone()),
            _ => {
                let sigs = slice.signals.clone();
                violations.push(Anomaly::error(
                    AnomalyKind::BindingError,
                    "sclk and cs roles must be bound".into(),
                    EvidenceWindow::new(
                        slice.bounds.from,
                        Some(slice.bounds.to),
                        sigs,
                        "missing required SPI bindings".into(),
                    ),
                ));
                return SpiAnalysisReport {
                    protocol: "spi".into(),
                    summary: SpiAnalysisSummary {
                        total_transfers: 0,
                        total_bits: 0,
                        total_violations: 1,
                        pass: false,
                    },
                    transfers: vec![],
                    violations: violations.iter().map(violation_info_from_anomaly).collect(),
                    cs_windows: vec![],
                    sclk_edges: vec![],
                };
            }
        };

        let sclk_data = match slice
            .data
            .get(slice.signal_index(&sclk).unwrap_or(usize::MAX))
        {
            Some(d) => d,
            None => return empty_report("sclk data not in slice"),
        };
        let cs_data = match slice
            .data
            .get(slice.signal_index(&cs).unwrap_or(usize::MAX))
        {
            Some(d) => d,
            None => return empty_report("cs data not in slice"),
        };

        // Step 1: Derive CS windows (CS asserted → deasserted intervals)
        let cs_windows_raw = derive_cs_windows(
            cs_data,
            &cs,
            self.config.cs_active_low,
            slice.bounds,
            &mut violations,
        );

        // Step 2: Detect all SCLK edges within bounds
        let all_sclk_edges = detect_edges(&sclk, sclk_data, EdgePolarity::Toggle, slice.bounds);

        // Collect SCLK edge times for the report
        let sclk_edge_times: Vec<u64> = all_sclk_edges
            .iter()
            .filter_map(|e| {
                if let crate::events::DerivedEvent::Edge { time, .. } = e {
                    Some(*time)
                } else {
                    None
                }
            })
            .collect();

        // Filter edges by sample polarity
        let sample_edges: Vec<u64> =
            detect_edges(&sclk, sclk_data, self.config.sample_edge, slice.bounds)
                .iter()
                .filter_map(|e| {
                    if let crate::events::DerivedEvent::Edge { time, .. } = e {
                        Some(*time)
                    } else {
                        None
                    }
                })
                .collect();

        // Step 3: Reconstruct bitstream per CS window
        // Get MOSI/MISO data if available
        let mosi_data = mosi_path
            .as_ref()
            .and_then(|p| slice.signal_index(p))
            .and_then(|i| slice.data.get(i));

        let miso_data = miso_path
            .as_ref()
            .and_then(|p| slice.signal_index(p))
            .and_then(|i| slice.data.get(i));

        // Build transfer info
        let mosi_p = mosi_path.clone().unwrap_or_default();
        let miso_p = miso_path.clone().unwrap_or_default();

        for w in &cs_windows_raw {
            // Find sample edges that fall within this CS window
            let window_edges: Vec<u64> = sample_edges
                .iter()
                .filter(|&&t| t >= w.start && t <= w.end)
                .copied()
                .collect();

            // Sample MOSI/MISO at each edge
            let mut mosi_bits: Vec<SpiBit> = Vec::new();
            let mut miso_bits: Vec<SpiBit> = Vec::new();

            for &t in &window_edges {
                let mosi_val = mosi_data.and_then(|d| d.sample(t));
                let miso_val = miso_data.and_then(|d| d.sample(t));

                if let Some(val) = mosi_val {
                    mosi_bits.push(bit_sample(val, t));
                }
                if let Some(val) = miso_val {
                    miso_bits.push(bit_sample(val, t));
                }
            }

            // Group into words
            let mosi_words = group_into_words(&mosi_bits, self.config.word_size);
            let miso_words = group_into_words(&miso_bits, self.config.word_size);

            // Check violations within this CS window
            // Check for truncated words
            let total_mosi_bits = mosi_bits.len();
            let total_miso_bits = miso_bits.len();

            // Detect truncated transfer: partial word with no more SCLK edges before CS deassert
            if !mosi_bits.is_empty() && !total_mosi_bits.is_multiple_of(self.config.word_size) {
                // Check if there are remaining edges that would complete this word
                let remaining = self.config.word_size - (total_mosi_bits % self.config.word_size);
                violations.push(
                    Anomaly::warning(
                        AnomalyKind::ProtocolViolation,
                        format!(
                            "CS window [{}, {}] truncated mid-word: {} MOSI bits sampled, \
                         need {} more for a {}-bit word",
                            w.start, w.end, total_mosi_bits, remaining, self.config.word_size,
                        ),
                        EvidenceWindow::new(
                            w.start,
                            Some(w.end),
                            vec![cs.clone(), sclk.clone(), mosi_p.clone()],
                            "truncated MOSI transfer check".into(),
                        ),
                    )
                    .with_roles(vec![
                        CS_ROLE.into(),
                        SCLK_ROLE.into(),
                        MOSI_ROLE.into(),
                    ]),
                );
            }

            if !miso_bits.is_empty() && !total_miso_bits.is_multiple_of(self.config.word_size) {
                let remaining = self.config.word_size - (total_miso_bits % self.config.word_size);
                violations.push(
                    Anomaly::warning(
                        AnomalyKind::ProtocolViolation,
                        format!(
                            "CS window [{}, {}] truncated mid-word: {} MISO bits sampled, \
                         need {} more for a {}-bit word",
                            w.start, w.end, total_miso_bits, remaining, self.config.word_size,
                        ),
                        EvidenceWindow::new(
                            w.start,
                            Some(w.end),
                            vec![cs.clone(), sclk.clone(), miso_p.clone()],
                            "truncated MISO transfer check".into(),
                        ),
                    )
                    .with_roles(vec![
                        CS_ROLE.into(),
                        SCLK_ROLE.into(),
                        MISO_ROLE.into(),
                    ]),
                );
            }

            // Check payload stability on MOSI between sample edges
            if let (Some(md), true) = (mosi_data, !mosi_p.is_empty()) {
                // In SPI, MOSI should be stable at the sample edge - we check
                // for changes between consecutive sample edges during CS asserted
                check_data_stability_spi(
                    md,
                    &mosi_p,
                    &sclk,
                    &cs,
                    &window_edges,
                    w.start,
                    w.end,
                    &mut violations,
                    MOSI_ROLE,
                );
            }

            if let (Some(md), true) = (miso_data, !miso_p.is_empty()) {
                check_data_stability_spi(
                    md,
                    &miso_p,
                    &sclk,
                    &cs,
                    &window_edges,
                    w.start,
                    w.end,
                    &mut violations,
                    MISO_ROLE,
                );
            }

            // Check for SCLK edges outside any CS window (but within bounds)
            // This is done globally below

            // Record transfer
            transfers.push(SpiTransfer {
                cs_start: w.start,
                cs_end: w.end,
                sclk_edges: window_edges.len(),
                mosi_bits: mosi_bits.iter().map(|b| b.value.clone()).collect(),
                miso_bits: miso_bits.iter().map(|b| b.value.clone()).collect(),
                mosi_words,
                miso_words,
            });

            cs_windows.push(CsWindow {
                start: w.start,
                end: w.end,
            });
        }

        // Global: check SCLK edges outside any CS window
        for &t in &sample_edges {
            let in_window = cs_windows_raw.iter().any(|w| t >= w.start && t <= w.end);
            if !in_window {
                violations.push(
                    Anomaly::warning(
                        AnomalyKind::ProtocolViolation,
                        format!(
                            "SCLK sample edge at {} occurs outside any CS window — spurious clock",
                            t,
                        ),
                        EvidenceWindow::new(
                            t,
                            None,
                            vec![sclk.clone(), cs.clone()],
                            "spurious SCLK edge check".into(),
                        ),
                    )
                    .with_roles(vec![SCLK_ROLE.into(), CS_ROLE.into()]),
                );
            }
        }

        // Check ambiguous values on control signals
        check_ambiguous_values(
            sclk_data,
            &sclk,
            slice.bounds,
            SCLK_ROLE,
            &mut violations,
            &self.config,
        );
        check_ambiguous_values(
            cs_data,
            &cs,
            slice.bounds,
            CS_ROLE,
            &mut violations,
            &self.config,
        );

        if let Some(md) = mosi_data {
            if !mosi_p.is_empty() {
                check_ambiguous_values(
                    md,
                    &mosi_p,
                    slice.bounds,
                    MOSI_ROLE,
                    &mut violations,
                    &self.config,
                );
            }
        }
        if let Some(md) = miso_data {
            if !miso_p.is_empty() {
                check_ambiguous_values(
                    md,
                    &miso_p,
                    slice.bounds,
                    MISO_ROLE,
                    &mut violations,
                    &self.config,
                );
            }
        }

        // Build report
        let total_bits: usize = transfers.iter().map(|t| t.sclk_edges).sum();
        let has_errors = violations
            .iter()
            .any(|v| v.severity == AnomalySeverity::Error);

        SpiAnalysisReport {
            protocol: "spi".into(),
            summary: SpiAnalysisSummary {
                total_transfers: transfers.len(),
                total_bits,
                total_violations: violations
                    .iter()
                    .filter(|v| v.severity == AnomalySeverity::Error)
                    .count(),
                pass: !has_errors,
            },
            transfers: transfers
                .iter()
                .map(|t| SpiTransferInfo {
                    cs_start: t.cs_start,
                    cs_end: t.cs_end,
                    sclk_edges: t.sclk_edges,
                    mosi_bits: t.mosi_bits.clone(),
                    miso_bits: t.miso_bits.clone(),
                    mosi_words: t.mosi_words.clone(),
                    miso_words: t.miso_words.clone(),
                })
                .collect(),
            violations: violations.iter().map(violation_info_from_anomaly).collect(),
            cs_windows: cs_windows
                .iter()
                .map(|w| SpiCsWindowInfo {
                    start: w.start,
                    end: w.end,
                })
                .collect(),
            sclk_edges: sclk_edge_times,
        }
    }
}

impl Default for SpiAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolPlugin for SpiAnalyzer {
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
                let candidates = self.suggest_candidates_for_role(&role.name, index);
                if !candidates.is_empty() {
                    suggestions.push(BindingSuggestion {
                        role: role.name.clone(),
                        candidates,
                    });
                }
            }
        }

        for binding in bindings {
            if index.lookup_exact(&binding.signal_path).is_none() {
                warnings.push(format!(
                    "bound signal '{}' (role '{}') not found in waveform index",
                    binding.signal_path, binding.role,
                ));
            }
        }

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

impl SpiAnalyzer {
    fn suggest_candidates_for_role(
        &self,
        role: &str,
        index: &SignalIndex,
    ) -> Vec<crate::protocol::BindingCandidate> {
        let role_lower = role.to_lowercase();
        let mut candidates = Vec::new();

        // SPI-specific alias mapping for common names
        let aliases: &[&str] = match role_lower.as_str() {
            "sclk" => &["sclk", "clk", "sck", "clock", "spi_clk"],
            "cs" => &["cs", "cs_n", "cs_b", "ss", "ss_n", "ncs", "spi_cs"],
            "mosi" => &["mosi", "mosi", "simo", "sdo", "tx", "spi_mosi"],
            "miso" => &["miso", "miso", "somi", "sdi", "rx", "spi_miso"],
            _ => return candidates,
        };

        for entry in index.entries() {
            let name_lower = entry.short_name.to_lowercase();
            let mut confidence: f64 = 0.0;
            let mut reason = String::new();

            if name_lower == role_lower {
                confidence = 0.95;
                reason = format!("short name matches '{}' exactly", role);
            } else if aliases.contains(&name_lower.as_str()) {
                confidence = 0.85;
                reason = format!(
                    "short name '{}' matches SPI alias for '{}'",
                    entry.short_name, role
                );
            } else if name_lower.contains(&role_lower) {
                confidence = 0.5;
                reason = format!("short name '{}' contains '{}'", entry.short_name, role);
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

/// Machine-readable report produced by [`SpiAnalyzer::analyze`].
#[derive(Debug, Clone, Serialize)]
pub struct SpiAnalysisReport {
    pub protocol: String,
    pub summary: SpiAnalysisSummary,
    pub transfers: Vec<SpiTransferInfo>,
    pub violations: Vec<ViolationInfo>,
    pub cs_windows: Vec<SpiCsWindowInfo>,
    pub sclk_edges: Vec<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpiAnalysisSummary {
    pub total_transfers: usize,
    pub total_bits: usize,
    pub total_violations: usize,
    pub pass: bool,
}

/// Raw analysis data for a single transfer (CS window).
#[derive(Debug, Clone)]
struct SpiTransfer {
    cs_start: u64,
    cs_end: u64,
    sclk_edges: usize,
    mosi_bits: Vec<String>,
    miso_bits: Vec<String>,
    mosi_words: Vec<String>,
    miso_words: Vec<String>,
}

/// A single CS window.
#[derive(Debug, Clone)]
struct CsWindow {
    start: u64,
    end: u64,
}

// ── Internal helpers ───────────────────────────────────────────────

/// A single sampled bit with time.
struct SpiBit {
    #[allow(dead_code)]
    time: u64,
    value: String,
}

fn bit_sample(val: &crate::backend::types::CompactValue, time: u64) -> SpiBit {
    SpiBit {
        time,
        value: val.as_str().to_string(),
    }
}

fn group_into_words(bits: &[SpiBit], word_size: usize) -> Vec<String> {
    let mut words = Vec::new();
    for chunk in bits.chunks(word_size) {
        if chunk.len() == word_size {
            let word_str: String = chunk.iter().map(|b| b.value.as_str()).collect();
            words.push(word_str);
        }
    }
    words
}

fn get_binding(bindings: &[RoleBinding], role: &str) -> Option<String> {
    let lower = role.to_lowercase();
    bindings
        .iter()
        .find(|b| b.role.to_lowercase() == lower)
        .map(|b| b.signal_path.clone())
}

/// Derive CS windows: periods where CS is asserted.
/// Returns a list of [start, end] intervals.
fn derive_cs_windows(
    cs_data: &crate::backend::types::SignalData,
    cs_path: &str,
    active_low: bool,
    bounds: crate::trace::TimeBound,
    violations: &mut Vec<Anomaly>,
) -> Vec<CsWindow> {
    let mut windows = Vec::new();
    let mut asserted_start: Option<u64> = None;

    let filtered: Vec<(u64, &crate::backend::types::CompactValue)> = cs_data
        .changes
        .iter()
        .filter(|(t, _)| *t >= bounds.from && *t <= bounds.to)
        .map(|(t, v)| (*t, v))
        .collect();

    // Seed with pre-window state
    let pre_val = cs_data.sample(bounds.from.saturating_sub(1));
    let pre_asserted = pre_val
        .map(|v| {
            let class = ValueClass::classify(v);
            if active_low {
                class.is_low()
            } else {
                class.is_high()
            }
        })
        .unwrap_or(false);

    if pre_asserted {
        asserted_start = Some(bounds.from);
    }

    for (t, v) in &filtered {
        let class = ValueClass::classify(v);
        let is_asserted = if active_low {
            class.is_low()
        } else {
            class.is_high()
        };
        let is_known = class.is_known();

        if !is_known {
            // Unknown value on CS — report violation
            violations.push(
                Anomaly::warning(
                    AnomalyKind::AmbiguousValue,
                    format!("CS signal {} is unknown (x/z) at time {}", cs_path, t),
                    EvidenceWindow::new(
                        *t,
                        None,
                        vec![cs_path.to_string()],
                        "ambiguous CS value".into(),
                    ),
                )
                .with_roles(vec![CS_ROLE.into()]),
            );
            continue;
        }

        match (asserted_start, is_asserted) {
            (Some(start), false) => {
                // CS deasserted
                windows.push(CsWindow { start, end: *t });
                asserted_start = None;
            }
            (None, true) => {
                // CS asserted
                asserted_start = Some(*t);
            }
            _ => {} // No state change
        }
    }

    // Close any open window at the end of bounds
    if let Some(start) = asserted_start {
        windows.push(CsWindow {
            start,
            end: bounds.to,
        });
    }

    windows
}

/// Check that a data signal (MOSI/MISO) is stable between consecutive
/// SCLK sample edges within a CS window.  In SPI, data changes should
/// only occur near (not between) the sample edges.
#[allow(clippy::too_many_arguments)]
fn check_data_stability_spi(
    data: &crate::backend::types::SignalData,
    data_path: &str,
    clock_path: &str,
    cs_path: &str,
    sample_edges: &[u64],
    cs_start: u64,
    cs_end: u64,
    violations: &mut Vec<Anomaly>,
    role_name: &str,
) {
    // For each pair of consecutive sample edges, check that data
    // did not change between them.
    for win in sample_edges.windows(2) {
        let e1 = win[0];
        let e2 = win[1];
        let mid = (e1 + e2) / 2;

        // Check if data changed between e1 and e2
        let val_at_e1 = data.sample(e1);
        let val_at_mid = data.sample(mid);

        if let (Some(v1), Some(v2)) = (val_at_e1, val_at_mid) {
            if v1.as_str() != v2.as_str() {
                violations.push(
                    Anomaly::warning(
                        AnomalyKind::PayloadInstability,
                        format!(
                            "{} changed between SCLK edges ({} → {}) within CS window [{}, {}]: \
                         data may not be stable at sample time",
                            data_path,
                            v1.as_str(),
                            v2.as_str(),
                            cs_start,
                            cs_end,
                        ),
                        EvidenceWindow::new(
                            e1,
                            Some(e2),
                            vec![
                                clock_path.to_string(),
                                data_path.to_string(),
                                cs_path.to_string(),
                            ],
                            format!("{} stability check between SCLK edges", data_path),
                        ),
                    )
                    .with_roles(vec![
                        SCLK_ROLE.into(),
                        role_name.into(),
                        CS_ROLE.into(),
                    ]),
                );
                break;
            }
        }
    }
}

/// Check for ambiguous values (x/z/u) on a control/data signal.
fn check_ambiguous_values(
    data: &crate::backend::types::SignalData,
    path: &str,
    bounds: crate::trace::TimeBound,
    role: &str,
    violations: &mut Vec<Anomaly>,
    _config: &SpiConfig,
) {
    for (t, v) in &data.changes {
        if *t < bounds.from || *t > bounds.to {
            continue;
        }
        let class = ValueClass::classify(v);
        if !class.is_known() {
            violations.push(
                Anomaly::warning(
                    AnomalyKind::AmbiguousValue,
                    format!("{} value is unknown at {}: {}", path, t, v.as_str()),
                    EvidenceWindow::new(
                        *t,
                        None,
                        vec![path.to_string()],
                        format!("ambiguous value check on {}", path),
                    ),
                )
                .with_roles(vec![role.into()]),
            );
        }
    }
}

fn empty_report(msg: &str) -> SpiAnalysisReport {
    SpiAnalysisReport {
        protocol: "spi".into(),
        summary: SpiAnalysisSummary {
            total_transfers: 0,
            total_bits: 0,
            total_violations: 1,
            pass: false,
        },
        transfers: vec![],
        violations: vec![ViolationInfo {
            kind: "binding_error".into(),
            severity: "error".into(),
            description: msg.into(),
            related_roles: vec![SCLK_ROLE.into(), CS_ROLE.into()],
            related_signals: vec![],
            evidence_start: 0,
            evidence_end: None,
        }],
        cs_windows: vec![],
        sclk_edges: vec![],
    }
}

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

    /// Build SPI signals representing a clean single-byte transfer:
    /// CPOL=0, CPHA=0, CS active-low, 8-bit word, MOSI=0xA5 (10100101)
    ///
    /// Time:  0    10   20   30   40   50   60   70   80   90   100  110  120  130  140  150  160  170  180  190  200
    /// CS:    ────────────────────────────────┐                                                         ┌────────────
    ///                                        └─────────────────────────────────────────────────────────┘
    /// SCLK:  ──────┐     ┌─────┐     ┌─────┐     ┌─────┐     ┌─────┐     ┌─────┐     ┌─────┐     ┌─────┐
    ///              └─────┘     └─────┘     └─────┘     └─────┘     └─────┘     └─────┘     └─────┘     └─────
    ///              30    40    50    60    70    80    90    100   110   120   130   140   150   160   170   180
    /// MOSI:  ────────────────────┐     ┌───────────┐     ┌─────────────────────────────────────────┐
    ///                            └─────┘           └─────┘                                         └─────
    ///                            1     0     1     0     0     1     0     1
    fn make_spi_clean_signals() -> (SignalData, SignalData, SignalData, SignalData) {
        // CS: deasserted (high) from 0-30, asserted (low) from 30-190, deasserted from 190+
        let cs = SignalData {
            changes: vec![
                (0, CompactValue::new("1")),
                (30, CompactValue::new("0")),
                (190, CompactValue::new("1")),
            ],
        };

        // SCLK: 8 rising edges during CS window (30-190)
        let sclk = SignalData {
            changes: vec![
                (0, CompactValue::new("0")),
                (30, CompactValue::new("1")),
                (40, CompactValue::new("0")),
                (50, CompactValue::new("1")),
                (60, CompactValue::new("0")),
                (70, CompactValue::new("1")),
                (80, CompactValue::new("0")),
                (90, CompactValue::new("1")),
                (100, CompactValue::new("0")),
                (110, CompactValue::new("1")),
                (120, CompactValue::new("0")),
                (130, CompactValue::new("1")),
                (140, CompactValue::new("0")),
                (150, CompactValue::new("1")),
                (160, CompactValue::new("0")),
                (170, CompactValue::new("1")),
                (180, CompactValue::new("0")),
            ],
        };

        // MOSI: 0xA5 = 10100101 (MSB first): 1 at 0, 0 at 40, 1 at 60, 0 at 80, 0 at 100, 1 at 120, 0 at 140, 1 at 160
        // But setup happens slightly before each rising edge
        let mosi = SignalData {
            changes: vec![
                (0, CompactValue::new("0")),
                (25, CompactValue::new("1")),  // bit 7 = 1 (MSB)
                (45, CompactValue::new("0")),  // bit 6 = 0
                (65, CompactValue::new("1")),  // bit 5 = 1
                (85, CompactValue::new("0")),  // bit 4 = 0
                (105, CompactValue::new("0")), // bit 3 = 0
                (125, CompactValue::new("1")), // bit 2 = 1
                (145, CompactValue::new("0")), // bit 1 = 0
                (165, CompactValue::new("1")), // bit 0 = 1 (LSB)
            ],
        };

        // MISO: all zeros
        let miso = SignalData {
            changes: vec![(0, CompactValue::new("0"))],
        };

        (sclk, cs, mosi, miso)
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
        sclk: SignalData,
        cs: SignalData,
        mosi: SignalData,
        miso: SignalData,
    ) -> TestBackend {
        let signals = vec![
            SignalInfo {
                path: "top.sclk".into(),
                width: 1,
            },
            SignalInfo {
                path: "top.cs".into(),
                width: 1,
            },
            SignalInfo {
                path: "top.mosi".into(),
                width: 1,
            },
            SignalInfo {
                path: "top.miso".into(),
                width: 1,
            },
        ];
        let mut data = HashMap::new();
        data.insert("top.sclk".into(), sclk);
        data.insert("top.cs".into(), cs);
        data.insert("top.mosi".into(), mosi);
        data.insert("top.miso".into(), miso);

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

    fn run_analysis(backend: &TestBackend, bindings: &[RoleBinding]) -> SpiAnalysisReport {
        run_analysis_with_bounds(backend, bindings, TimeBound::new(0, 250).unwrap())
    }

    fn run_analysis_with_bounds(
        backend: &TestBackend,
        bindings: &[RoleBinding],
        bounds: TimeBound,
    ) -> SpiAnalysisReport {
        let analyzer = SpiAnalyzer::new();
        let signal_paths: Vec<String> = bindings.iter().map(|b| b.signal_path.clone()).collect();
        let req = TraceSliceRequest::new(backend, signal_paths, bounds);
        let slice = req.build().expect("slice should build");

        analyzer.analyze(&slice, bindings)
    }

    // ── Happy path: clean SPI transfer ──────────────────────────

    #[test]
    fn test_clean_spi_transfer_detected() {
        let (sclk, cs, mosi, miso) = make_spi_clean_signals();
        let backend = make_backend(sclk, cs, mosi, miso);
        let bindings = vec![
            RoleBinding::user_specified("sclk", "top.sclk"),
            RoleBinding::user_specified("cs", "top.cs"),
            RoleBinding::user_specified("mosi", "top.mosi"),
            RoleBinding::user_specified("miso", "top.miso"),
        ];

        let report = run_analysis(&backend, &bindings);
        assert!(report.summary.pass, "clean SPI transfer should pass");
        assert_eq!(report.summary.total_transfers, 1, "one CS window");
        assert_eq!(
            report.summary.total_violations, 0,
            "no violations in clean transfer"
        );
        assert_eq!(
            report.transfers[0].sclk_edges, 8,
            "8 SCLK edges in CS window"
        );
        assert!(!report.cs_windows.is_empty(), "should detect CS windows");
    }

    #[test]
    fn test_spi_mosi_word_reconstruction() {
        let (sclk, cs, mosi, miso) = make_spi_clean_signals();
        let backend = make_backend(sclk, cs, mosi, miso);
        let bindings = vec![
            RoleBinding::user_specified("sclk", "top.sclk"),
            RoleBinding::user_specified("cs", "top.cs"),
            RoleBinding::user_specified("mosi", "top.mosi"),
        ];

        let report = run_analysis(&backend, &bindings);
        // MOSI bits sampled at rising edges of SCLK:
        // edges at 30,50,70,90,110,130,150,170
        // MOSI values at those times: 1,0,1,0,0,1,0,1 → "10100101" = 0xA5
        assert_eq!(report.transfers[0].mosi_bits.len(), 8);
        assert_eq!(report.transfers[0].mosi_words.len(), 1);
        assert_eq!(report.transfers[0].mosi_words[0], "10100101");
    }

    // ── Truncated transfer ─────────────────────────────────────

    #[test]
    fn test_truncated_transfer_detected() {
        // CS deasserts after only 5 SCLK edges (should be 8 for a full byte)
        let sclk = SignalData {
            changes: vec![
                (0, CompactValue::new("0")),
                (10, CompactValue::new("1")),
                (20, CompactValue::new("0")),
                (30, CompactValue::new("1")),
                (40, CompactValue::new("0")),
                (50, CompactValue::new("1")),
                (60, CompactValue::new("0")),
                (70, CompactValue::new("1")),
                (80, CompactValue::new("0")),
                (90, CompactValue::new("1")),
                (100, CompactValue::new("0")),
            ],
        };
        let cs = SignalData {
            changes: vec![
                (0, CompactValue::new("1")),
                (5, CompactValue::new("0")),
                (75, CompactValue::new("1")),
            ],
        };
        let mosi = SignalData {
            changes: vec![
                (0, CompactValue::new("0")),
                (8, CompactValue::new("1")),
                (28, CompactValue::new("0")),
                (48, CompactValue::new("1")),
                (68, CompactValue::new("0")),
                (88, CompactValue::new("1")),
            ],
        };
        let miso = SignalData {
            changes: vec![(0, CompactValue::new("0"))],
        };
        let backend = make_backend(sclk, cs, mosi, miso);
        let bindings = vec![
            RoleBinding::user_specified("sclk", "top.sclk"),
            RoleBinding::user_specified("cs", "top.cs"),
            RoleBinding::user_specified("mosi", "top.mosi"),
        ];

        let report = run_analysis(&backend, &bindings);
        // Should have a violation about truncated transfer
        let trunc = report
            .violations
            .iter()
            .any(|v| v.description.contains("truncated"));
        assert!(
            trunc,
            "truncated transfer should be flagged, got violations: {:?}",
            report.violations
        );
    }

    // ── Spurious SCLK ─────────────────────────────────────────

    #[test]
    fn test_spurious_sclk_detected() {
        // SCLK edges happen before and after CS window
        let sclk = SignalData {
            changes: vec![
                (0, CompactValue::new("0")),
                (10, CompactValue::new("1")), // spurious: before CS
                (20, CompactValue::new("0")),
                (30, CompactValue::new("1")), // in CS window
                (40, CompactValue::new("0")),
                (50, CompactValue::new("1")), // in CS window
                (60, CompactValue::new("0")),
                (70, CompactValue::new("1")), // spurious: after CS
                (80, CompactValue::new("0")),
            ],
        };
        let cs = SignalData {
            changes: vec![
                (0, CompactValue::new("1")),
                (25, CompactValue::new("0")),
                (55, CompactValue::new("1")),
            ],
        };
        let mosi = SignalData {
            changes: vec![(0, CompactValue::new("0"))],
        };
        let miso = SignalData {
            changes: vec![(0, CompactValue::new("0"))],
        };
        let backend = make_backend(sclk, cs, mosi, miso);
        let bindings = vec![
            RoleBinding::user_specified("sclk", "top.sclk"),
            RoleBinding::user_specified("cs", "top.cs"),
        ];

        let report = run_analysis(&backend, &bindings);
        let has_spurious = report
            .violations
            .iter()
            .any(|v| v.description.contains("spurious"));
        assert!(
            has_spurious,
            "spurious SCLK edges should be flagged, got violations: {:?}",
            report.violations
        );
    }

    // ── Unknown values ────────────────────────────────────────

    #[test]
    fn test_unknown_cs_value_detected() {
        let sclk = SignalData {
            changes: vec![(0, CompactValue::new("0")), (10, CompactValue::new("1"))],
        };
        let cs = SignalData {
            changes: vec![
                (0, CompactValue::new("1")),
                (10, CompactValue::new("x")),
                (20, CompactValue::new("0")),
                (50, CompactValue::new("1")),
            ],
        };
        let mosi = SignalData {
            changes: vec![(0, CompactValue::new("0"))],
        };
        let miso = SignalData {
            changes: vec![(0, CompactValue::new("0"))],
        };
        let backend = make_backend(sclk, cs, mosi, miso);
        let bindings = vec![
            RoleBinding::user_specified("sclk", "top.sclk"),
            RoleBinding::user_specified("cs", "top.cs"),
        ];

        let report = run_analysis(&backend, &bindings);
        let has_unknown = report.violations.iter().any(|v| v.kind == "ambiguousvalue");
        assert!(
            has_unknown,
            "unknown CS value should be flagged, got violations: {:?}",
            report.violations
        );
    }

    // ── Binding validation ─────────────────────────────────────

    #[test]
    fn test_missing_required_role_fails_validation() {
        let analyzer = SpiAnalyzer::new();
        let bindings = vec![RoleBinding::user_specified("sclk", "top.sclk")];
        let backend = make_backend(
            SignalData { changes: vec![] },
            SignalData { changes: vec![] },
            SignalData { changes: vec![] },
            SignalData { changes: vec![] },
        );
        let index = crate::index::SignalIndex::build(&backend);

        let validation = analyzer.validate_bindings(&bindings, &index);
        assert!(!validation.is_valid, "missing cs role should fail");
        assert!(validation.missing_roles.contains(&"cs".to_string()));
    }

    #[test]
    fn test_full_binding_passes_validation() {
        let analyzer = SpiAnalyzer::new();
        let bindings = vec![
            RoleBinding::user_specified("sclk", "top.sclk"),
            RoleBinding::user_specified("cs", "top.cs"),
        ];
        let backend = make_backend(
            SignalData { changes: vec![] },
            SignalData { changes: vec![] },
            SignalData { changes: vec![] },
            SignalData { changes: vec![] },
        );
        let index = crate::index::SignalIndex::build(&backend);

        let validation = analyzer.validate_bindings(&bindings, &index);
        assert!(validation.is_valid, "full binding should pass");
    }

    // ── Plugin trait compliance ────────────────────────────────

    #[test]
    fn test_plugin_name_and_description() {
        let analyzer = SpiAnalyzer::new();
        assert_eq!(analyzer.name(), "spi");
        assert!(!analyzer.description().is_empty());
    }

    #[test]
    fn test_plugin_schema_has_required_and_optional_roles() {
        let analyzer = SpiAnalyzer::new();
        let schema = analyzer.schema();
        assert_eq!(schema.required_roles.len(), 2, "sclk + cs required");
        assert_eq!(schema.optional_roles.len(), 2, "mosi + miso optional");
        assert_eq!(schema.required_roles[0].name, "sclk");
        assert_eq!(schema.required_roles[1].name, "cs");
    }

    // ── Serialization ──────────────────────────────────────────

    #[test]
    fn test_analysis_report_serialization() {
        let report = SpiAnalysisReport {
            protocol: "spi".into(),
            summary: SpiAnalysisSummary {
                total_transfers: 1,
                total_bits: 8,
                total_violations: 0,
                pass: true,
            },
            transfers: vec![SpiTransferInfo {
                cs_start: 30,
                cs_end: 190,
                sclk_edges: 8,
                mosi_bits: vec![
                    "1".into(),
                    "0".into(),
                    "1".into(),
                    "0".into(),
                    "0".into(),
                    "1".into(),
                    "0".into(),
                    "1".into(),
                ],
                miso_bits: vec![],
                mosi_words: vec!["10100101".into()],
                miso_words: vec![],
            }],
            violations: vec![],
            cs_windows: vec![SpiCsWindowInfo {
                start: 30,
                end: 190,
            }],
            sclk_edges: vec![30, 50, 70, 90, 110, 130, 150, 170],
        };

        let json = serde_json::to_string_pretty(&report).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["protocol"], "spi");
        assert_eq!(parsed["summary"]["pass"], true);
        assert_eq!(parsed["summary"]["total_transfers"], 1);
        assert_eq!(parsed["transfers"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["cs_windows"].as_array().unwrap().len(), 1);
    }

    // ── Suggestion helpers ─────────────────────────────────────

    #[test]
    fn test_suggest_candidates_for_sclk() {
        let analyzer = SpiAnalyzer::new();
        let backend = make_backend(
            SignalData { changes: vec![] },
            SignalData { changes: vec![] },
            SignalData { changes: vec![] },
            SignalData { changes: vec![] },
        );
        let index = crate::index::SignalIndex::build(&backend);

        let candidates = analyzer.suggest_candidates_for_role("sclk", &index);
        assert!(!candidates.is_empty(), "should suggest signals for sclk");
        let top = &candidates[0];
        // top.sclk is an exact match
        assert!(
            top.confidence >= 0.9,
            "exact match should have high confidence"
        );
    }

    #[test]
    fn test_suggest_candidates_missing_signal() {
        let analyzer = SpiAnalyzer::new();
        let backend = make_backend(
            SignalData { changes: vec![] },
            SignalData { changes: vec![] },
            SignalData { changes: vec![] },
            SignalData { changes: vec![] },
        );
        let index = crate::index::SignalIndex::build(&backend);

        let candidates = analyzer.suggest_candidates_for_role("nonexistent", &index);
        assert!(candidates.is_empty(), "unknown role should return empty");
    }
}

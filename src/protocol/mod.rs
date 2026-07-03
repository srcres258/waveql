// ── Protocol Plugins, Role Schemas, and Binding Models ─────────────
//
// This module defines the contract between WaveQL's analysis engine
// and protocol-aware analyzers (valid/ready, SPI, etc.).  No bus-family
// names or signal conventions live here — the contract is deliberately
// generic so that any waveform protocol can be described.
//
// # Architecture
//
// ## Role Schema
//
// A [`RoleSchema`] declares the logical roles a protocol expects
// (e.g., "valid", "ready", "sclk", "mosi").  Each role carries a
// human-readable description and an optional width hint that helps
// downstream suggestion engines narrow the search space.
//
// ## Binding
//
// [`RoleBinding`] maps a logical role to a concrete waveform signal
// path.  Bindings come in three flavours (see [`BindingKind`]):
//
// * `UserSpecified` — the user explicitly provided a path.
// * `PatternMatched` — resolved through wildcard or alias matching.
// * `Suggested(confidence)` — the engine proposed it, but it was never
//   explicitly confirmed.
//
// Suggestion objects are advisory only; deterministic validation
// (via [`BindingValidation`]) decides pass/fail.
//
// ## Protocol Plugin Trait
//
// [`ProtocolPlugin`] is the interface that every concrete analyzer
// (valid/ready, SPI, etc.) implements.  It covers:
//
// * schema declaration
// * binding validation against a [`SignalIndex`]
// * binding suggestion against a [`SignalIndex`]
//
// ## Catalog
//
// [`ProtocolCatalog`] provides runtime discovery.  The CLI's
// `protocols` subcommand lists available schemas; the engine uses the
// catalog to look up a schema by name when validating bindings.
//
// ## Anomaly
//
// [`Anomaly`] is the output vocabulary for protocol violations and
// observations.  Every anomaly carries an [`EvidenceWindow`] from the
// derived-event layer so that reporters can cite the raw data that
// produced a conclusion.
//
// # Design principles (inherited from Task 6)
//
// 1. **Protocol-neutral** — no signal names, bus families, or
//    protocol-specific assumptions live in this module.
// 2. **Deterministic** — validation is pure and repeatable; LLMs
//    consume anomaly output but never decide pass/fail.
// 3. **Evidence-preserving** — every anomaly includes the time window
//    and signal set that was inspected.
// 4. **Advisory suggestions** — [`BindingSuggestion`] objects carry
//    confidence scores but are never auto-applied.

pub mod spi;
pub mod valid_ready;

use crate::events::EvidenceWindow;
use crate::index::SignalIndex;
use serde::Serialize;

// ── Role Schema ────────────────────────────────────────────────────

/// A logical role that a protocol expects to be bound to a signal.
///
/// Each [`RoleDescriptor`] carries enough metadata for a suggestion
/// engine to rank candidate signals and for a validation engine to
/// check that the bound signal meets basic structural expectations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleDescriptor {
    /// Logical role name (e.g., `"valid"`, `"sclk"`, `"mosi"`).
    pub name: String,

    /// Human-readable description of the role's function.
    pub description: String,

    /// Expected bit width, if the protocol constrains it.
    ///
    /// `None` means "any width is acceptable".  `Some(1)` means a
    /// single-bit signal (or multi-bit treated as scalar).  Not used
    /// for hard rejection — only for suggestion ranking and validation
    /// warnings.
    pub width_hint: Option<u32>,
}

/// Complete declaration of a protocol's signal interface.
///
/// A [`RoleSchema`] is what a protocol analyzer exposes so that the
/// engine can discover required roles, validate bindings, and drive
/// the suggestion pipeline.  This struct is pure metadata — it
/// describes *what* the protocol needs, not *how* it checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleSchema {
    /// Unique protocol name (e.g., `"valid_ready"`, `"spi"`).
    pub name: String,

    /// Human-readable description of the protocol family.
    pub description: String,

    /// Roles that MUST be bound before analysis can proceed.
    pub required_roles: Vec<RoleDescriptor>,

    /// Roles that are optional — analysis can proceed without them,
    /// but the protocol may produce tighter results when they are bound.
    pub optional_roles: Vec<RoleDescriptor>,
}

impl RoleSchema {
    /// All roles (required + optional) in a single flat iterator.
    pub fn all_roles(&self) -> impl Iterator<Item = &RoleDescriptor> {
        self.required_roles.iter().chain(self.optional_roles.iter())
    }

    /// Find a role descriptor by name (exact, case-insensitive).
    pub fn find_role(&self, name: &str) -> Option<&RoleDescriptor> {
        let lower = name.to_lowercase();
        self.all_roles().find(|r| r.name.to_lowercase() == lower)
    }

    /// True when `name` is the name of a required role.
    pub fn is_required(&self, name: &str) -> bool {
        let lower = name.to_lowercase();
        self.required_roles
            .iter()
            .any(|r| r.name.to_lowercase() == lower)
    }

    /// Role names that must be bound.
    pub fn required_role_names(&self) -> Vec<&str> {
        self.required_roles
            .iter()
            .map(|r| r.name.as_str())
            .collect()
    }

    /// Role names that are optional.
    pub fn optional_role_names(&self) -> Vec<&str> {
        self.optional_roles
            .iter()
            .map(|r| r.name.as_str())
            .collect()
    }
}

// ── Binding ────────────────────────────────────────────────────────

/// How a [`RoleBinding`] was established.
///
/// The [`BindingKind`] is stored alongside every binding so that
/// reporters can indicate whether a role was explicitly set by the
/// user or derived automatically.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BindingKind {
    /// The user provided this binding explicitly (e.g., `--bind valid=top.valid`).
    UserSpecified,

    /// Resolved by the engine through wildcard, alias, or pattern matching.
    PatternMatched,

    /// Engine-generated suggestion that has NOT been confirmed by the user.
    /// Confidence is a value in `[0.0, 1.0]`.
    Suggested { confidence: f64 },
}

/// A single logical-role → concrete-signal association.
///
/// Every [`RoleBinding`] records who supplied it ([`BindingKind`]) so
/// that validation never silently promotes a suggestion into an
/// accepted binding.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoleBinding {
    /// Logical role name (must correspond to a [`RoleDescriptor::name`]).
    pub role: String,

    /// Concrete waveform signal path.
    pub signal_path: String,

    /// How this binding was established.
    pub kind: BindingKind,
}

impl RoleBinding {
    pub fn user_specified(role: impl Into<String>, signal_path: impl Into<String>) -> Self {
        RoleBinding {
            role: role.into(),
            signal_path: signal_path.into(),
            kind: BindingKind::UserSpecified,
        }
    }

    pub fn pattern_matched(role: impl Into<String>, signal_path: impl Into<String>) -> Self {
        RoleBinding {
            role: role.into(),
            signal_path: signal_path.into(),
            kind: BindingKind::PatternMatched,
        }
    }

    pub fn suggested(
        role: impl Into<String>,
        signal_path: impl Into<String>,
        confidence: f64,
    ) -> Self {
        RoleBinding {
            role: role.into(),
            signal_path: signal_path.into(),
            kind: BindingKind::Suggested { confidence },
        }
    }

    /// True when this binding was explicitly set by the user.
    pub fn is_user_specified(&self) -> bool {
        matches!(self.kind, BindingKind::UserSpecified)
    }
}

// ── Binding Suggestions ────────────────────────────────────────────

/// A single candidate signal for a logical role, with a confidence
/// score and a human-readable explanation.
#[derive(Debug, Clone, Serialize)]
pub struct BindingCandidate {
    /// Concrete waveform signal path.
    pub signal_path: String,

    /// How confident the engine is (`0.0` = pure guess, `1.0` = certain).
    pub confidence: f64,

    /// Why this candidate was suggested (e.g., `"short name matches 'valid'"`).
    pub reason: String,
}

/// A ranked list of candidates for a single unbound role.
///
/// [`BindingSuggestion`] objects are **advisory only**.  The engine
/// produces them so the user or a downstream agent can decide whether
/// to accept a suggestion, but suggestions are never automatically
/// applied.
#[derive(Debug, Clone, Serialize)]
pub struct BindingSuggestion {
    /// Logical role that needs a signal.
    pub role: String,

    /// Ranked candidates (best confidence first).
    pub candidates: Vec<BindingCandidate>,
}

// ── Binding Validation ─────────────────────────────────────────────

/// Result of validating a set of role bindings against a [`RoleSchema`].
///
/// Validation is **deterministic**: it checks that every required role
/// is bound and that every bound signal exists in the waveform index.
/// It does not require running the full protocol analyzer — it is a
/// structural gate that runs before analysis.
#[derive(Debug, Clone, Serialize)]
pub struct BindingValidation {
    /// True when all required roles are bound with existing signals.
    pub is_valid: bool,

    /// Required roles that have no binding.
    pub missing_roles: Vec<String>,

    /// Non-fatal concerns (e.g., optional role unbound, width mismatch hint).
    pub warnings: Vec<String>,

    /// For each missing role, a ranked list of candidate signals that
    /// the engine believes are plausible.
    pub suggestions: Vec<BindingSuggestion>,
}

impl BindingValidation {
    /// Convenience constructor: all required roles bound, no warnings.
    pub fn valid() -> Self {
        BindingValidation {
            is_valid: true,
            missing_roles: Vec::new(),
            warnings: Vec::new(),
            suggestions: Vec::new(),
        }
    }

    /// Convenience constructor: validation failed with missing roles.
    pub fn invalid(missing_roles: Vec<String>, suggestions: Vec<BindingSuggestion>) -> Self {
        BindingValidation {
            is_valid: false,
            missing_roles,
            warnings: Vec::new(),
            suggestions,
        }
    }
}

// ── Anomaly / Violation ────────────────────────────────────────────

/// Severity of a protocol anomaly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum AnomalySeverity {
    /// Protocol contract violation — analysis cannot be trusted.
    Error,

    /// Suspicious but not strictly a violation (e.g., a stall that
    /// exceeds a soft threshold).
    Warning,

    /// Informational observation (e.g., a protocol phase was detected
    /// but no problem exists).
    Info,
}

/// Categorisation of what went wrong (or what was observed).
///
/// New variants may be added as more protocol analyzers are registered;
/// reporters should handle unknown variants gracefully.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnomalyKind {
    /// A signal held a value longer than the protocol's stall threshold.
    Stall,

    /// A handshake phase was not acknowledged within the deadline.
    HandshakeTimeout,

    /// A required handshake phase was skipped or occurred out of order.
    HandshakeOrderViolation,

    /// A payload signal changed while the protocol was in a stable
    /// phase (e.g., data toggled while valid was asserted but before
    /// ready acknowledged).
    PayloadInstability,

    /// An expected edge (rising/falling) did not occur within the
    /// specified window.
    MissingEdge,

    /// A signal value fell outside the protocol's expected range.
    ValueOutOfRange,

    /// Unknown or ambiguous value (`x`, `z`, `u`) was observed where
    /// the protocol expects a clean binary signal.
    AmbiguousValue,

    /// A binding required by the protocol is missing or invalid.
    BindingError,

    /// A generic protocol violation that does not fit into the other
    /// categories.  The `description` field carries the explanation.
    ProtocolViolation,
}

/// A single protocol-level finding: a violation, a warning, or an
/// informational observation.
///
/// Every [`Anomaly`] refers back to the roles that were involved and
/// carries an [`EvidenceWindow`] that points at the raw waveform data
/// supporting the conclusion.
#[derive(Debug, Clone, Serialize)]
pub struct Anomaly {
    /// What kind of anomaly this is.
    pub kind: AnomalyKind,

    /// How severe the finding is.
    pub severity: AnomalySeverity,

    /// Human-readable explanation of what happened and why.
    pub description: String,

    /// The protocol role(s) implicated in this finding.
    pub related_roles: Vec<String>,

    /// Concrete signal paths that were inspected.
    pub related_signals: Vec<String>,

    /// Time window of the raw data that produced this conclusion.
    pub evidence: EvidenceWindow,
}

impl Anomaly {
    pub fn error(kind: AnomalyKind, description: String, evidence: EvidenceWindow) -> Self {
        Anomaly {
            kind,
            severity: AnomalySeverity::Error,
            description,
            related_roles: evidence.signals.clone(),
            related_signals: evidence.signals.clone(),
            evidence,
        }
    }

    pub fn warning(kind: AnomalyKind, description: String, evidence: EvidenceWindow) -> Self {
        Anomaly {
            kind,
            severity: AnomalySeverity::Warning,
            description,
            related_roles: evidence.signals.clone(),
            related_signals: evidence.signals.clone(),
            evidence,
        }
    }

    pub fn info(kind: AnomalyKind, description: String, evidence: EvidenceWindow) -> Self {
        Anomaly {
            kind,
            severity: AnomalySeverity::Info,
            description,
            related_roles: evidence.signals.clone(),
            related_signals: evidence.signals.clone(),
            evidence,
        }
    }

    /// Set the logical role names for this anomaly (overrides the
    /// default which copies `evidence.signals`).
    pub fn with_roles(mut self, roles: Vec<String>) -> Self {
        self.related_roles = roles;
        self
    }
}

// ── Protocol Plugin Trait ──────────────────────────────────────────

/// Interface that every concrete protocol analyzer implements.
///
/// # Contract
///
/// * [`schema`](ProtocolPlugin::schema) — declare the protocol's role
///   interface (required + optional roles, width hints, descriptions).
/// * [`validate_bindings`](ProtocolPlugin::validate_bindings) — check
///   that a set of [`RoleBinding`] objects satisfies the schema.  Must
///   be deterministic and pure — no LLM, no randomness.
/// * [`suggest_bindings`](ProtocolPlugin::suggest_bindings) — produce
///   ranked, confidence-scored suggestions for any unbound roles by
///   inspecting the waveform's [`SignalIndex`].
/// * [`name`](ProtocolPlugin::name) / [`description`](ProtocolPlugin::description)
///   — metadata for discovery and CLI listing.
///
/// # Usage
///
/// Concrete analyzers (valid/ready, SPI) implement this trait and are
/// registered into the [`ProtocolCatalog`] at startup.  The engine
/// calls `validate_bindings` before analysis and `suggest_bindings`
/// when the user asks for binding help.
pub trait ProtocolPlugin {
    /// The role schema declared by this plugin.
    fn schema(&self) -> &RoleSchema;

    /// Validate a set of role bindings against the schema.
    ///
    /// The `index` parameter allows the validator to check that every
    /// bound signal exists in the waveform.
    fn validate_bindings(&self, bindings: &[RoleBinding], index: &SignalIndex)
        -> BindingValidation;

    /// Suggest concrete signal paths for unbound (or all) roles by
    /// inspecting the waveform's signal index.
    ///
    /// Returns ranked suggestions with confidence scores.  Callers
    /// should present these to the user — the engine never auto-accepts
    /// a suggestion.
    fn suggest_bindings(&self, _index: &SignalIndex) -> Vec<BindingSuggestion> {
        // Default: return empty — concrete plugins override this.
        Vec::new()
    }

    /// Short, unique protocol name (e.g., `"valid_ready"`, `"spi"`).
    fn name(&self) -> &str;

    /// Human-readable description of what this protocol checks.
    fn description(&self) -> &str;
}

// ── Protocol Catalog ───────────────────────────────────────────────

/// Metadata returned by [`ProtocolCatalog::discovery_metadata`] so
/// that the CLI can list available protocol schemas without loading
/// a waveform.
#[derive(Debug, Clone, Serialize)]
pub struct ProtocolDiscoveryMetadata {
    /// Unique protocol name.
    pub name: String,

    /// Human-readable description.
    pub description: String,

    /// Number of roles that must be bound.
    pub required_role_count: usize,

    /// Number of roles that are optional.
    pub optional_role_count: usize,

    /// Names of required roles (for quick inspection).
    pub required_roles: Vec<String>,

    /// Names of optional roles.
    pub optional_roles: Vec<String>,
}

/// Runtime registry of available protocol plugins.
///
/// The catalog is the single source of truth for protocol discovery.
/// Plugins are registered at startup (or lazily) and looked up by name
/// when the user invokes `bind` or `analyze`.
///
/// # Thread safety
///
/// The catalog is not `Send + Sync` because it may hold boxed trait
/// objects that own waveform-specific state.  Each `Session` should
/// own its own catalog (or a shared catalog initialised once).
pub struct ProtocolCatalog {
    /// Registered protocol plugins.
    plugins: Vec<Box<dyn ProtocolPlugin>>,
}

impl ProtocolCatalog {
    /// Create an empty catalog.
    pub fn new() -> Self {
        ProtocolCatalog {
            plugins: Vec::new(),
        }
    }

    /// Register a protocol plugin.
    ///
    /// Panics if a plugin with the same name is already registered.
    pub fn register(&mut self, plugin: Box<dyn ProtocolPlugin>) {
        let name = plugin.name().to_string();
        assert!(
            !self.plugins.iter().any(|p| p.name() == name),
            "duplicate protocol plugin: {name}"
        );
        self.plugins.push(plugin);
    }

    /// Look up a plugin by name (exact match).
    pub fn get(&self, name: &str) -> Option<&dyn ProtocolPlugin> {
        self.plugins
            .iter()
            .find(|p| p.name() == name)
            .map(|p| p.as_ref())
    }

    /// Number of registered plugins.
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// True when no plugins are registered.
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Discovery metadata for every registered plugin.
    ///
    /// This is the entry point for the CLI's `protocols` subcommand.
    pub fn discovery_metadata(&self) -> Vec<ProtocolDiscoveryMetadata> {
        self.plugins
            .iter()
            .map(|p| {
                let schema = p.schema();
                ProtocolDiscoveryMetadata {
                    name: p.name().to_string(),
                    description: p.description().to_string(),
                    required_role_count: schema.required_roles.len(),
                    optional_role_count: schema.optional_roles.len(),
                    required_roles: schema
                        .required_role_names()
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                    optional_roles: schema
                        .optional_role_names()
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                }
            })
            .collect()
    }
}

impl Default for ProtocolCatalog {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── RoleSchema ──────────────────────────────────────────────

    fn make_valid_ready_schema() -> RoleSchema {
        RoleSchema {
            name: "valid_ready".into(),
            description: "Generic valid/ready handshake".into(),
            required_roles: vec![
                RoleDescriptor {
                    name: "valid".into(),
                    description: "Source asserts when data is available".into(),
                    width_hint: Some(1),
                },
                RoleDescriptor {
                    name: "ready".into(),
                    description: "Sink asserts when it can accept data".into(),
                    width_hint: Some(1),
                },
            ],
            optional_roles: vec![RoleDescriptor {
                name: "data".into(),
                description: "Payload bus".into(),
                width_hint: None,
            }],
        }
    }

    #[test]
    fn test_role_schema_required_role_names() {
        let schema = make_valid_ready_schema();
        assert_eq!(schema.required_role_names(), vec!["valid", "ready"]);
        assert_eq!(schema.optional_role_names(), vec!["data"]);
    }

    #[test]
    fn test_role_schema_find_role() {
        let schema = make_valid_ready_schema();
        assert!(schema.find_role("valid").is_some());
        assert!(schema.find_role("VALID").is_some()); // case-insensitive
        assert!(schema.find_role("nonexistent").is_none());
    }

    #[test]
    fn test_role_schema_is_required() {
        let schema = make_valid_ready_schema();
        assert!(schema.is_required("valid"));
        assert!(schema.is_required("ready"));
        assert!(!schema.is_required("data"));
    }

    // ── RoleBinding ────────────────────────────────────────────

    #[test]
    fn test_role_binding_user_specified() {
        let binding = RoleBinding::user_specified("valid", "top.valid");
        assert_eq!(binding.role, "valid");
        assert_eq!(binding.signal_path, "top.valid");
        assert!(binding.is_user_specified());
    }

    #[test]
    fn test_role_binding_suggested() {
        let binding = RoleBinding::suggested("ready", "top.rdy", 0.85);
        assert!(!binding.is_user_specified());
        match binding.kind {
            BindingKind::Suggested { confidence } => {
                assert!((confidence - 0.85).abs() < f64::EPSILON);
            }
            _ => panic!("expected Suggested"),
        }
    }

    #[test]
    fn test_binding_serialization_tags() {
        let b = RoleBinding::user_specified("valid", "top.valid");
        let json = serde_json::to_string(&b).unwrap();
        assert!(json.contains("user_specified"));

        let b = RoleBinding::suggested("ready", "top.rdy", 0.5);
        let json = serde_json::to_string(&b).unwrap();
        assert!(json.contains("suggested"));
        assert!(json.contains("0.5"));
    }

    // ── BindingValidation ──────────────────────────────────────

    #[test]
    fn test_binding_validation_valid() {
        let v = BindingValidation::valid();
        assert!(v.is_valid);
        assert!(v.missing_roles.is_empty());
        assert!(v.suggestions.is_empty());
    }

    #[test]
    fn test_binding_validation_invalid() {
        let s = vec![BindingSuggestion {
            role: "ready".into(),
            candidates: vec![BindingCandidate {
                signal_path: "top.rdy".into(),
                confidence: 0.9,
                reason: "short name matches 'ready'".into(),
            }],
        }];
        let v = BindingValidation::invalid(vec!["ready".into()], s);
        assert!(!v.is_valid);
        assert_eq!(v.missing_roles, vec!["ready"]);
        assert_eq!(v.suggestions.len(), 1);
        assert_eq!(v.suggestions[0].candidates[0].signal_path, "top.rdy");
    }

    // ── Anomaly ────────────────────────────────────────────────

    #[test]
    fn test_anomaly_construction() {
        let evidence = EvidenceWindow::new(
            0,
            Some(100),
            vec!["top.valid".into(), "top.ready".into()],
            "handshake timeout check".into(),
        );
        let anomaly = Anomaly::error(
            AnomalyKind::HandshakeTimeout,
            "ready did not assert before deadline".into(),
            evidence,
        )
        .with_roles(vec!["valid".into(), "ready".into()]);

        assert_eq!(anomaly.kind, AnomalyKind::HandshakeTimeout);
        assert_eq!(anomaly.severity, AnomalySeverity::Error);
        assert!(anomaly.description.contains("did not assert"));
        assert_eq!(anomaly.related_roles, vec!["valid", "ready"]);
        assert_eq!(anomaly.related_signals, vec!["top.valid", "top.ready"]);
        assert_eq!(anomaly.evidence.start, 0);
    }

    #[test]
    fn test_anomaly_warning_and_info_severity() {
        let evidence =
            EvidenceWindow::new(0, Some(50), vec!["top.clk".into()], "stall check".into());
        let warn = Anomaly::warning(AnomalyKind::Stall, "long stall".into(), evidence.clone());
        assert_eq!(warn.severity, AnomalySeverity::Warning);

        let info = Anomaly::info(AnomalyKind::Stall, "normal stall".into(), evidence);
        assert_eq!(info.severity, AnomalySeverity::Info);
    }

    // ── ProtocolCatalog ────────────────────────────────────────

    struct MockPlugin {
        schema: RoleSchema,
    }

    impl ProtocolPlugin for MockPlugin {
        fn schema(&self) -> &RoleSchema {
            &self.schema
        }
        fn validate_bindings(
            &self,
            _bindings: &[RoleBinding],
            _index: &SignalIndex,
        ) -> BindingValidation {
            BindingValidation::valid()
        }
        fn name(&self) -> &str {
            &self.schema.name
        }
        fn description(&self) -> &str {
            &self.schema.description
        }
    }

    #[test]
    fn test_catalog_register_and_lookup() {
        let mut cat = ProtocolCatalog::new();
        assert!(cat.is_empty());

        let plugin = Box::new(MockPlugin {
            schema: make_valid_ready_schema(),
        });
        cat.register(plugin);
        assert_eq!(cat.len(), 1);

        let found = cat.get("valid_ready").expect("should find plugin");
        assert_eq!(found.name(), "valid_ready");
        assert_eq!(found.schema().required_roles.len(), 2);
    }

    #[test]
    #[should_panic(expected = "duplicate protocol plugin")]
    fn test_catalog_rejects_duplicate() {
        let mut cat = ProtocolCatalog::new();
        cat.register(Box::new(MockPlugin {
            schema: make_valid_ready_schema(),
        }));
        // Second registration with same name should panic
        cat.register(Box::new(MockPlugin {
            schema: make_valid_ready_schema(),
        }));
    }

    #[test]
    fn test_catalog_discovery_metadata() {
        let mut cat = ProtocolCatalog::new();
        cat.register(Box::new(MockPlugin {
            schema: make_valid_ready_schema(),
        }));

        let meta = cat.discovery_metadata();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].name, "valid_ready");
        assert_eq!(meta[0].required_role_count, 2);
        assert_eq!(meta[0].optional_role_count, 1);
        assert_eq!(meta[0].required_roles, vec!["valid", "ready"]);
        assert_eq!(meta[0].optional_roles, vec!["data"]);
    }

    #[test]
    fn test_catalog_missing_plugin_returns_none() {
        let cat = ProtocolCatalog::new();
        assert!(cat.get("nonexistent").is_none());
    }

    // ── RoleDescriptor serialization ───────────────────────────

    #[test]
    fn test_role_descriptor_serialization() {
        let rd = RoleDescriptor {
            name: "valid".into(),
            description: "Source asserts when data available".into(),
            width_hint: Some(1),
        };
        let json = serde_json::to_string(&rd).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["name"], "valid");
        assert_eq!(parsed["width_hint"], 1);
    }

    // ── BindingSuggestion serialization ────────────────────────

    #[test]
    fn test_binding_suggestion_serialization() {
        let suggestion = BindingSuggestion {
            role: "valid".into(),
            candidates: vec![
                BindingCandidate {
                    signal_path: "top.valid".into(),
                    confidence: 0.95,
                    reason: "exact short name match 'valid'".into(),
                },
                BindingCandidate {
                    signal_path: "top.vld".into(),
                    confidence: 0.4,
                    reason: "fuzzy match 'vld' ≈ 'valid'".into(),
                },
            ],
        };
        let json = serde_json::to_string(&suggestion).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["role"], "valid");
        assert_eq!(parsed["candidates"].as_array().unwrap().len(), 2);
        assert_eq!(parsed["candidates"][0]["confidence"], 0.95);
    }
}

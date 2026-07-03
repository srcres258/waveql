use crate::backend::WaveformBackend;
use std::collections::HashMap;

// ── SignalHandle ──────────────────────────────────────────────────

/// Lightweight stable handle referencing a signal in the index.
///
/// Carries only an index — no signal data is materialized.
/// The handle remains valid as long as the `SignalIndex` is not rebuilt.
/// Use [`SignalIndex::get`] to retrieve the associated [`SignalEntry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SignalHandle(usize);

// ── SignalEntry ───────────────────────────────────────────────────

/// Metadata entry for a single signal, stored in the [`SignalIndex`].
///
/// All fields are populated from the backend at index-build time.
/// No waveform history is loaded — this is purely structural metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalEntry {
    /// Full hierarchical path from the waveform file (e.g., `"top.sub.clk"`).
    pub path: String,

    /// Last component of the path (e.g., `"clk"`).
    pub short_name: String,

    /// Hierarchy segments above the signal (e.g., `["top", "sub"]`).
    pub hierarchy: Vec<String>,

    /// Bit width of the signal.
    pub width: u32,

    /// Optional RTL kind (e.g., `"wire"`, `"reg"`).
    /// `None` when the backend does not expose this information.
    /// Reserved for future role-binding and protocol analysis.
    pub kind: Option<String>,

    /// Opaque per-backend handle used to load signal data.
    ///
    /// Currently the signal path string — passed directly to
    /// [`WaveformBackend::signal_data`].  In the future this may
    /// carry backend-specific opaque references (e.g., `wellen::SignalRef`).
    pub backend_handle: String,
}

// ── LookupResult ──────────────────────────────────────────────────

/// Result returned by [`SignalIndex::lookup`].
#[derive(Debug, Clone)]
pub struct LookupResult {
    /// Stable handle into the index.
    pub handle: SignalHandle,

    /// Copy of the matched entry.
    pub entry: SignalEntry,

    /// How the match was resolved:
    /// - `"exact"` — full path matched character-for-character.
    /// - `"short_name"` — matched by last hierarchy segment.
    /// - `"alias"` — matched through normalization rules.
    pub match_kind: String,

    /// The normalized query form that actually matched.
    pub matched_query: String,
}

// ── SignalIndex ───────────────────────────────────────────────────

/// Searchable signal index built from any [`WaveformBackend`].
///
/// Supports three tiers of lookup (tried in priority order):
///
/// 1. **Exact match** — full hierarchical path, character-for-character.
/// 2. **Short-name match** — last segment of the path only.
/// 3. **Normalized-alias match** — case-insensitive, RTL abbreviation
///    expansion, common suffix stripping (see [`normalize_query`]).
///
/// When nothing matches, [`SignalIndex::suggest`] provides ranked fuzzy
/// candidates suitable for error messages or interactive selection.
///
/// # Building
///
/// ```ignore
/// let index = SignalIndex::build(backend);
/// ```
///
/// The index is read-only after construction.  Rebuild it when the
/// signal list changes (e.g., after opening a new file).
pub struct SignalIndex {
    entries: Vec<SignalEntry>,

    /// Full path → handle.
    by_path: HashMap<String, SignalHandle>,

    /// Short (last-segment) name → all handles sharing that short name.
    by_short_name: HashMap<String, Vec<SignalHandle>>,

    /// Normalized alias string → handles whose normalized forms match.
    aliases: HashMap<String, Vec<SignalHandle>>,
}

impl SignalIndex {
    // ── Construction ──────────────────────────────────────────

    /// Build the index by scanning all signals exposed by `backend`.
    ///
    /// Iterates [`WaveformBackend::signal_iter`] once; no signal data
    /// is loaded.  The index is agnostic to VCD vs. FST provenance —
    /// normalization rules are backend-independent.
    pub fn build(backend: &dyn WaveformBackend) -> Self {
        let signals: Vec<crate::backend::types::SignalInfo> =
            backend.signal_iter().cloned().collect();

        let mut entries = Vec::with_capacity(signals.len());
        let mut by_path = HashMap::with_capacity(signals.len());
        let mut by_short_name: HashMap<String, Vec<SignalHandle>> = HashMap::new();
        let mut aliases: HashMap<String, Vec<SignalHandle>> = HashMap::new();

        for (i, sig) in signals.iter().enumerate() {
            let handle = SignalHandle(i);

            // ── hierarchy parsing ──
            let hierarchy: Vec<String> = sig.path.split('.').map(|s| s.to_string()).collect();
            let short_name = hierarchy
                .last()
                .cloned()
                .unwrap_or_else(|| sig.path.clone());
            let parent_hierarchy = hierarchy[..hierarchy.len().saturating_sub(1)].to_vec();

            let entry = SignalEntry {
                path: sig.path.clone(),
                short_name: short_name.clone(),
                hierarchy: parent_hierarchy,
                width: sig.width,
                kind: None, // backend does not expose kind yet
                backend_handle: sig.path.clone(),
            };

            by_path.insert(sig.path.clone(), handle);

            by_short_name
                .entry(short_name.clone())
                .or_default()
                .push(handle);

            // Pre-compute every normalized alias form that should
            // resolve to this signal.  A later query like "clock" must
            // find an entry whose path contains "clk".
            let alias_forms = signal_alias_forms(&sig.path);
            for alias in alias_forms {
                aliases.entry(alias).or_default().push(handle);
            }

            entries.push(entry);
        }

        SignalIndex {
            entries,
            by_path,
            by_short_name,
            aliases,
        }
    }

    // ── Exact Lookup ─────────────────────────────────────────

    /// Exact lookup by full hierarchical path.
    ///
    /// Returns `None` when no signal matches character-for-character.
    pub fn lookup_exact(&self, path: &str) -> Option<(SignalHandle, &SignalEntry)> {
        self.by_path.get(path).map(|&h| (h, &self.entries[h.0]))
    }

    // ── Tiered Lookup ────────────────────────────────────────

    /// Multi-tier lookup: exact → short-name → normalized alias.
    ///
    /// Returns *all* matches (there may be multiple when short-name
    /// or alias resolution is ambiguous, e.g., two modules both export
    /// a signal named `clk`).
    ///
    /// When nothing matches the returned `Vec` is empty — call
    /// [`SignalIndex::suggest`] for ranked fuzzy candidates.
    pub fn lookup(&self, query: &str) -> Vec<LookupResult> {
        // ── Tier 1: exact path match ──
        if let Some(&handle) = self.by_path.get(query) {
            let entry = &self.entries[handle.0];
            return vec![LookupResult {
                handle,
                entry: entry.clone(),
                match_kind: "exact".into(),
                matched_query: query.to_string(),
            }];
        }

        // ── Tier 2 + Tier 3: short-name + normalized alias ──
        //
        // Both tiers are searched and results are combined.
        // Short-name matches come first in the result order, then alias
        // matches.  Duplicates (same handle) are suppressed — a signal
        // that matches both via short-name AND alias only appears once.

        let mut results: Vec<LookupResult> = Vec::new();
        let mut seen: std::collections::HashSet<SignalHandle> = std::collections::HashSet::new();

        if let Some(handles) = self.by_short_name.get(query) {
            for &h in handles {
                if seen.insert(h) {
                    let entry = &self.entries[h.0];
                    results.push(LookupResult {
                        handle: h,
                        entry: entry.clone(),
                        match_kind: "short_name".into(),
                        matched_query: query.to_string(),
                    });
                }
            }
        }

        let normalized_forms = normalize_query(query);
        for nf in &normalized_forms {
            if let Some(handles) = self.aliases.get(nf) {
                for &h in handles {
                    if seen.insert(h) {
                        let entry = &self.entries[h.0];
                        results.push(LookupResult {
                            handle: h,
                            entry: entry.clone(),
                            match_kind: "alias".into(),
                            matched_query: nf.clone(),
                        });
                    }
                }
            }
        }

        results
    }

    // ── Fuzzy Suggestions ────────────────────────────────────

    /// Ranked fuzzy suggestions for when [`lookup`](Self::lookup) returns empty.
    ///
    /// Candidates are scored by a cheap prefix+substring heuristic and
    /// returned in best-match-first order.  At most `limit` results.
    ///
    /// Designed for error messages and interactive disambiguation, not
    /// for direct resolution — callers should present the list to the
    /// user rather than auto-picking the top result.
    pub fn suggest(&self, query: &str, limit: usize) -> Vec<&SignalEntry> {
        let query_lower = query.to_lowercase();
        let mut scored: Vec<(&SignalEntry, usize)> = self
            .entries
            .iter()
            .filter_map(|e| {
                let s = fuzzy_score(
                    &query_lower,
                    &e.short_name.to_lowercase(),
                    &e.path.to_lowercase(),
                );
                if s > 0 {
                    Some((e, s))
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by_key(|b| std::cmp::Reverse(b.1));
        scored.truncate(limit);
        scored.into_iter().map(|(e, _)| e).collect()
    }

    // ── Accessors ────────────────────────────────────────────

    /// Retrieve the entry for a previously obtained [`SignalHandle`].
    #[inline]
    pub fn get(&self, handle: SignalHandle) -> &SignalEntry {
        &self.entries[handle.0]
    }

    /// Borrow all entries (e.g., for enumeration / listing).
    #[inline]
    pub fn entries(&self) -> &[SignalEntry] {
        &self.entries
    }

    /// Number of signals in the index.
    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the index has zero entries.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ── Normalization Engine ──────────────────────────────────────────
//
// Signal names in HDL waveforms exhibit several systematic
// inconsistencies that make exact-path matching brittle:
//
//   Source               Example
//   ──────────────────   ──────────────────────────
//   VCD scope prefix     module:top.sub.clk  →  top.sub.clk
//   Verilator            TOP__DOT__sub__DOT__clk
//   VCS / Icarus         top.sub.clk
//   Vivado XSIM          /top/sub/clk
//
// Even after WaveQL's unified-path representation (dot-separated
// hierarchy), users query signals with different conventions:
//
//   - Short name only:          "clk"        (no hierarchy)
//   - Case variation:           "CLK", "Clk"
//   - RTL abbreviations:        "clock"      ↔ "clk"
//                                "reset"      ↔ "rst"
//   - Active-low suffixes:      "rst_n"      → "rst"
//   - Register-stage suffixes:  "data_reg"   → "data"
//
// The normalization rules are **backend-agnostic** — they operate on
// the unified dot-separated paths and apply the same transformations
// regardless of whether the source was VCD or FST.
//
// ## Rule Summary
//
// | # | Rule                     | Input Example | Output Examples           |
// |---|--------------------------|---------------|---------------------------|
// | 1 | lower-case               | `TOP.CLK`     | `top.clk`                 |
// | 2 | short name               | `top.sub.clk` | `clk`                     |
// | 3 | clk ↔ clock              | `clock`       | `clk`                     |
// | 4 | rst ↔ reset              | `reset`       | `rst`                     |
// | 5 | en ↔ enable              | `enable`      | `en`                      |
// | 6 | strip _n (active-low)    | `rst_n`       | `rst`                     |
// | 7 | strip _reg, _d, _q, _r   | `data_reg`    | `data`                    |
//
// Rules are applied to each dot-separated segment independently
// (hierarchy segments are also normalized).  The cross-product of
// segment-level expansions is *not* computed — only segment-local
// aliases are indexed, keeping the alias map linear in signal count.

// ── Alias generation (per-signal, at index-build time) ──────────

/// Generate every normalized alias form that should resolve to the
/// signal at `path`.  These are stored in `SignalIndex.aliases`.
fn signal_alias_forms(path: &str) -> Vec<String> {
    let mut forms = Vec::new();

    let lower = path.to_lowercase();
    forms.push(lower.clone());

    // Short name
    if let Some(short) = path.rsplit('.').next() {
        forms.push(short.to_lowercase());
    }

    // Per-segment alias expansions
    for segment in path.split('.') {
        let seg_lower = segment.to_lowercase();
        // Abbreviation-expanded forms of this segment
        for expanded in expand_abbreviations(&seg_lower) {
            if expanded != seg_lower {
                forms.push(expanded);
            }
        }
        // Suffix-stripped forms
        for stripped in strip_suffixes(&seg_lower) {
            if stripped != seg_lower {
                forms.push(stripped);
            }
        }
    }

    forms.sort();
    forms.dedup();
    forms
}

// ── Query normalization (at lookup time) ───────────────────────

/// Transform a raw user query into a set of canonical forms to try
/// against the alias map.
fn normalize_query(query: &str) -> Vec<String> {
    let mut candidates = Vec::new();

    let lower = query.to_lowercase();
    candidates.push(lower.clone());

    // Abbreviation expansion / contraction
    candidates.extend(expand_abbreviations(&lower));

    // Suffix stripping
    candidates.extend(strip_suffixes(&lower));

    // If query is a short name, also try it as-is against aliases
    // (the alias map already contains short-name entries).

    candidates.sort();
    candidates.dedup();
    candidates
}

// ── Abbreviation Expansion ─────────────────────────────────────

/// Common RTL abbreviation pairs.  Expands in both directions:
/// `"clk"` yields `"clock"` and `"clock"` yields `"clk"`.
const ABBREVIATIONS: &[(&str, &str)] = &[
    ("clk", "clock"),
    ("rst", "reset"),
    ("en", "enable"),
    ("addr", "address"),
    ("cnt", "count"),
    ("sig", "signal"),
    ("req", "request"),
    ("ack", "acknowledge"),
    ("reg", "register"),
    ("cfg", "config"),
];

/// For a given single segment (no dots), return the set of
/// abbreviation-expanded/contracted forms.
fn expand_abbreviations(segment: &str) -> Vec<String> {
    let mut out = Vec::new();
    for &(short, long) in ABBREVIATIONS {
        if segment == short {
            out.push(long.to_string());
        }
        if segment == long {
            out.push(short.to_string());
        }
    }
    out
}

// ── Suffix Stripping ───────────────────────────────────────────

/// Common HDL suffixes that do not change the signal's logical identity.
const STRIP_SUFFIXES: &[&str] = &["_n", "_b", "_bar", "_reg", "_d", "_q", "_r"];

/// For a single segment, return forms with each recognised suffix
/// stripped (when present).
fn strip_suffixes(segment: &str) -> Vec<String> {
    let mut out = Vec::new();
    for suffix in STRIP_SUFFIXES {
        if let Some(base) = segment.strip_suffix(suffix) {
            if !base.is_empty() {
                out.push(base.to_string());
            }
        }
    }
    out
}

// ── Fuzzy Scoring ──────────────────────────────────────────────

/// Cheap heuristic: score a candidate entry against a lower-cased query.
///
/// Scoring tiers (higher = better match):
/// - 20: short name is an exact match
/// - 15: short name starts with the query
/// - 12: high char-set overlap (typo-tolerant, ≥ 70%)
/// - 10: path contains the query as a substring
/// - 7:  medium char-set overlap (≥ 50%)
/// - 5:  short name contains the query as a substring
/// - 1:  any token (dot-separated segment) matches prefix or substring
fn fuzzy_score(query_lower: &str, short_lower: &str, path_lower: &str) -> usize {
    if short_lower == query_lower {
        return 20;
    }
    if short_lower.starts_with(query_lower) {
        return 15;
    }

    let char_sim = char_jaccard(short_lower, query_lower);
    if char_sim >= 0.7 {
        return 12;
    }
    if path_lower.contains(query_lower) {
        return 10;
    }
    if char_sim >= 0.5 {
        return 7;
    }
    if short_lower.contains(query_lower) {
        return 5;
    }
    for token in path_lower.split('.') {
        if token == query_lower || token.starts_with(query_lower) {
            return 3;
        }
        if token.contains(query_lower) {
            return 1;
        }
    }
    0
}

/// Jaccard similarity over character sets (order-agnostic).
/// Handles typos, insertions, and deletions naturally because the
/// majority of characters are shared.
fn char_jaccard(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let chars_a: std::collections::HashSet<u8> = a.bytes().collect();
    let chars_b: std::collections::HashSet<u8> = b.bytes().collect();
    let intersection = chars_a.intersection(&chars_b).count();
    let union = chars_a.len() + chars_b.len() - intersection;
    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::capabilities::BackendCapabilities;
    use crate::backend::metadata::WaveformMetadata;
    use crate::backend::types::{FileFormat, SignalData, SignalInfo, Timescale};
    use crate::error::WaveqlError;

    // ── Mock Backend ──────────────────────────────────────────

    /// Minimal backend implementation used only for index tests.
    struct MockBackend {
        metadata: WaveformMetadata,
        signals: Vec<SignalInfo>,
        capabilities: BackendCapabilities,
    }

    impl MockBackend {
        fn new(signals: Vec<(&str, u32)>) -> Self {
            let sig_infos: Vec<SignalInfo> = signals
                .into_iter()
                .map(|(path, width)| SignalInfo {
                    path: path.to_string(),
                    width,
                })
                .collect();
            let count = sig_infos.len();
            MockBackend {
                metadata: WaveformMetadata {
                    timescale: Timescale::default(),
                    date: None,
                    version: None,
                    signal_count: count,
                    format: FileFormat::Vcd,
                },
                signals: sig_infos,
                capabilities: BackendCapabilities {
                    supports_lazy_load: true,
                    supports_slice: true,
                    supports_incremental: false,
                    format: FileFormat::Vcd,
                    description: "mock",
                },
            }
        }
    }

    impl WaveformBackend for MockBackend {
        fn metadata(&self) -> &WaveformMetadata {
            &self.metadata
        }
        fn capabilities(&self) -> &BackendCapabilities {
            &self.capabilities
        }
        fn signal_info(&self, path: &str) -> Result<&SignalInfo, WaveqlError> {
            self.signals
                .iter()
                .find(|s| s.path == path)
                .ok_or_else(|| WaveqlError::SignalNotFound(path.to_string()))
        }
        fn signal_iter(&self) -> Box<dyn Iterator<Item = &SignalInfo> + '_> {
            Box::new(self.signals.iter())
        }
        fn load_signals(&mut self, _paths: &[String]) -> Result<(), WaveqlError> {
            Ok(())
        }
        fn signal_data(&self, path: &str) -> Result<&SignalData, WaveqlError> {
            Err(WaveqlError::SignalNotFound(path.to_string()))
        }
    }

    fn make_backend() -> MockBackend {
        MockBackend::new(vec![
            ("top.clk", 1),
            ("top.rst_n", 1),
            ("top.en", 1),
            ("top.data", 8),
            ("top.reg_file.data_reg", 32),
            ("top.counter.addr", 16),
        ])
    }

    // ── Exact Lookup ──────────────────────────────────────────

    #[test]
    fn test_exact_lookup_finds() {
        let backend = make_backend();
        let index = SignalIndex::build(&backend);

        let (handle, entry) = index.lookup_exact("top.clk").expect("should find top.clk");
        assert_eq!(entry.path, "top.clk");
        assert_eq!(entry.short_name, "clk");
        assert_eq!(entry.hierarchy, vec!["top"]);
        assert_eq!(entry.width, 1);
        assert_eq!(index.get(handle).path, "top.clk");
    }

    #[test]
    fn test_exact_lookup_missing() {
        let backend = make_backend();
        let index = SignalIndex::build(&backend);
        assert!(index.lookup_exact("top.nonexistent").is_none());
    }

    // ── Short-Name Lookup ─────────────────────────────────────

    #[test]
    fn test_short_name_lookup_finds_clk() {
        let backend = make_backend();
        let index = SignalIndex::build(&backend);

        let results = index.lookup("clk");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.path, "top.clk");
        assert_eq!(results[0].match_kind, "short_name");
    }

    #[test]
    fn test_short_name_lookup_finds_data() {
        let backend = make_backend();
        let index = SignalIndex::build(&backend);

        let results = index.lookup("data");
        // "data" matches top.data (short_name) AND
        // top.reg_file.data_reg (alias via suffix stripping _reg → "data")
        assert!(!results.is_empty(), "should find at least top.data");
        let has_data = results.iter().any(|r| r.entry.path == "top.data");
        assert!(has_data, "should include top.data in results");
    }

    // ── Alias (Normalized) Lookup ─────────────────────────────

    #[test]
    fn test_alias_clock_matches_clk() {
        let backend = make_backend();
        let index = SignalIndex::build(&backend);

        let results = index.lookup("clock");
        assert!(!results.is_empty(), "should find clk via clock alias");
        assert_eq!(results[0].entry.path, "top.clk");
        assert_eq!(results[0].match_kind, "alias");
    }

    #[test]
    fn test_alias_reset_matches_rst_n() {
        let backend = make_backend();
        let index = SignalIndex::build(&backend);

        let results = index.lookup("reset");
        assert!(!results.is_empty(), "should find rst_n via reset alias");
        assert_eq!(results[0].entry.path, "top.rst_n");
        assert_eq!(results[0].match_kind, "alias");
    }

    #[test]
    fn test_alias_rst_matches_rst_n() {
        let backend = make_backend();
        let index = SignalIndex::build(&backend);

        // "rst" should match "rst_n" via suffix stripping
        let results = index.lookup("rst");
        assert!(!results.is_empty(), "should find rst_n via rst alias");
        // The match_kind depends on whether "rst" is an alias or short-name
        // (there's no short_name "rst" so it should be alias)
        let found = results.iter().any(|r| r.entry.path == "top.rst_n");
        assert!(found, "rst should match top.rst_n via alias resolution");
    }

    #[test]
    fn test_alias_data_reg_stripped() {
        let backend = make_backend();
        let index = SignalIndex::build(&backend);

        // "data_reg" → "data" (strip suffix).  "data" exists as short_name.
        let results = index.lookup("data_reg");
        let found = results.iter().any(|r| r.entry.path == "top.data");
        assert!(found, "data_reg should match top.data via suffix stripping");
    }

    #[test]
    fn test_alias_enable_matches_en() {
        let backend = make_backend();
        let index = SignalIndex::build(&backend);

        let results = index.lookup("enable");
        assert!(!results.is_empty(), "should find en via enable alias");
        assert_eq!(results[0].entry.path, "top.en");
    }

    #[test]
    fn test_alias_address_matches_addr() {
        let backend = make_backend();
        let index = SignalIndex::build(&backend);

        let results = index.lookup("address");
        let found = results.iter().any(|r| r.entry.path.contains("addr"));
        assert!(found, "address should match addr via alias expansion");
    }

    // ── Case Insensitivity ────────────────────────────────────

    #[test]
    fn test_case_insensitive_lookup() {
        let backend = make_backend();
        let index = SignalIndex::build(&backend);

        let results = index.lookup("TOP.CLK");
        assert!(!results.is_empty(), "TOP.CLK should match top.clk");
        let found = results.iter().any(|r| r.entry.path == "top.clk");
        assert!(found);
    }

    // ── Missing / Suggestions ─────────────────────────────────

    #[test]
    fn test_missing_signal_returns_empty() {
        let backend = make_backend();
        let index = SignalIndex::build(&backend);

        let results = index.lookup("garbage_not_a_signal");
        assert!(results.is_empty(), "unknown query should return empty vec");
    }

    #[test]
    fn test_suggest_returns_candidates_for_missing() {
        let backend = make_backend();
        let index = SignalIndex::build(&backend);

        let suggestions = index.suggest("clok", 5);
        assert!(!suggestions.is_empty(), "typo 'clok' should suggest 'clk'");
        let has_clk = suggestions.iter().any(|e| e.path == "top.clk");
        assert!(
            has_clk,
            "suggestions for 'clok' should include 'top.clk', got: {:?}",
            suggestions.iter().map(|e| &e.path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_suggest_does_not_return_false_positive_for_orthogonal_query() {
        let backend = make_backend();
        let index = SignalIndex::build(&backend);

        let suggestions = index.suggest("zzzzzz", 5);
        // "zzzzzz" has no overlap with any signal name
        assert!(
            suggestions.is_empty(),
            "completely unrelated query should return no suggestions"
        );
    }

    #[test]
    fn test_lookup_still_returns_empty_for_missing_signal() {
        // Even with aliases, a signal that truly doesn't exist should
        // return empty (not a false positive).
        let backend = make_backend();
        let index = SignalIndex::build(&backend);

        // "widget" is not a known abbreviation and doesn't appear in any path
        let results = index.lookup("widget");
        assert!(
            results.is_empty(),
            "'widget' should not produce a false positive lookup"
        );
    }

    // ── SignalEntry Fields ────────────────────────────────────

    #[test]
    fn test_entry_fields_populated() {
        let backend = make_backend();
        let index = SignalIndex::build(&backend);

        let (_, entry) = index.lookup_exact("top.reg_file.data_reg").unwrap();
        assert_eq!(entry.path, "top.reg_file.data_reg");
        assert_eq!(entry.short_name, "data_reg");
        assert_eq!(entry.hierarchy, vec!["top", "reg_file"]);
        assert_eq!(entry.width, 32);
        assert_eq!(entry.kind, None);
        assert_eq!(entry.backend_handle, "top.reg_file.data_reg");
    }

    // ── Index Size ────────────────────────────────────────────

    #[test]
    fn test_index_len_matches_signal_count() {
        let backend = make_backend();
        let index = SignalIndex::build(&backend);
        assert_eq!(index.len(), 6);
        assert!(!index.is_empty());
    }

    #[test]
    fn test_empty_index() {
        let backend = MockBackend::new(vec![]);
        let index = SignalIndex::build(&backend);
        assert_eq!(index.len(), 0);
        assert!(index.is_empty());
    }

    // ── SignalHandle Stability ────────────────────────────────

    #[test]
    fn test_handle_is_lightweight_copy() {
        let backend = make_backend();
        let index = SignalIndex::build(&backend);

        let h1 = index.lookup_exact("top.clk").unwrap().0;
        // Handles are Copy
        let h2 = h1;
        assert_eq!(index.get(h1).path, index.get(h2).path);
    }

    // ── Exact overrides short-name when same string ───────────

    #[test]
    fn test_lookup_prefers_exact_over_short_name() {
        // Signal named "clk" at top level → "top.clk"
        // Query "top.clk" → exact match (not short-name)
        let backend = make_backend();
        let index = SignalIndex::build(&backend);

        let results = index.lookup("top.clk");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].match_kind, "exact");
        assert_eq!(results[0].entry.path, "top.clk");
    }
}

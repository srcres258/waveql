// ── Trace Slice Module ───────────────────────────────────────────
//
// Bounded multi-signal windows for downstream analysis.  A slice captures
// a time range and a set of signals, then provides streaming access to raw
// events without full materialization.

use crate::backend::types::CompactValue;
use crate::backend::types::SignalData;
use crate::backend::WaveformBackend;
use crate::error::WaveqlError;

// ── TimeBound ─────────────────────────────────────────────────────

/// A closed interval `[from, to]` in waveform-native time units.
///
/// Construction validates that `from <= to` — empty or inverted ranges
/// produce a validation error rather than silently returning nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeBound {
    pub from: u64,
    pub to: u64,
}

impl TimeBound {
    /// Create a new bound.  Returns an error when `from > to`.
    pub fn new(from: u64, to: u64) -> Result<Self, WaveqlError> {
        if from > to {
            return Err(WaveqlError::Other(format!(
                "invalid time range: from {} > to {}",
                from, to
            )));
        }
        Ok(TimeBound { from, to })
    }

    /// True when `time` falls within the closed interval.
    #[inline]
    pub fn contains(&self, time: u64) -> bool {
        time >= self.from && time <= self.to
    }
}

// ── TraceSliceRequest ─────────────────────────────────────────────

/// Request to open a bounded window over a waveform backend.
///
/// The request captures everything needed to produce a [`TraceSlice`]:
/// a backend reference, the set of signals to load, and the time bounds.
/// It does not load any data — that happens when [`TraceSliceRequest::build`]
/// is called.
///
/// # Protocol-neutral design
///
/// `TraceSliceRequest` works with any [`WaveformBackend`] — VCD, FST,
/// or future backends.  No format-specific logic lives here.
pub struct TraceSliceRequest<'a> {
    pub backend: &'a dyn WaveformBackend,
    pub signals: Vec<String>,
    pub bounds: TimeBound,
}

impl<'a> TraceSliceRequest<'a> {
    /// Convenience constructor.
    pub fn new(backend: &'a dyn WaveformBackend, signals: Vec<String>, bounds: TimeBound) -> Self {
        TraceSliceRequest {
            backend,
            signals,
            bounds,
        }
    }

    /// Resolve signal patterns (when wildcards are used) and build a
    /// [`TraceSlice`].  This loads signal data via the backend and
    /// validates that every requested signal exists.
    pub fn build(&self) -> Result<TraceSlice<'_>, WaveqlError> {
        let resolved = self.backend.resolve_signals(&self.signals)?;
        if resolved.is_empty() {
            return Err(WaveqlError::Other(
                "no signals match the requested patterns".into(),
            ));
        }

        let mut data: Vec<&SignalData> = Vec::with_capacity(resolved.len());
        for sig in &resolved {
            data.push(self.backend.signal_data(sig)?);
        }

        Ok(TraceSlice {
            signals: resolved,
            data,
            bounds: self.bounds,
        })
    }
}

// ── TraceEvent ────────────────────────────────────────────────────

/// A single event yielded by a [`TraceEventCursor`] in time order.
///
/// Each event represents a signal transition at a specific time within
/// the slice's bounds.  The `value` is the signal's value **after** the
/// transition (i.e., at time `t` the signal became `value`).
#[derive(Debug, Clone)]
pub struct TraceEvent {
    pub time: u64,
    pub signal: String,
    pub value: String,
}

// ── TraceSlice ────────────────────────────────────────────────────

/// A bounded multi-signal window over a waveform backend.
///
/// Constructed from a [`TraceSliceRequest`], the slice holds references to
/// the backend's signal data alongside the resolved signal list and time
/// bounds.  Use the [`event_cursor`](Self::event_cursor) method to stream
/// events in time order without allocating a second event collection.
///
/// # Empty results
///
/// A slice with zero events is valid and won't panic — the cursor simply
/// yields `None` immediately.
pub struct TraceSlice<'a> {
    /// Resolved signal paths (parallel to `data`).
    pub signals: Vec<String>,

    /// References to each signal's data from the backend (parallel to `signals`).
    pub data: Vec<&'a SignalData>,

    /// Time window for this slice.
    pub bounds: TimeBound,
}

impl<'a> TraceSlice<'a> {
    /// Number of signals in this slice.
    #[inline]
    pub fn signal_count(&self) -> usize {
        self.signals.len()
    }

    /// Return a streaming cursor that yields [`TraceEvent`] items in
    /// ascending time order across all signals in this slice.
    ///
    /// The cursor is lazy — no collection is materialized.  Call
    /// `.collect::<Vec<_>>()` on the returned iterator if you need the
    /// full event list.
    pub fn event_cursor(&self) -> TraceEventCursor<'_> {
        // Pre-compute the first valid index for each signal within bounds.
        let start_positions: Vec<usize> = self
            .data
            .iter()
            .map(|d| d.changes.partition_point(|(t, _)| *t < self.bounds.from))
            .collect();

        TraceEventCursor {
            signals: &self.signals,
            data: &self.data,
            positions: start_positions,
            bounds: self.bounds,
        }
    }

    /// Collect all unique time points across signals within bounds, sorted
    /// ascending.  Useful for ASCII rendering and timeline reconstruction.
    pub fn unique_time_points(&self) -> Vec<u64> {
        let mut points: Vec<u64> = Vec::new();
        for d in &self.data {
            for (t, _) in &d.changes {
                if self.bounds.contains(*t) {
                    points.push(*t);
                }
            }
        }
        points.sort();
        points.dedup();
        points
    }

    /// Sample a specific signal at a given time.
    ///
    /// Returns `None` when the time is before the signal's first change
    /// or outside the slice bounds.
    pub fn sample(&self, signal_idx: usize, time: u64) -> Option<&CompactValue> {
        if !self.bounds.contains(time) {
            return None;
        }
        self.data.get(signal_idx).and_then(|d| d.sample(time))
    }

    /// Index of `signal` in the signals list, if present.
    #[inline]
    pub fn signal_index(&self, signal: &str) -> Option<usize> {
        self.signals.iter().position(|s| s == signal)
    }

    /// True when no signals or no data.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.signals.is_empty() || self.data.is_empty()
    }
}

// ── TraceEventCursor ──────────────────────────────────────────────

/// Streaming multi-signal iterator that yields [`TraceEvent`] items in
/// strict ascending time order.
///
/// The cursor implements a **k-way merge** over the per-signal change
/// lists.  At each step it finds the signal with the smallest next time,
/// advances that signal's position, and emits the event.
///
/// Because each signal's change list is already sorted by time, the merge
/// correctness follows from repeatedly picking the global minimum.
pub struct TraceEventCursor<'a> {
    /// Signal paths (parallel to `data` and `positions`).
    signals: &'a [String],

    /// Signal data slices (parallel to `signals` and `positions`).
    data: &'a [&'a SignalData],

    /// Current read position in each signal's `changes` vector.
    positions: Vec<usize>,

    /// Time window — events outside this interval are skipped.
    bounds: TimeBound,
}

impl<'a> Iterator for TraceEventCursor<'a> {
    type Item = TraceEvent;

    fn next(&mut self) -> Option<Self::Item> {
        let n = self.signals.len();
        let mut best_idx: Option<usize> = None;
        let mut best_time: u64 = u64::MAX;

        // ── Find the signal with the smallest next time within bounds ──
        for i in 0..n {
            loop {
                let pos = self.positions[i];
                if pos >= self.data[i].changes.len() {
                    break; // exhausted this signal
                }
                let (t, _) = self.data[i].changes[pos];
                if t < self.bounds.from {
                    // Skip past events before the window start
                    self.positions[i] = pos + 1;
                    continue;
                }
                if t > self.bounds.to {
                    break; // past the window end — no more from this signal
                }
                if t < best_time {
                    best_time = t;
                    best_idx = Some(i);
                }
                break;
            }
        }

        let idx = best_idx?;
        let (t, v) = &self.data[idx].changes[self.positions[idx]];
        self.positions[idx] += 1;

        Some(TraceEvent {
            time: *t,
            signal: self.signals[idx].clone(),
            value: v.as_str().to_string(),
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::capabilities::BackendCapabilities;
    use crate::backend::metadata::WaveformMetadata;
    use crate::backend::types::{FileFormat, SignalInfo, Timescale};

    // ── Mock Backend ──────────────────────────────────────────

    /// A minimal backend that exposes pre-built `SignalData`.
    struct MockSliceBackend {
        metadata: WaveformMetadata,
        signals: Vec<SignalInfo>,
        data: std::collections::HashMap<String, SignalData>,
        capabilities: BackendCapabilities,
    }

    impl MockSliceBackend {
        fn new(spec: Vec<(&str, Vec<(u64, &str)>)>) -> Self {
            let mut signals = Vec::new();
            let mut data = std::collections::HashMap::new();
            for (path, changes) in &spec {
                signals.push(SignalInfo {
                    path: path.to_string(),
                    width: 1,
                });
                let cvs: Vec<(u64, CompactValue)> = changes
                    .iter()
                    .map(|(t, v)| (*t, CompactValue::new(v)))
                    .collect();
                data.insert(path.to_string(), SignalData { changes: cvs });
            }
            let count = signals.len();
            MockSliceBackend {
                metadata: WaveformMetadata {
                    timescale: Timescale::default(),
                    date: None,
                    version: None,
                    signal_count: count,
                    format: FileFormat::Vcd,
                },
                signals,
                data,
                capabilities: BackendCapabilities {
                    supports_lazy_load: true,
                    supports_slice: true,
                    supports_incremental: false,
                    format: FileFormat::Vcd,
                    description: "mock-slice",
                },
            }
        }
    }

    impl WaveformBackend for MockSliceBackend {
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
            self.data
                .get(path)
                .ok_or_else(|| WaveqlError::SignalNotFound(path.to_string()))
        }
    }

    // ── Helpers ────────────────────────────────────────────────

    fn clk_data() -> Vec<(u64, &'static str)> {
        vec![
            (0, "0"),
            (10, "1"),
            (40, "0"),
            (60, "1"),
            (80, "0"),
            (100, "1"),
        ]
    }

    fn en_data() -> Vec<(u64, &'static str)> {
        vec![(0, "0"), (20, "1"), (70, "0")]
    }

    fn data_data() -> Vec<(u64, &'static str)> {
        vec![
            (0, "00000000"),
            (30, "10100011"),
            (50, "01000010"),
            (90, "00000000"),
        ]
    }

    fn make_backend() -> MockSliceBackend {
        MockSliceBackend::new(vec![
            ("top.clk", clk_data()),
            ("top.en", en_data()),
            ("top.data", data_data()),
        ])
    }

    // ── TimeBound ──────────────────────────────────────────────

    #[test]
    fn test_timebound_valid() {
        let b = TimeBound::new(0, 100).unwrap();
        assert_eq!(b.from, 0);
        assert_eq!(b.to, 100);
    }

    #[test]
    fn test_timebound_equal() {
        let b = TimeBound::new(42, 42).unwrap();
        assert!(b.contains(42));
        assert!(!b.contains(41));
        assert!(!b.contains(43));
    }

    #[test]
    fn test_timebound_invalid_inverted() {
        let result = TimeBound::new(100, 0);
        assert!(result.is_err());
    }

    #[test]
    fn test_timebound_contains() {
        let b = TimeBound::new(10, 50).unwrap();
        assert!(b.contains(10));
        assert!(b.contains(25));
        assert!(b.contains(50));
        assert!(!b.contains(0));
        assert!(!b.contains(51));
    }

    // ── TraceSliceRequest ──────────────────────────────────────

    #[test]
    fn test_slice_request_build_basic() {
        let backend = make_backend();
        let bounds = TimeBound::new(0, 100).unwrap();
        let req = TraceSliceRequest::new(&backend, vec!["top.clk".to_string()], bounds);
        let slice = req.build().unwrap();
        assert_eq!(slice.signal_count(), 1);
        assert_eq!(slice.signals[0], "top.clk");
        assert_eq!(slice.bounds, bounds);
    }

    #[test]
    fn test_slice_request_invalid_signal() {
        let backend = make_backend();
        let bounds = TimeBound::new(0, 100).unwrap();
        let req = TraceSliceRequest::new(&backend, vec!["nonexistent".to_string()], bounds);
        assert!(req.build().is_err());
    }

    #[test]
    fn test_slice_request_empty_signals_uses_all() {
        let backend = make_backend();
        let bounds = TimeBound::new(0, 100).unwrap();
        let req = TraceSliceRequest::new(&backend, vec![], bounds);
        let slice = req.build().unwrap();
        assert_eq!(slice.signal_count(), 3);
    }

    // ── TraceEventCursor: bounded events ───────────────────────

    #[test]
    fn test_cursor_bounded_events_stay_within_range() {
        let backend = make_backend();
        let bounds = TimeBound::new(10, 70).unwrap();
        let req = TraceSliceRequest::new(&backend, vec!["top.clk".to_string()], bounds);
        let slice = req.build().unwrap();
        let events: Vec<TraceEvent> = slice.event_cursor().collect();

        // clk changes: (0,0) (10,1) (40,0) (60,1) (80,0) (100,1)
        // Window [10,70]: (10,1) (40,0) (60,1)
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].time, 10);
        assert_eq!(events[1].time, 40);
        assert_eq!(events[2].time, 60);

        for ev in &events {
            assert!(
                bounds.contains(ev.time),
                "event at {} outside [{}, {}]",
                ev.time,
                bounds.from,
                bounds.to
            );
        }
    }

    #[test]
    fn test_cursor_empty_range_returns_no_events() {
        let backend = make_backend();
        // Range where no signal has any changes
        let bounds = TimeBound::new(500, 600).unwrap();
        let req = TraceSliceRequest::new(&backend, vec!["top.clk".to_string()], bounds);
        let slice = req.build().unwrap();
        let events: Vec<TraceEvent> = slice.event_cursor().collect();
        assert!(events.is_empty());
    }

    #[test]
    fn test_cursor_empty_range_does_not_panic() {
        let backend = make_backend();
        let bounds = TimeBound::new(500, 600).unwrap();
        let req = TraceSliceRequest::new(
            &backend,
            vec!["top.clk".to_string(), "top.en".to_string()],
            bounds,
        );
        let slice = req.build().unwrap();
        let _events: Vec<TraceEvent> = slice.event_cursor().collect();
        // Should not panic — just returns empty
    }

    #[test]
    fn test_cursor_multi_signal_time_order() {
        let backend = make_backend();
        let bounds = TimeBound::new(0, 100).unwrap();
        let req = TraceSliceRequest::new(
            &backend,
            vec!["top.clk".to_string(), "top.en".to_string()],
            bounds,
        );
        let slice = req.build().unwrap();
        let events: Vec<TraceEvent> = slice.event_cursor().collect();

        // Verify strict ascending time order
        for w in events.windows(2) {
            assert!(
                w[0].time <= w[1].time,
                "events out of order: {} then {}",
                w[0].time,
                w[1].time
            );
        }
    }

    #[test]
    fn test_cursor_events_are_complete() {
        let backend = make_backend();
        let bounds = TimeBound::new(0, 100).unwrap();
        let req = TraceSliceRequest::new(&backend, vec!["top.clk".to_string()], bounds);
        let slice = req.build().unwrap();
        let events: Vec<TraceEvent> = slice.event_cursor().collect();

        // Expect all 6 clk events within [0,100]
        assert_eq!(events.len(), 6);
        assert_eq!(events[0].time, 0);
        assert_eq!(events[0].value, "0");
        assert_eq!(events[5].time, 100);
        assert_eq!(events[5].value, "1");
    }

    // ── unique_time_points ─────────────────────────────────────

    #[test]
    fn test_unique_time_points_are_sorted_and_deduped() {
        let backend = make_backend();
        let bounds = TimeBound::new(10, 90).unwrap();
        let req = TraceSliceRequest::new(
            &backend,
            vec!["top.clk".to_string(), "top.en".to_string()],
            bounds,
        );
        let slice = req.build().unwrap();
        let points = slice.unique_time_points();

        // clk within [10,90]: 10, 40, 60, 80
        // en  within [10,90]: 20, 70
        // Expected union sorted: 10, 20, 40, 60, 70, 80
        assert_eq!(points, vec![10, 20, 40, 60, 70, 80]);

        // Verify sorted and deduplicated
        for w in points.windows(2) {
            assert!(w[0] < w[1], "points not strictly increasing");
        }
    }

    #[test]
    fn test_unique_time_points_empty_range() {
        let backend = make_backend();
        let bounds = TimeBound::new(500, 600).unwrap();
        let req = TraceSliceRequest::new(&backend, vec!["top.clk".to_string()], bounds);
        let slice = req.build().unwrap();
        let points = slice.unique_time_points();
        assert!(points.is_empty());
    }

    // ── TraceSlice::sample ─────────────────────────────────────

    #[test]
    fn test_slice_sample_within_bounds() {
        let backend = make_backend();
        let bounds = TimeBound::new(10, 70).unwrap();
        let req = TraceSliceRequest::new(&backend, vec!["top.clk".to_string()], bounds);
        let slice = req.build().unwrap();

        // At time 25, clk was last changed to 1 at time 10
        let val = slice.sample(0, 25).unwrap();
        assert_eq!(val.as_str(), "1");

        // At time 50, clk was last changed to 0 at time 40
        let val = slice.sample(0, 50).unwrap();
        assert_eq!(val.as_str(), "0");
    }

    #[test]
    fn test_slice_sample_outside_bounds_returns_none() {
        let backend = make_backend();
        let bounds = TimeBound::new(10, 70).unwrap();
        let req = TraceSliceRequest::new(&backend, vec!["top.clk".to_string()], bounds);
        let slice = req.build().unwrap();

        // Time 5 is before the window
        assert!(slice.sample(0, 5).is_none());

        // Time 80 is after the window
        assert!(slice.sample(0, 80).is_none());
    }

    // ── Edge case: zero-point bound ────────────────────────────

    #[test]
    fn test_cursor_single_time_point() {
        let backend = make_backend();
        // Bound exactly at a single change time
        let bounds = TimeBound::new(40, 40).unwrap();
        let req = TraceSliceRequest::new(&backend, vec!["top.clk".to_string()], bounds);
        let slice = req.build().unwrap();
        let events: Vec<TraceEvent> = slice.event_cursor().collect();

        // clk changes at 40: (40, 0)
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].time, 40);
        assert_eq!(events[0].value, "0");
    }

    // ── Edge case: all events before window ────────────────────

    #[test]
    fn test_cursor_window_after_all_events() {
        let backend = make_backend();
        let bounds = TimeBound::new(500, 600).unwrap();
        let req = TraceSliceRequest::new(&backend, vec!["top.clk".to_string()], bounds);
        let slice = req.build().unwrap();
        let events: Vec<TraceEvent> = slice.event_cursor().collect();
        assert!(events.is_empty());
    }

    // ── Edge case: all events before window start ──────────────

    #[test]
    fn test_cursor_window_before_all_events() {
        let backend = make_backend();
        // No events at negative times (waveforms start at 0), but just test window logic
        let bounds = TimeBound::new(500, 600).unwrap();
        let req = TraceSliceRequest::new(&backend, vec!["top.clk".to_string()], bounds);
        let slice = req.build().unwrap();
        let events: Vec<TraceEvent> = slice.event_cursor().collect();
        assert!(events.is_empty());
    }
}

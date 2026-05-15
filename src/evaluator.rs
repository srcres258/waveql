use crate::error::WaveqlError;
use crate::query::{
    ChangeEvent, ChangesOutput, EdgesOutput, EdgeType, ListOutput, RangeInfo, SampleOutput,
};
use crate::{Query, Waveform};

/// Evaluate a query against a loaded waveform.
pub fn evaluate(
    waveform: &Waveform,
    query: &Query,
    file_name: &str,
) -> Result<String, WaveqlError> {
    match query {
        Query::List => evaluate_list(waveform, file_name),
        Query::Changes { signals, range } => evaluate_changes(waveform, signals, *range),
        Query::Edges {
            signal,
            edge_type,
            range,
        } => evaluate_edges(waveform, signal, *edge_type, *range),
        Query::Sample { signal, at } => evaluate_sample(waveform, signal, *at),
        Query::Ascii { signals, range } => evaluate_ascii(waveform, signals, *range),
    }
}

fn evaluate_list(waveform: &Waveform, file_name: &str) -> Result<String, WaveqlError> {
    let output = ListOutput {
        file: file_name.to_string(),
        format: waveform.file_format.to_string(),
        timescale: waveform.timescale.to_string(),
        total_signals: waveform.signals.len(),
        signals: waveform.signals.clone(),
    };
    Ok(serde_json::to_string_pretty(&output)?)
}

fn evaluate_changes(
    waveform: &Waveform,
    signals: &[String],
    range: crate::query::TimeRange,
) -> Result<String, WaveqlError> {
    let resolved = resolve_signals(waveform, signals)?;
    let mut events: Vec<ChangeEvent> = Vec::new();

    let from = range.from.unwrap_or(0);
    let to = range.to.unwrap_or(u64::MAX);

    for sig in &resolved {
        if let Some(data) = waveform.data.get(sig) {
            for (t, v) in &data.changes {
                if *t >= from && *t <= to {
                    events.push(ChangeEvent {
                        time: *t,
                        signal: sig.clone(),
                        value: v.clone(),
                    });
                }
            }
        }
    }

    events.sort_by(|a, b| a.time.cmp(&b.time).then_with(|| a.signal.cmp(&b.signal)));

    let output = ChangesOutput {
        query_type: "changes".into(),
        signal_count: resolved.len(),
        range: RangeInfo {
            from: range.from,
            to: range.to,
        },
        events,
    };
    Ok(serde_json::to_string_pretty(&output)?)
}

fn evaluate_edges(
    waveform: &Waveform,
    signal: &str,
    edge_type: EdgeType,
    range: crate::query::TimeRange,
) -> Result<String, WaveqlError> {
    let data = waveform
        .data
        .get(signal)
        .ok_or_else(|| WaveqlError::SignalNotFound(signal.to_string()))?;

    let from = range.from.unwrap_or(0);
    let to = range.to.unwrap_or(u64::MAX);

    let filtered: Vec<&(u64, String)> = data
        .changes
        .iter()
        .skip_while(|(t, _)| *t < from)
        .take_while(|(t, _)| *t <= to)
        .collect();

    let mut edges: Vec<u64> = Vec::new();

    for i in 1..filtered.len() {
        let prev_val = &filtered[i - 1].1;
        let curr_val = &filtered[i].1;
        let time = filtered[i].0;

        let prev_high = is_high(prev_val);
        let curr_high = is_high(curr_val);

        match edge_type {
            EdgeType::Rising => {
                if !prev_high && curr_high {
                    edges.push(time);
                }
            }
            EdgeType::Falling => {
                if prev_high && !curr_high {
                    edges.push(time);
                }
            }
            EdgeType::Both => {
                if prev_high != curr_high {
                    edges.push(time);
                }
            }
        }
    }

    let edge_type_str = match edge_type {
        EdgeType::Rising => "rising",
        EdgeType::Falling => "falling",
        EdgeType::Both => "both",
    };

    let output = EdgesOutput {
        signal: signal.to_string(),
        edge_type: edge_type_str.to_string(),
        edge_count: edges.len(),
        edges,
    };
    Ok(serde_json::to_string_pretty(&output)?)
}

fn evaluate_sample(
    waveform: &Waveform,
    signal: &str,
    at: u64,
) -> Result<String, WaveqlError> {
    let data = waveform
        .data
        .get(signal)
        .ok_or_else(|| WaveqlError::SignalNotFound(signal.to_string()))?;

    let value = data.sample(at).map(|s| s.to_string());

    let output = SampleOutput {
        signal: signal.to_string(),
        at,
        value,
    };
    Ok(serde_json::to_string_pretty(&output)?)
}

fn evaluate_ascii(
    waveform: &Waveform,
    signals: &[String],
    range: crate::query::TimeRange,
) -> Result<String, WaveqlError> {
    let resolved = resolve_signals(waveform, signals)?;

    let from = range.from.unwrap_or(0);
    let to = range.to.unwrap_or(u64::MAX);

    // Collect time points: all unique times in range from resolved signals
    let mut time_points: Vec<u64> = Vec::new();
    for sig in &resolved {
        if let Some(data) = waveform.data.get(sig) {
            for (t, _) in &data.changes {
                if *t >= from && *t <= to {
                    time_points.push(*t);
                }
            }
        }
    }
    time_points.sort();
    time_points.dedup();

    // Build ASCII table
    let mut output = String::new();

    // Header
    output.push_str(&format!(
        "{:<10} | {}\n",
        "Time",
        resolved
            .iter()
            .map(|s| format!("{:>width$}", s, width = s.len().max(4)))
            .collect::<Vec<_>>()
            .join(" | ")
    ));
    output.push_str(&format!(
        "{:-<10}-+-{}\n",
        "",
        resolved
            .iter()
            .map(|s| "-".repeat(s.len().max(4)))
            .collect::<Vec<_>>()
            .join("-+-")
    ));

    // Data rows
    for &t in &time_points {
        let time_str = waveform.timescale.format_time(t);
        let values: Vec<String> = resolved
            .iter()
            .map(|sig| {
                waveform
                    .data
                    .get(sig)
                    .and_then(|d| d.sample(t))
                    .map(|v| format_sig_value(v))
                    .unwrap_or_else(|| "??".to_string())
            })
            .collect();

        output.push_str(&format!(
            "{:<10} | {}\n",
            time_str,
            values
                .iter()
                .enumerate()
                .map(|(i, v)| format!(
                    "{:>width$}",
                    v,
                    width = resolved[i].len().max(4)
                ))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }

    Ok(output)
}

// ── Signal Resolution ────────────────────────────────────────────

fn is_high(value: &str) -> bool {
    if value == "1" {
        return true;
    }
    if value == "0" {
        return false;
    }
    // Multi-bit: treat as high if contains any '1' or non-zero hex
    for c in value.chars() {
        match c {
            '1'..='9' | 'A'..='F' | 'a'..='f' => return true,
            _ => {}
        }
    }
    false
}

fn resolve_signals(waveform: &Waveform, patterns: &[String]) -> Result<Vec<String>, WaveqlError> {
    if patterns.is_empty() {
        return Ok(waveform.signals.iter().map(|s| s.path.clone()).collect());
    }

    let mut result = Vec::new();
    for pattern in patterns {
        let matched = wildcard_match(&waveform.signals, pattern);
        if matched.is_empty() {
            return Err(WaveqlError::SignalNotFound(pattern.clone()));
        }
        result.extend(matched);
    }
    let mut seen = std::collections::HashSet::new();
    result.retain(|s| seen.insert(s.clone()));
    Ok(result)
}

fn wildcard_match(signals: &[crate::SignalInfo], pattern: &str) -> Vec<String> {
    if !pattern.contains('*') {
        if signals.iter().any(|s| s.path == pattern) {
            return vec![pattern.to_string()];
        }
        return vec![];
    }

    let (prefix, suffix) = if let Some(pos) = pattern.find('*') {
        (&pattern[..pos], &pattern[pos + 1..])
    } else {
        (pattern, "")
    };

    signals
        .iter()
        .filter(|s| s.path.starts_with(prefix) && s.path.ends_with(suffix))
        .map(|s| s.path.clone())
        .collect()
}

fn format_sig_value(value: &str) -> String {
    // Convert 1-bit values to block characters for ASCII view
    match value {
        "0" => "░".to_string(),
        "1" => "█".to_string(),
        "X" | "x" => "╳".to_string(),
        "Z" | "z" => "Z".to_string(),
        _ => {
            // Truncate long values
            if value.len() > 8 {
                format!("{}..", &value[..6])
            } else {
                value.to_string()
            }
        }
    }
}

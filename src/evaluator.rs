use crate::error::WaveqlError;
use crate::query::{
    ChangeEvent, ChangesOutput, EdgesOutput, EdgeType, ListOutput, RangeInfo, SampleOutput,
};
use crate::{CompactValue, Query, Waveform};

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
    let resolved = waveform.resolve_signals(signals)?;
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
                        value: v.as_str().to_string(),
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

    let filtered: Vec<&(u64, CompactValue)> = data
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

        let prev_high = prev_val.is_high();
        let curr_high = curr_val.is_high();

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

    let value = data.sample(at).map(|cv| cv.as_str().to_string());

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
    let resolved = waveform.resolve_signals(signals)?;

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
                    .map(|v| v.format_ascii())
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



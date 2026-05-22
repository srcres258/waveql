use crate::error::WaveqlError;
use crate::query::{ChangeEvent, TimeRange};
use crate::{Query, Waveform};

/// Render a human-readable text output (plaintext, no JSON).
pub fn render(
    waveform: &Waveform,
    query: &Query,
) -> Result<String, WaveqlError> {
    match query {
        Query::List => render_list_text(waveform),
        Query::Sample { signal, at } => render_sample_text(waveform, signal, *at),
        Query::Edges {
            signal,
            edge_type,
            range,
        } => render_edges_text(waveform, signal, *edge_type, *range),
        Query::Changes { signals, range } => render_changes_text(waveform, signals, *range),
        Query::Ascii { signals, range } => render_ascii_text(waveform, signals, *range),
    }
}

fn render_list_text(waveform: &Waveform) -> Result<String, WaveqlError> {
    let mut out = String::new();
    out.push_str(&format!(
        "File format: {}\nTimescale: {}\nTotal signals: {}\n\n",
        waveform.file_format,
        waveform.timescale,
        waveform.signals.len()
    ));
    out.push_str(&format!(
        "{:<40} {:>6}\n",
        "Signal Path", "Width"
    ));
    out.push_str(&format!(
        "{:-<40} {:-<6}\n",
        "", ""
    ));
    for sig in &waveform.signals {
        out.push_str(&format!("{:<40} {:>6}\n", sig.path, sig.width));
    }
    Ok(out)
}

fn render_sample_text(
    waveform: &Waveform,
    signal: &str,
    at: u64,
) -> Result<String, WaveqlError> {
    let data = waveform
        .data
        .get(signal)
        .ok_or_else(|| WaveqlError::SignalNotFound(signal.to_string()))?;
    let value = data.sample(at).map(|cv| cv.as_str()).unwrap_or("(unknown)");
    Ok(format!(
        "Signal: {}\nAt: {}\nValue: {}\n",
        signal, at, value
    ))
}

fn render_edges_text(
    waveform: &Waveform,
    signal: &str,
    edge_type: crate::query::EdgeType,
    range: TimeRange,
) -> Result<String, WaveqlError> {
    // Reuse evaluator logic
    let json = crate::evaluator::evaluate(
        waveform,
        &Query::Edges {
            signal: signal.to_string(),
            edge_type,
            range,
        },
        "",
    )?;

    let v: serde_json::Value = serde_json::from_str(&json)?;
    let edge_type_str = v["edge_type"].as_str().unwrap_or("?");
    let edge_count = v["edge_count"].as_u64().unwrap_or(0);
    let edges: Vec<u64> = v["edges"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_u64()).collect())
        .unwrap_or_default();

    let mut out = String::new();
    out.push_str(&format!(
        "Signal: {}\nEdge type: {}\nEdge count: {}\n",
        signal, edge_type_str, edge_count
    ));
    if !edges.is_empty() {
        out.push_str("Edges:\n");
        for t in edges {
            out.push_str(&format!("  {}\n", waveform.timescale.format_time(t)));
        }
    }
    Ok(out)
}

fn render_changes_text(
    waveform: &Waveform,
    signals: &[String],
    range: TimeRange,
) -> Result<String, WaveqlError> {
    let json = crate::evaluator::evaluate(
        waveform,
        &Query::Changes {
            signals: signals.to_vec(),
            range,
        },
        "",
    )?;

    let v: serde_json::Value = serde_json::from_str(&json)?;
    let events: Vec<ChangeEvent> = v["events"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|e| ChangeEvent {
                    time: e["time"].as_u64().unwrap_or(0),
                    signal: e["signal"].as_str().unwrap_or("?").to_string(),
                    value: e["value"].as_str().unwrap_or("?").to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    let mut out = String::new();
    out.push_str(&format!(
        "{:<10} {:<30} {}\n",
        "Time", "Signal", "Value"
    ));
    out.push_str(&format!("{:-<10} {:-<30} {}\n", "", "", ""));
    for ev in events {
        out.push_str(&format!(
            "{:<10} {:<30} {}\n",
            waveform.timescale.format_time(ev.time),
            ev.signal,
            ev.value
        ));
    }
    Ok(out)
}

fn render_ascii_text(
    waveform: &Waveform,
    signals: &[String],
    range: TimeRange,
) -> Result<String, WaveqlError> {
    crate::evaluator::evaluate(
        waveform,
        &Query::Ascii {
            signals: signals.to_vec(),
            range,
        },
        "",
    )
}

use crate::error::WaveqlError;
use crate::query::TimeRange;
use crate::{Query, Waveform};

/// Render a tabular output (like text but with | delimiters, suitable for CSV).
pub fn render(
    waveform: &Waveform,
    query: &Query,
) -> Result<String, WaveqlError> {
    match query {
        Query::List => render_list_table(waveform),
        Query::Sample { signal, at } => render_sample_table(waveform, signal, *at),
        Query::Edges {
            signal,
            edge_type,
            range,
        } => render_edges_table(waveform, signal, *edge_type, *range),
        Query::Changes { signals, range } => render_changes_table(waveform, signals, *range),
        Query::Ascii { signals, range } => render_ascii_table(waveform, signals, *range),
    }
}

fn render_list_table(waveform: &Waveform) -> Result<String, WaveqlError> {
    let mut out = String::new();
    out.push_str("path|width\n");
    for sig in &waveform.signals {
        out.push_str(&format!("{}|{}\n", sig.path, sig.width));
    }
    Ok(out)
}

fn render_sample_table(
    waveform: &Waveform,
    signal: &str,
    at: u64,
) -> Result<String, WaveqlError> {
    let data = waveform
        .data
        .get(signal)
        .ok_or_else(|| WaveqlError::SignalNotFound(signal.to_string()))?;
    let value = data.sample(at).map(|cv| cv.as_str()).unwrap_or("?");
    Ok(format!("{}|{}|{}\n", signal, at, value))
}

fn render_edges_table(
    waveform: &Waveform,
    signal: &str,
    edge_type: crate::query::EdgeType,
    range: TimeRange,
) -> Result<String, WaveqlError> {
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
    let edges: Vec<u64> = v["edges"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_u64()).collect())
        .unwrap_or_default();

    let mut out = String::new();
    out.push_str("time\n");
    for t in edges {
        out.push_str(&format!("{}\n", t));
    }
    Ok(out)
}

fn render_changes_table(
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
    let events: Vec<serde_json::Value> = v["events"].as_array().cloned().unwrap_or_default();

    let mut out = String::new();
    out.push_str("time|signal|value\n");
    for e in events {
        let time = e["time"].as_u64().unwrap_or(0);
        let sig = e["signal"].as_str().unwrap_or("?");
        let val = e["value"].as_str().unwrap_or("?");
        out.push_str(&format!("{}|{}|{}\n", time, sig, val));
    }
    Ok(out)
}

fn render_ascii_table(
    waveform: &Waveform,
    signals: &[String],
    range: TimeRange,
) -> Result<String, WaveqlError> {
    crate::output::text::render(
        waveform,
        &Query::Ascii {
            signals: signals.to_vec(),
            range,
        },
    )
}

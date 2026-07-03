use crate::backend::WaveformBackend;
use crate::error::WaveqlError;
use crate::query::{ChangesOutput, EdgesOutput, EventsOutput, TimeRange};
use crate::report::Report;
use crate::Query;

pub fn render(waveform: &dyn WaveformBackend, query: &Query) -> Result<String, WaveqlError> {
    match query {
        Query::List => render_list_text(waveform),
        Query::Sample { signal, at } => render_sample_text(waveform, signal, *at),
        Query::Ascii { signals, range } => render_ascii_text(waveform, signals, *range),
        Query::Edges {
            signal,
            edge_type,
            range,
        } => {
            let report = crate::evaluator::evaluate(
                waveform,
                &Query::Edges {
                    signal: signal.clone(),
                    edge_type: *edge_type,
                    range: *range,
                },
                "",
            )?;
            if let Report::Edges(output) = &report {
                render_edges_text(waveform, output)
            } else {
                Ok("".into())
            }
        }
        Query::Changes { signals, range } => {
            let report = crate::evaluator::evaluate(
                waveform,
                &Query::Changes {
                    signals: signals.clone(),
                    range: *range,
                },
                "",
            )?;
            if let Report::Changes(output) = &report {
                render_changes_text(waveform, output)
            } else {
                Ok("".into())
            }
        }
        Query::Events { derivation, range } => {
            let report = crate::evaluator::evaluate(
                waveform,
                &Query::Events {
                    derivation: derivation.clone(),
                    range: *range,
                },
                "",
            )?;
            if let Report::Events(output) = &report {
                render_events_text(waveform, output)
            } else {
                Ok("".into())
            }
        }
        Query::Protocols => {
            Ok("Protocols query — use --format json or call from Session.\n".into())
        }
        Query::Bind(_) => Ok("Bind query — use --format json or call from Session.\n".into()),
        Query::Analyze(_) => Ok("Analyze query — use --format json or call from Session.\n".into()),
    }
}

fn render_list_text(waveform: &dyn WaveformBackend) -> Result<String, WaveqlError> {
    let metadata = waveform.metadata();
    let mut out = String::new();
    out.push_str(&format!(
        "File format: {}\nTimescale: {}\nTotal signals: {}\n\n",
        metadata.format, metadata.timescale, metadata.signal_count,
    ));
    out.push_str(&format!("{:<40} {:>6}\n", "Signal Path", "Width"));
    out.push_str(&format!("{:-<40} {:-<6}\n", "", ""));
    for sig in waveform.signal_iter() {
        out.push_str(&format!("{:<40} {:>6}\n", sig.path, sig.width));
    }
    Ok(out)
}

fn render_sample_text(
    waveform: &dyn WaveformBackend,
    signal: &str,
    at: u64,
) -> Result<String, WaveqlError> {
    let data = waveform.signal_data(signal)?;
    let value = data.sample(at).map(|cv| cv.as_str()).unwrap_or("(unknown)");
    Ok(format!(
        "Signal: {}\nAt: {}\nValue: {}\n",
        signal, at, value
    ))
}

fn render_edges_text(
    waveform: &dyn WaveformBackend,
    output: &EdgesOutput,
) -> Result<String, WaveqlError> {
    let mut out = String::new();
    out.push_str(&format!(
        "Signal: {}\nEdge type: {}\nEdge count: {}\n",
        output.signal, output.edge_type, output.edge_count
    ));
    if !output.edges.is_empty() {
        out.push_str("Edges:\n");
        for &t in &output.edges {
            out.push_str(&format!("  {}\n", waveform.timescale().format_time(t)));
        }
    }
    Ok(out)
}

fn render_changes_text(
    waveform: &dyn WaveformBackend,
    output: &ChangesOutput,
) -> Result<String, WaveqlError> {
    let mut out = String::new();
    out.push_str(&format!("{:<10} {:<30} {}\n", "Time", "Signal", "Value"));
    out.push_str(&format!("{:-<10} {:-<30} {}\n", "", "", ""));
    for ev in &output.events {
        out.push_str(&format!(
            "{:<10} {:<30} {}\n",
            waveform.timescale().format_time(ev.time),
            ev.signal,
            ev.value
        ));
    }
    Ok(out)
}

fn render_ascii_text(
    waveform: &dyn WaveformBackend,
    signals: &[String],
    range: TimeRange,
) -> Result<String, WaveqlError> {
    let report = crate::evaluator::evaluate(
        waveform,
        &Query::Ascii {
            signals: signals.to_vec(),
            range,
        },
        "",
    )?;
    if let Report::Ascii(s) = &report {
        Ok(s.clone())
    } else {
        Ok("".into())
    }
}

fn render_events_text(
    waveform: &dyn WaveformBackend,
    output: &EventsOutput,
) -> Result<String, WaveqlError> {
    let mut out = String::new();
    out.push_str(&format!(
        "Derivation: {}\nEvent count: {}\n\n",
        output.derivation, output.event_count
    ));

    if output.events.is_empty() {
        out.push_str("(no events)\n");
        return Ok(out);
    }

    for e in &output.events {
        use crate::query::DerivedEventOutput;
        match e {
            DerivedEventOutput::Edge {
                time,
                signal,
                polarity,
                prev_value,
                next_value,
            } => {
                out.push_str(&format!(
                    "  [{:<10}] {:?} edge on {}: {} → {}\n",
                    waveform.timescale().format_time(*time),
                    polarity,
                    signal,
                    prev_value,
                    next_value,
                ));
            }
            DerivedEventOutput::Handshake {
                time,
                phase,
                signal_a,
                signal_b,
            } => {
                out.push_str(&format!(
                    "  [{:<10}] {} handshake: {} / {}\n",
                    waveform.timescale().format_time(*time),
                    phase,
                    signal_a,
                    signal_b,
                ));
            }
            DerivedEventOutput::Stall {
                signal,
                value,
                duration,
                ..
            } => {
                out.push_str(&format!(
                    "  {} stalled at {} for {} units\n",
                    signal, value, duration,
                ));
            }
            DerivedEventOutput::Timeout { description, .. } => {
                out.push_str(&format!("  TIMEOUT: {}\n", description));
            }
            DerivedEventOutput::StateTransition {
                time,
                signal,
                from,
                to,
            } => {
                out.push_str(&format!(
                    "  [{:<10}] {}: {} → {}\n",
                    waveform.timescale().format_time(*time),
                    signal,
                    from,
                    to,
                ));
            }
            DerivedEventOutput::SampleOnEdge {
                time,
                clock,
                target,
                value,
                ..
            } => {
                out.push_str(&format!(
                    "  [{:<10}] sampled {} on edge of {} → {}\n",
                    waveform.timescale().format_time(*time),
                    target,
                    clock,
                    value,
                ));
            }
        }
    }
    Ok(out)
}

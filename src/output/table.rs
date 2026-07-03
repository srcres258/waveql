use crate::backend::WaveformBackend;
use crate::error::WaveqlError;
use crate::query::{ChangesOutput, EdgesOutput, EventsOutput, TimeRange};
use crate::report::Report;
use crate::Query;

pub fn render(waveform: &dyn WaveformBackend, query: &Query) -> Result<String, WaveqlError> {
    match query {
        Query::List => render_list_table(waveform),
        Query::Sample { signal, at } => render_sample_table(waveform, signal, *at),
        Query::Ascii { signals, range } => render_ascii_table(waveform, signals, *range),
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
                render_edges_table(output)
            } else {
                Ok("time|\n".into())
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
                render_changes_table(output)
            } else {
                Ok("time|signal|value|\n".into())
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
                render_events_table(output)
            } else {
                Ok("kind|time|signal|detail|\n".into())
            }
        }
        Query::Protocols => Ok("protocol||\n".into()),
        Query::Bind(_) => Ok("protocol|bind_result|||\n".into()),
        Query::Analyze(_) => Ok("protocol|verdict|||\n".into()),
    }
}

fn render_list_table(waveform: &dyn WaveformBackend) -> Result<String, WaveqlError> {
    let mut out = String::new();
    out.push_str("path|width\n");
    for sig in waveform.signal_iter() {
        out.push_str(&format!("{}|{}\n", sig.path, sig.width));
    }
    Ok(out)
}

fn render_sample_table(
    waveform: &dyn WaveformBackend,
    signal: &str,
    at: u64,
) -> Result<String, WaveqlError> {
    let data = waveform.signal_data(signal)?;
    let value = data.sample(at).map(|cv| cv.as_str()).unwrap_or("?");
    Ok(format!("{}|{}|{}\n", signal, at, value))
}

fn render_edges_table(output: &EdgesOutput) -> Result<String, WaveqlError> {
    let mut out = String::new();
    out.push_str("time\n");
    for &t in &output.edges {
        out.push_str(&format!("{}\n", t));
    }
    Ok(out)
}

fn render_changes_table(output: &ChangesOutput) -> Result<String, WaveqlError> {
    let mut out = String::new();
    out.push_str("time|signal|value\n");
    for e in &output.events {
        out.push_str(&format!("{}|{}|{}\n", e.time, e.signal, e.value));
    }
    Ok(out)
}

fn render_ascii_table(
    waveform: &dyn WaveformBackend,
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

fn render_events_table(output: &EventsOutput) -> Result<String, WaveqlError> {
    let mut out = String::new();
    out.push_str("kind|time|signal|detail\n");
    for e in &output.events {
        use crate::query::DerivedEventOutput;
        let (kind, time, signal, detail) = match e {
            DerivedEventOutput::Edge {
                time: t,
                signal: s,
                prev_value,
                next_value,
                ..
            } => (
                "edge",
                *t,
                s.as_str(),
                format!("{}→{}", prev_value, next_value),
            ),
            DerivedEventOutput::Handshake {
                time: t,
                phase,
                signal_a,
                ..
            } => ("handshake", *t, signal_a.as_str(), phase.clone()),
            DerivedEventOutput::Stall {
                signal: s,
                value,
                since_time,
                duration,
                ..
            } => (
                "stall",
                *since_time,
                s.as_str(),
                format!("dur={} val={}", duration, value),
            ),
            DerivedEventOutput::Timeout {
                description,
                deadline,
                ..
            } => ("timeout", *deadline, "?", description.clone()),
            DerivedEventOutput::StateTransition {
                time: t,
                signal: s,
                from,
                to,
            } => (
                "state_transition",
                *t,
                s.as_str(),
                format!("{}→{}", from, to),
            ),
            DerivedEventOutput::SampleOnEdge {
                time: t,
                target,
                value,
                ..
            } => ("sample_on_edge", *t, target.as_str(), value.clone()),
        };
        out.push_str(&format!("{}|{}|{}|{}\n", kind, time, signal, detail));
    }
    Ok(out)
}

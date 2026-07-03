use crate::backend::WaveformBackend;
use crate::error::WaveqlError;
use crate::events::{derive_events, DerivationRequest, DerivedEvent, EdgePolarity};
use crate::protocol::ProtocolCatalog;
use crate::query::{
    ChangeEvent, ChangesOutput, DerivationKind, DerivedEventOutput, EdgeType, EdgesOutput,
    EventsOutput, ListOutput, ProtocolSchemaInfo, ProtocolsOutput, RangeInfo, SampleOutput,
};
use crate::report::Report;
use crate::trace::{TimeBound, TraceSliceRequest};
use crate::Query;

pub fn evaluate(
    waveform: &dyn WaveformBackend,
    query: &Query,
    file_name: &str,
) -> Result<Report, WaveqlError> {
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
        Query::Events { derivation, range } => evaluate_events(waveform, derivation, *range),
        Query::Protocols => Err(WaveqlError::Other(
            "Protocols query must be handled by Session, not evaluator".into(),
        )),
        Query::Bind(_) => Err(WaveqlError::Other(
            "Bind query must be handled by Session, not evaluator".into(),
        )),
        Query::Analyze(_) => Err(WaveqlError::Other(
            "Analyze query must be handled by Session, not evaluator".into(),
        )),
    }
}

fn evaluate_list(waveform: &dyn WaveformBackend, file_name: &str) -> Result<Report, WaveqlError> {
    let metadata = waveform.metadata();
    let signals: Vec<crate::backend::types::SignalInfo> = waveform.signal_iter().cloned().collect();
    Ok(Report::List(ListOutput {
        file: file_name.to_string(),
        format: metadata.format.to_string(),
        timescale: metadata.timescale.to_string(),
        total_signals: metadata.signal_count,
        signals,
    }))
}

fn evaluate_changes(
    waveform: &dyn WaveformBackend,
    signals: &[String],
    range: crate::query::TimeRange,
) -> Result<Report, WaveqlError> {
    let from = range.from.unwrap_or(0);
    let to = range.to.unwrap_or(u64::MAX);
    let bounds = TimeBound::new(from, to)?;

    let request = TraceSliceRequest::new(waveform, signals.to_vec(), bounds);
    let slice = request.build()?;

    let resolved_count = slice.signal_count();

    let mut events: Vec<ChangeEvent> = slice
        .event_cursor()
        .map(|ev| ChangeEvent {
            time: ev.time,
            signal: ev.signal,
            value: ev.value,
        })
        .collect();

    events.sort_by(|a, b| a.time.cmp(&b.time).then_with(|| a.signal.cmp(&b.signal)));

    Ok(Report::Changes(ChangesOutput {
        query_type: "changes".into(),
        signal_count: resolved_count,
        range: RangeInfo {
            from: range.from,
            to: range.to,
        },
        events,
    }))
}

fn evaluate_edges(
    waveform: &dyn WaveformBackend,
    signal: &str,
    edge_type: EdgeType,
    range: crate::query::TimeRange,
) -> Result<Report, WaveqlError> {
    let from = range.from.unwrap_or(0);
    let to = range.to.unwrap_or(u64::MAX);
    let bounds = TimeBound::new(from, to)?;

    let request = TraceSliceRequest::new(waveform, vec![signal.to_string()], bounds);
    let slice = request.build()?;

    let data = slice.data.first().expect("slice has at least one signal");

    let filtered: Vec<&(u64, crate::backend::types::CompactValue)> = data
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

    Ok(Report::Edges(EdgesOutput {
        signal: signal.to_string(),
        edge_type: edge_type_str.to_string(),
        edge_count: edges.len(),
        edges,
    }))
}

fn evaluate_sample(
    waveform: &dyn WaveformBackend,
    signal: &str,
    at: u64,
) -> Result<Report, WaveqlError> {
    let data = waveform.signal_data(signal)?;

    let value = data.sample(at).map(|cv| cv.as_str().to_string());

    Ok(Report::Sample(SampleOutput {
        signal: signal.to_string(),
        at,
        value,
    }))
}

fn evaluate_ascii(
    waveform: &dyn WaveformBackend,
    signals: &[String],
    range: crate::query::TimeRange,
) -> Result<Report, WaveqlError> {
    let from = range.from.unwrap_or(0);
    let to = range.to.unwrap_or(u64::MAX);
    let bounds = TimeBound::new(from, to)?;

    let request = TraceSliceRequest::new(waveform, signals.to_vec(), bounds);
    let slice = request.build()?;

    let resolved = &slice.signals;
    let time_points = slice.unique_time_points();

    let mut output = String::new();

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

    for &t in &time_points {
        let time_str = waveform.timescale().format_time(t);
        let values: Vec<String> = resolved
            .iter()
            .enumerate()
            .map(|(i, _sig)| {
                slice
                    .sample(i, t)
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
                .map(|(i, v)| format!("{:>width$}", v, width = resolved[i].len().max(4)))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }

    Ok(Report::Ascii(output))
}

fn evaluate_events(
    waveform: &dyn WaveformBackend,
    derivation: &DerivationKind,
    range: crate::query::TimeRange,
) -> Result<Report, WaveqlError> {
    let from = range.from.unwrap_or(0);
    let to = range.to.unwrap_or(u64::MAX);
    let bounds = TimeBound::new(from, to)?;

    let (signals, request, derivation_name) = match derivation {
        DerivationKind::Edges { signals, polarity } => {
            let pol = match polarity {
                crate::query::EdgePolarity::Rise => EdgePolarity::Rise,
                crate::query::EdgePolarity::Fall => EdgePolarity::Fall,
                crate::query::EdgePolarity::Toggle => EdgePolarity::Toggle,
            };
            (
                signals.clone(),
                DerivationRequest::Edges {
                    signals: signals.clone(),
                    polarity: pol,
                },
                "edges",
            )
        }
        DerivationKind::Handshake { signal_a, signal_b } => (
            vec![signal_a.clone(), signal_b.clone()],
            DerivationRequest::Handshake {
                signal_a: signal_a.clone(),
                signal_b: signal_b.clone(),
            },
            "handshake",
        ),
        DerivationKind::Stalls {
            signals,
            min_duration,
        } => (
            signals.clone(),
            DerivationRequest::Stalls {
                signals: signals.clone(),
                min_duration: *min_duration,
            },
            "stalls",
        ),
        DerivationKind::StateTransitions { signal } => (
            vec![signal.clone()],
            DerivationRequest::StateTransitions {
                signal: signal.clone(),
            },
            "state_transitions",
        ),
    };

    let slice_request = TraceSliceRequest::new(waveform, signals, bounds);
    let slice = slice_request.build()?;

    let derived = derive_events(&slice, &request);

    let event_outputs: Vec<DerivedEventOutput> = derived
        .iter()
        .map(|ev| match ev {
            DerivedEvent::Edge {
                time,
                signal,
                polarity,
                prev_value,
                next_value,
                ..
            } => DerivedEventOutput::Edge {
                time: *time,
                signal: signal.clone(),
                polarity: format!("{:?}", polarity).to_lowercase(),
                prev_value: prev_value.clone(),
                next_value: next_value.clone(),
            },
            DerivedEvent::Handshake {
                time,
                phase,
                signal_a,
                signal_b,
                ..
            } => DerivedEventOutput::Handshake {
                time: *time,
                phase: format!("{:?}", phase).to_lowercase(),
                signal_a: signal_a.clone(),
                signal_b: signal_b.clone(),
            },
            DerivedEvent::Stall {
                signal,
                value,
                since_time,
                duration,
                ..
            } => DerivedEventOutput::Stall {
                signal: signal.clone(),
                value: value.clone(),
                since_time: *since_time,
                duration: *duration,
            },
            DerivedEvent::Timeout {
                description,
                deadline,
                last_event_time,
                signal,
                ..
            } => DerivedEventOutput::Timeout {
                description: description.clone(),
                deadline: *deadline,
                last_event_time: *last_event_time,
                signal: signal.clone(),
            },
            DerivedEvent::StateTransition {
                time,
                signal,
                from_value,
                to_value,
                ..
            } => DerivedEventOutput::StateTransition {
                time: *time,
                signal: signal.clone(),
                from: from_value.clone(),
                to: to_value.clone(),
            },
            DerivedEvent::SampleOnEdge {
                time,
                clock,
                target,
                target_value,
                edge_polarity,
                ..
            } => DerivedEventOutput::SampleOnEdge {
                time: *time,
                clock: clock.clone(),
                target: target.clone(),
                value: target_value.clone(),
                edge: format!("{:?}", edge_polarity).to_lowercase(),
            },
        })
        .collect();

    Ok(Report::Events(EventsOutput {
        query_type: "events".into(),
        derivation: derivation_name.into(),
        event_count: event_outputs.len(),
        range: RangeInfo {
            from: range.from,
            to: range.to,
        },
        events: event_outputs,
    }))
}

pub fn evaluate_protocols(catalog: &ProtocolCatalog) -> Result<ProtocolsOutput, WaveqlError> {
    let meta = catalog.discovery_metadata();
    Ok(ProtocolsOutput {
        protocols: meta
            .iter()
            .map(|m| ProtocolSchemaInfo {
                name: m.name.clone(),
                description: m.description.clone(),
                required_role_count: m.required_role_count,
                optional_role_count: m.optional_role_count,
                required_roles: m.required_roles.clone(),
                optional_roles: m.optional_roles.clone(),
            })
            .collect(),
    })
}

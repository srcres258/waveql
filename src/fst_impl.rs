use crate::backend::capabilities::BackendCapabilities;
use crate::backend::metadata::WaveformMetadata;
use crate::backend::types::{CompactValue, FileFormat, SignalInfo, TimeUnit, Timescale};
use crate::error::WaveqlError;
use crate::{LazyLoader, Waveform};
use std::cell::RefCell;
use std::collections::HashMap;

pub fn parse_fst(file_path: &str) -> Result<Waveform, WaveqlError> {
    let waves = wellen::simple::read(file_path)?;

    let hierarchy = waves.hierarchy();
    let time_table = waves.time_table().to_vec();

    let timescale = hierarchy
        .timescale()
        .map(|ts| Timescale {
            magnitude: ts.factor as u64,
            unit: convert_wellen_timeunit(&ts.unit),
        })
        .unwrap_or_default();

    let mut signals: Vec<SignalInfo> = Vec::new();
    let mut sig_refs: HashMap<String, wellen::SignalRef> = HashMap::new();

    for var in hierarchy.iter_vars() {
        let path = var.full_name(hierarchy);
        let width = var.length().unwrap_or(1);
        let sig_ref = var.signal_ref();

        sig_refs.insert(path.clone(), sig_ref);
        signals.push(SignalInfo { path, width });
    }

    signals.sort_by(|a, b| a.path.cmp(&b.path));
    let signal_count = signals.len();

    let metadata = WaveformMetadata {
        timescale: timescale.clone(),
        date: None,
        version: None,
        signal_count,
        format: FileFormat::Fst,
    };

    let capabilities = BackendCapabilities {
        supports_lazy_load: true,
        supports_slice: true,
        supports_incremental: true,
        format: FileFormat::Fst,
        description: "FST parser — wellen-backed with indexed random access",
    };

    Ok(Waveform {
        metadata,
        signals,
        data: HashMap::new(),
        capabilities,
        lazy: Some(LazyLoader::Fst {
            waves: Box::new(RefCell::new(waves)),
            time_table,
            sig_refs,
        }),
    })
}

fn convert_wellen_timeunit(unit: &wellen::TimescaleUnit) -> TimeUnit {
    match unit {
        wellen::TimescaleUnit::Seconds => TimeUnit::S,
        wellen::TimescaleUnit::MilliSeconds => TimeUnit::Ms,
        wellen::TimescaleUnit::MicroSeconds => TimeUnit::Us,
        wellen::TimescaleUnit::NanoSeconds => TimeUnit::Ns,
        wellen::TimescaleUnit::PicoSeconds => TimeUnit::Ps,
        wellen::TimescaleUnit::FemtoSeconds => TimeUnit::Fs,
        _ => TimeUnit::Ns,
    }
}

pub(crate) fn format_signal_value(val: &wellen::SignalValue) -> CompactValue {
    if val.is_event() {
        return CompactValue::new("EVENT");
    }
    if let Some(bit_str) = val.to_bit_string() {
        return CompactValue::new(&bit_str);
    }
    if let Some(bits) = val.bits() {
        return CompactValue::new(&format!("{:X}", bits));
    }
    CompactValue::Bit(b'?')
}

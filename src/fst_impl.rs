use crate::error::WaveqlError;
use crate::{FileFormat, SignalData, SignalInfo, TimeUnit, Timescale, Waveform};
use std::collections::HashMap;

pub fn parse_fst(file_path: &str) -> Result<Waveform, WaveqlError> {
    let mut waves = wellen::simple::read(file_path)?;

    // Step 1: Collect signal refs (immutable borrow of waves)
    let sig_refs: Vec<wellen::SignalRef> = {
        let hierarchy = waves.hierarchy();
        hierarchy.iter_vars().map(|v| v.signal_ref()).collect()
    };

    // Step 2: Load signals (mutable borrow)
    waves.load_signals(&sig_refs);

    // Step 3: Extract data (immutable borrow again)
    let hierarchy = waves.hierarchy();
    let time_table = waves.time_table();

    let timescale = hierarchy
        .timescale()
        .map(|ts| Timescale {
            magnitude: ts.factor as u64,
            unit: convert_wellen_timeunit(&ts.unit),
        })
        .unwrap_or_default();

    let mut signals: Vec<SignalInfo> = Vec::new();
    let mut data: HashMap<String, SignalData> = HashMap::new();

    for var in hierarchy.iter_vars() {
        let path = var.full_name(&hierarchy);
        let width = var.length().unwrap_or(1);

        signals.push(SignalInfo {
            path: path.clone(),
            width,
        });

        let sig_ref = var.signal_ref();
        if let Some(signal) = waves.get_signal(sig_ref) {
            let mut changes: Vec<(u64, String)> = Vec::new();
            for (time_idx, val) in signal.iter_changes() {
                let time = time_table[time_idx as usize];
                let val_str = format_signal_value(&val);
                changes.push((time, val_str));
            }
            data.insert(path, SignalData { changes });
        }
    }

    signals.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(Waveform {
        timescale,
        signals,
        data,
        file_format: FileFormat::Fst,
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

fn format_signal_value(val: &wellen::SignalValue) -> String {
    if val.is_event() {
        return "EVENT".to_string();
    }
    if let Some(bit_str) = val.to_bit_string() {
        return bit_str;
    }
    if let Some(bits) = val.bits() {
        return format!("{:X}", bits);
    }
    "?".to_string()
}

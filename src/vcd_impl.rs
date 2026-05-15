use crate::error::WaveqlError;
use crate::{FileFormat, SignalData, SignalInfo, Timescale, TimeUnit, Waveform};
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;

pub fn parse_vcd(file_path: &str) -> Result<Waveform, WaveqlError> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let mut parser = vcd::Parser::new(reader);

    let mut timescale = Timescale::default();
    let mut scope_stack: Vec<String> = Vec::new();
    let mut signal_map: HashMap<String, SignalInfo> = HashMap::new(); // id_code -> info
    let mut signal_changes: HashMap<String, Vec<(u64, String)>> = HashMap::new();
    let mut current_time: u64 = 0;

    loop {
        let cmd = match parser.next() {
            Some(Ok(cmd)) => cmd,
            Some(Err(e)) => return Err(WaveqlError::VcdParse(format!("{e}"))),
            None => break,
        };

        match cmd {
            vcd::Command::Timescale(magnitude, unit) => {
                timescale = Timescale {
                    magnitude: magnitude as u64,
                    unit: convert_timeunit(unit),
                };
            }
            vcd::Command::ScopeDef(scope_type, name) => {
                scope_stack.push(format!("{}:{}", scope_type_str(&scope_type), name));
            }
            vcd::Command::Upscope => {
                scope_stack.pop();
            }
            vcd::Command::VarDef(_var_type, size, code, name, _ref_idx) => {
                let mut full_path = scope_stack.clone();
                full_path.push(name);
                let path = clean_path(&full_path.join("."));
                let code_str = code.to_string();
                signal_map.insert(
                    code_str.clone(),
                    SignalInfo {
                        path: path.clone(),
                        width: size,
                    },
                );
                signal_changes.entry(path).or_default();
            }
            vcd::Command::Timestamp(time) => {
                current_time = time;
            }
            vcd::Command::ChangeScalar(code, value) => {
                let code_str = code.to_string();
                if let Some(info) = signal_map.get(&code_str) {
                    let val_str = match &value {
                        vcd::Value::V0 => "0",
                        vcd::Value::V1 => "1",
                        vcd::Value::X => "X",
                        vcd::Value::Z => "Z",
                    };
                    if let Some(changes) = signal_changes.get_mut(&info.path) {
                        changes.push((current_time, val_str.to_string()));
                    }
                }
            }
            vcd::Command::ChangeVector(code, values) => {
                let code_str = code.to_string();
                if let Some(info) = signal_map.get(&code_str) {
                    let val_str = values
                        .iter()
                        .map(value_to_char)
                        .collect::<String>();
                    if let Some(changes) = signal_changes.get_mut(&info.path) {
                        changes.push((current_time, val_str));
                    }
                }
            }
            _ => {} // skip other commands
        }
    }

    let mut signals: Vec<SignalInfo> = signal_map.into_values().collect();
    signals.sort_by(|a, b| a.path.cmp(&b.path));

    let mut data: HashMap<String, SignalData> = HashMap::new();
    for (path, mut changes) in signal_changes {
        changes.sort_by_key(|(t, _)| *t);
        data.insert(path, SignalData { changes });
    }

    Ok(Waveform {
        timescale,
        signals,
        data,
        file_format: FileFormat::Vcd,
    })
}

fn convert_timeunit(unit: vcd::TimescaleUnit) -> TimeUnit {
    match unit {
        vcd::TimescaleUnit::S => TimeUnit::S,
        vcd::TimescaleUnit::MS => TimeUnit::Ms,
        vcd::TimescaleUnit::US => TimeUnit::Us,
        vcd::TimescaleUnit::NS => TimeUnit::Ns,
        vcd::TimescaleUnit::PS => TimeUnit::Ps,
        vcd::TimescaleUnit::FS => TimeUnit::Fs,
    }
}

fn scope_type_str(st: &vcd::ScopeType) -> &'static str {
    match st {
        vcd::ScopeType::Module => "module",
        vcd::ScopeType::Task => "task",
        vcd::ScopeType::Function => "function",
        vcd::ScopeType::Begin => "begin",
        vcd::ScopeType::Fork => "fork",
        _ => "scope",
    }
}

fn value_to_char(v: &vcd::Value) -> char {
    match v {
        vcd::Value::V0 => '0',
        vcd::Value::V1 => '1',
        vcd::Value::X => 'X',
        vcd::Value::Z => 'Z',
    }
}

fn clean_path(path: &str) -> String {
    path.split('.')
        .map(|seg| {
            if let Some(pos) = seg.find(':') {
                &seg[pos + 1..]
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

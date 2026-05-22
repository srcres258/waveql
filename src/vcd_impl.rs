use crate::error::WaveqlError;
use crate::{
    CompactValue, FileFormat, LazyLoader, SignalData, SignalInfo, Timescale, TimeUnit, Waveform,
};
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;

pub fn parse_vcd(file_path: &str) -> Result<Waveform, WaveqlError> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let mut parser = vcd::Parser::new(reader);

    let mut timescale = Timescale::default();
    let mut scope_stack: Vec<String> = Vec::new();
    let mut signal_map: HashMap<String, SignalInfo> = HashMap::new();
    let mut id_codes: HashMap<String, String> = HashMap::new();

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
                id_codes.insert(code_str, path);
            }
            vcd::Command::Enddefinitions => {
                break;
            }
            _ => {}
        }
    }

    let mut signals: Vec<SignalInfo> = signal_map.into_values().collect();
    signals.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(Waveform {
        timescale,
        signals,
        data: HashMap::new(),
        file_format: FileFormat::Vcd,
        lazy: Some(LazyLoader::Vcd {
            file_path: file_path.to_string(),
            id_codes,
        }),
    })
}

pub fn load_vcd_signal(
    file_path: &str,
    id_codes: &HashMap<String, String>,
    target_path: &str,
) -> Result<SignalData, WaveqlError> {
    let target_id = id_codes
        .iter()
        .find(|(_, path)| *path == target_path)
        .map(|(id, _)| id.clone())
        .ok_or_else(|| WaveqlError::SignalNotFound(target_path.to_string()))?;

    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let mut parser = vcd::Parser::new(reader);

    let mut current_time: u64 = 0;
    let mut changes: Vec<(u64, CompactValue)> = Vec::new();
    let mut header_done = false;

    loop {
        let cmd = match parser.next() {
            Some(Ok(cmd)) => cmd,
            Some(Err(e)) => return Err(WaveqlError::VcdParse(format!("{e}"))),
            None => break,
        };

        match cmd {
            vcd::Command::Enddefinitions => {
                header_done = true;
            }
            vcd::Command::Timestamp(time) if header_done => {
                current_time = time;
            }
            vcd::Command::ChangeScalar(code, value) if header_done => {
                if code.to_string() == target_id {
                    let val_byte = match &value {
                        vcd::Value::V0 => b'0',
                        vcd::Value::V1 => b'1',
                        vcd::Value::X => b'X',
                        vcd::Value::Z => b'Z',
                    };
                    changes.push((current_time, CompactValue::Bit(val_byte)));
                }
            }
            vcd::Command::ChangeVector(code, values) if header_done => {
                if code.to_string() == target_id {
                    let val_str: String = values.iter().map(value_to_char).collect();
                    changes.push((current_time, CompactValue::new(&val_str)));
                }
            }
            _ => {}
        }
    }

    Ok(SignalData { changes })
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

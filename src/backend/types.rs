use std::fmt;

// ── File Format ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFormat {
    Vcd,
    Fst,
}

impl fmt::Display for FileFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileFormat::Vcd => write!(f, "VCD"),
            FileFormat::Fst => write!(f, "FST"),
        }
    }
}

// ── Timescale ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Timescale {
    pub magnitude: u64,
    pub unit: TimeUnit,
}

impl Timescale {
    pub fn to_fs(&self, value: u64) -> u64 {
        let factor: u64 = match self.unit {
            TimeUnit::S => 1_000_000_000_000_000,
            TimeUnit::Ms => 1_000_000_000_000,
            TimeUnit::Us => 1_000_000_000,
            TimeUnit::Ns => 1_000_000,
            TimeUnit::Ps => 1_000,
            TimeUnit::Fs => 1,
        };
        value * self.magnitude * factor
    }

    pub fn from_fs(&self, fs: u64) -> u64 {
        let factor: u64 = match self.unit {
            TimeUnit::S => 1_000_000_000_000_000,
            TimeUnit::Ms => 1_000_000_000_000,
            TimeUnit::Us => 1_000_000_000,
            TimeUnit::Ns => 1_000_000,
            TimeUnit::Ps => 1_000,
            TimeUnit::Fs => 1,
        };
        fs / (self.magnitude * factor)
    }

    pub fn format_time(&self, value: u64) -> String {
        let unit_str = match self.unit {
            TimeUnit::S => "s",
            TimeUnit::Ms => "ms",
            TimeUnit::Us => "us",
            TimeUnit::Ns => "ns",
            TimeUnit::Ps => "ps",
            TimeUnit::Fs => "fs",
        };
        format!("{}{}", value * self.magnitude, unit_str)
    }
}

impl fmt::Display for Timescale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.magnitude, self.unit)
    }
}

impl Default for Timescale {
    fn default() -> Self {
        Timescale {
            magnitude: 1,
            unit: TimeUnit::Ns,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeUnit {
    S,
    Ms,
    Us,
    Ns,
    Ps,
    Fs,
}

impl fmt::Display for TimeUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TimeUnit::S => write!(f, "s"),
            TimeUnit::Ms => write!(f, "ms"),
            TimeUnit::Us => write!(f, "us"),
            TimeUnit::Ns => write!(f, "ns"),
            TimeUnit::Ps => write!(f, "ps"),
            TimeUnit::Fs => write!(f, "fs"),
        }
    }
}

// ── Signal Info / Data ───────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct SignalInfo {
    pub path: String,
    pub width: u32,
}

#[derive(Debug, Clone)]
pub enum CompactValue {
    Bit(u8),
    Str(Box<str>),
}

impl CompactValue {
    pub fn new(s: &str) -> Self {
        let bytes = s.as_bytes();
        if bytes.len() <= 1 {
            CompactValue::Bit(bytes.first().copied().unwrap_or(b'?'))
        } else {
            CompactValue::Str(s.into())
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            CompactValue::Bit(b) => unsafe {
                std::str::from_utf8_unchecked(std::slice::from_ref(b))
            },
            CompactValue::Str(s) => s.as_ref(),
        }
    }

    pub fn is_high(&self) -> bool {
        match self {
            CompactValue::Bit(b'1') => true,
            CompactValue::Bit(_) => false,
            CompactValue::Str(s) => {
                for c in s.chars() {
                    match c {
                        '1'..='9' | 'A'..='F' | 'a'..='f' => return true,
                        _ => {}
                    }
                }
                false
            }
        }
    }

    pub fn format_ascii(&self) -> String {
        match self {
            CompactValue::Bit(b'0') => "░".to_string(),
            CompactValue::Bit(b'1') => "█".to_string(),
            CompactValue::Bit(b'X') | CompactValue::Bit(b'x') => "╳".to_string(),
            CompactValue::Bit(b'Z') | CompactValue::Bit(b'z') => "Z".to_string(),
            CompactValue::Bit(_) => "?".to_string(),
            CompactValue::Str(s) => {
                if s.len() > 8 {
                    format!("{}..", &s[..6])
                } else {
                    s.to_string()
                }
            }
        }
    }
}

impl std::fmt::Display for CompactValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct SignalData {
    pub changes: Vec<(u64, CompactValue)>,
}

impl SignalData {
    pub fn sample(&self, time: u64) -> Option<&CompactValue> {
        let idx = self.changes.partition_point(|(t, _)| *t <= time);
        if idx == 0 {
            None
        } else {
            Some(&self.changes[idx - 1].1)
        }
    }

    pub fn changes_in_range(&self, from: u64, to: u64) -> Vec<(u64, &CompactValue)> {
        self.changes
            .iter()
            .skip_while(move |(t, _)| *t < from)
            .take_while(move |(t, _)| *t <= to)
            .map(|(t, v)| (*t, v))
            .collect()
    }
}

pub fn wildcard_match(signals: &[SignalInfo], pattern: &str) -> Vec<String> {
    if !pattern.contains('*') {
        if signals.iter().any(|s| s.path == pattern) {
            return vec![pattern.to_string()];
        }
        return vec![];
    }
    let (prefix, suffix) = if let Some(pos) = pattern.find('*') {
        (&pattern[..pos], &pattern[pos + 1..])
    } else {
        (pattern, "")
    };
    signals
        .iter()
        .filter(|s| s.path.starts_with(prefix) && s.path.ends_with(suffix))
        .map(|s| s.path.clone())
        .collect()
}

pub fn parse_time_str(s: &str, timescale: &Timescale) -> Result<u64, crate::error::WaveqlError> {
    let (num_str, unit_str) = if let Some(idx) = s.find(|c: char| !c.is_ascii_digit()) {
        (&s[..idx], &s[idx..])
    } else {
        (s, "ns")
    };

    let num: u64 = num_str
        .parse()
        .map_err(|_| crate::error::WaveqlError::InvalidTime(s.to_string()))?;

    let unit = match unit_str {
        "s" => TimeUnit::S,
        "ms" => TimeUnit::Ms,
        "us" => TimeUnit::Us,
        "ns" => TimeUnit::Ns,
        "ps" => TimeUnit::Ps,
        "fs" => TimeUnit::Fs,
        other => {
            return Err(crate::error::WaveqlError::InvalidTime(format!(
                "Unknown time unit: {other}"
            )))
        }
    };

    let in_fs = Timescale { magnitude: 1, unit }.to_fs(num);
    Ok(timescale.from_fs(in_fs))
}

pub mod error;
pub mod loader;
pub mod vcd_impl;
pub mod fst_impl;
pub mod query;
pub mod evaluator;
pub mod output;

pub use query::Query;

use std::collections::HashMap;
use std::fmt;

// ── Core Data Types ──────────────────────────────────────────────

/// Internal representation of a loaded waveform file.
#[derive(Debug, Clone)]
pub struct Waveform {
    pub timescale: Timescale,
    pub signals: Vec<SignalInfo>,
    pub data: HashMap<String, SignalData>,
    pub file_format: FileFormat,
}

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
pub struct SignalData {
    pub changes: Vec<(u64, String)>,
}

impl SignalData {
    pub fn sample(&self, time: u64) -> Option<&str> {
        let idx = self.changes.partition_point(|(t, _)| *t <= time);
        if idx == 0 {
            None
        } else {
            Some(&self.changes[idx - 1].1)
        }
    }

    pub fn changes_in_range(&self, from: u64, to: u64) -> Vec<(u64, &str)> {
        self.changes
            .iter()
            .skip_while(move |(t, _)| *t < from)
            .take_while(move |(t, _)| *t <= to)
            .map(|(t, v)| (*t, v.as_str()))
            .collect()
    }
}

// ── Helpers ──────────────────────────────────────────────────────

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

    let in_fs = Timescale {
        magnitude: 1,
        unit,
    }
    .to_fs(num);
    Ok(timescale.from_fs(in_fs))
}

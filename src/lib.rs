pub mod backend;
pub mod error;
pub mod evaluator;
pub mod events;
pub mod fst_impl;
pub mod index;
pub mod loader;
pub mod output;
pub mod planner;
pub mod protocol;
pub mod query;
pub mod report;
pub mod trace;
pub mod vcd_impl;

pub use query::Query;

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

use crate::backend::capabilities::BackendCapabilities;
use crate::backend::metadata::WaveformMetadata;
use crate::backend::WaveformBackend;
use crate::error::WaveqlError;

// Re-export core types for backward compatibility
pub use crate::backend::types::{
    parse_time_str, CompactValue, FileFormat, SignalData, SignalInfo, TimeUnit, Timescale,
};

// ── Waveform ─────────────────────────────────────────────────────

pub struct Waveform {
    pub metadata: WaveformMetadata,
    pub signals: Vec<SignalInfo>,
    pub data: HashMap<String, SignalData>,
    pub capabilities: BackendCapabilities,
    lazy: Option<LazyLoader>,
}

impl fmt::Debug for Waveform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Waveform")
            .field("metadata", &self.metadata)
            .field("signals", &self.signals)
            .field("data", &self.data)
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

impl WaveformBackend for Waveform {
    fn metadata(&self) -> &WaveformMetadata {
        &self.metadata
    }

    fn capabilities(&self) -> &BackendCapabilities {
        &self.capabilities
    }

    fn timescale(&self) -> &Timescale {
        &self.metadata.timescale
    }

    fn signal_info(&self, path: &str) -> Result<&SignalInfo, WaveqlError> {
        self.signals
            .iter()
            .find(|s| s.path == path)
            .ok_or_else(|| WaveqlError::SignalNotFound(path.to_string()))
    }

    fn signal_iter(&self) -> Box<dyn Iterator<Item = &SignalInfo> + '_> {
        Box::new(self.signals.iter())
    }

    fn load_signals(&mut self, paths: &[String]) -> Result<(), WaveqlError> {
        for path in paths {
            self.load_signal(path)?;
        }
        Ok(())
    }

    fn signal_data(&self, path: &str) -> Result<&SignalData, WaveqlError> {
        self.data
            .get(path)
            .ok_or_else(|| WaveqlError::SignalNotFound(path.to_string()))
    }
}

impl Waveform {
    pub fn load_signal(&mut self, path: &str) -> Result<(), WaveqlError> {
        if self.data.contains_key(path) {
            return Ok(());
        }
        match &self.lazy {
            Some(LazyLoader::Fst {
                waves,
                time_table,
                sig_refs,
            }) => {
                let sig_ref = sig_refs
                    .get(path)
                    .ok_or_else(|| WaveqlError::SignalNotFound(path.to_string()))?;
                let mut waves = waves.borrow_mut();
                waves.load_signals(&[*sig_ref]);
                if let Some(signal) = waves.get_signal(*sig_ref) {
                    let mut changes: Vec<(u64, CompactValue)> = Vec::new();
                    for (time_idx, val) in signal.iter_changes() {
                        let time = time_table[time_idx as usize];
                        let cv = crate::fst_impl::format_signal_value(&val);
                        changes.push((time, cv));
                    }
                    self.data.insert(path.to_string(), SignalData { changes });
                }
            }
            Some(LazyLoader::Vcd {
                file_path,
                id_codes,
            }) => {
                let signal_data = crate::vcd_impl::load_vcd_signal(file_path, id_codes, path)?;
                self.data.insert(path.to_string(), signal_data);
            }
            None => {}
        }
        Ok(())
    }
}

// ── Lazy Loader ──────────────────────────────────────────────────

pub(crate) enum LazyLoader {
    Fst {
        waves: Box<RefCell<wellen::simple::Waveform>>,
        time_table: Vec<u64>,
        sig_refs: HashMap<String, wellen::SignalRef>,
    },
    #[allow(dead_code)]
    Vcd {
        file_path: String,
        id_codes: HashMap<String, String>,
    },
}

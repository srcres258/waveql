pub mod capabilities;
pub mod metadata;
pub mod types;

use crate::error::WaveqlError;

pub trait WaveformBackend {
    fn metadata(&self) -> &metadata::WaveformMetadata;
    fn capabilities(&self) -> &capabilities::BackendCapabilities;

    fn timescale(&self) -> &types::Timescale {
        &self.metadata().timescale
    }

    fn signal_info(&self, path: &str) -> Result<&types::SignalInfo, WaveqlError>;
    fn signal_iter(&self) -> Box<dyn Iterator<Item = &types::SignalInfo> + '_>;

    fn load_signals(&mut self, paths: &[String]) -> Result<(), WaveqlError>;

    fn signal_data(&self, path: &str) -> Result<&types::SignalData, WaveqlError>;

    fn resolve_signals(&self, patterns: &[String]) -> Result<Vec<String>, WaveqlError> {
        if patterns.is_empty() {
            return Ok(self.signal_iter().map(|s| s.path.clone()).collect());
        }

        let all_signals: Vec<types::SignalInfo> = self.signal_iter().cloned().collect();
        let mut result = Vec::new();
        for pattern in patterns {
            let matched = types::wildcard_match(&all_signals, pattern);
            if matched.is_empty() {
                return Err(WaveqlError::SignalNotFound(pattern.clone()));
            }
            result.extend(matched);
        }
        let mut seen = std::collections::HashSet::new();
        result.retain(|s| seen.insert(s.clone()));
        Ok(result)
    }
}

pub use capabilities::BackendCapabilities;
pub use metadata::WaveformMetadata;
pub use types::{
    parse_time_str, wildcard_match, CompactValue, FileFormat, SignalData, SignalInfo, TimeUnit,
    Timescale,
};

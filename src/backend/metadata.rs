use crate::backend::types::{FileFormat, Timescale};

#[derive(Debug, Clone)]
pub struct WaveformMetadata {
    pub timescale: Timescale,
    pub date: Option<String>,
    pub version: Option<String>,
    pub signal_count: usize,
    pub format: FileFormat,
}

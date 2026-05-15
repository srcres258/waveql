use crate::error::WaveqlError;
use crate::{Query, Waveform};

/// Render query result as JSON string.
pub fn render(
    waveform: &Waveform,
    query: &Query,
    file_name: &str,
) -> Result<String, WaveqlError> {
    crate::evaluator::evaluate(waveform, query, file_name)
}

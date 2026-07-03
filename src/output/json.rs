use crate::backend::WaveformBackend;
use crate::error::WaveqlError;
use crate::report::Report;
use crate::Query;

pub fn render(
    waveform: &dyn WaveformBackend,
    query: &Query,
    file_name: &str,
) -> Result<String, WaveqlError> {
    let report = crate::evaluator::evaluate(waveform, query, file_name)?;
    match &report {
        Report::Ascii(s) => Ok(s.clone()),
        _ => Ok(serde_json::to_string_pretty(&report)?),
    }
}

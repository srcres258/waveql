use crate::error::WaveqlError;
use crate::{FileFormat, Waveform};

/// Detect file format and load the waveform.
pub fn load(file_path: &str) -> Result<Waveform, WaveqlError> {
    let format = detect_format(file_path)?;
    match format {
        FileFormat::Vcd => crate::vcd_impl::parse_vcd(file_path),
        FileFormat::Fst => crate::fst_impl::parse_fst(file_path),
    }
}

/// Detect format from file extension.
fn detect_format(file_path: &str) -> Result<FileFormat, WaveqlError> {
    let lower = file_path.to_lowercase();
    if lower.ends_with(".vcd") {
        Ok(FileFormat::Vcd)
    } else if lower.ends_with(".fst") {
        Ok(FileFormat::Fst)
    } else {
        Err(WaveqlError::UnsupportedFormat(format!(
            "Cannot detect format from extension: {file_path}. Use .vcd or .fst"
        )))
    }
}

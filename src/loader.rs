use crate::backend::types::FileFormat;
use crate::backend::WaveformBackend;
use crate::error::WaveqlError;

pub fn load(file_path: &str) -> Result<Box<dyn WaveformBackend>, WaveqlError> {
    let format = detect_format(file_path)?;
    let waveform = match format {
        FileFormat::Vcd => crate::vcd_impl::parse_vcd(file_path)?,
        FileFormat::Fst => crate::fst_impl::parse_fst(file_path)?,
    };
    Ok(Box::new(waveform))
}

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

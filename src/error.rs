use thiserror::Error;

#[derive(Error, Debug)]
pub enum WaveqlError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("VCD parse error: {0}")]
    VcdParse(String),

    #[error("FST parse error: {0}")]
    FstParse(String),

    #[error("Signal not found: {0}")]
    SignalNotFound(String),

    #[error("Invalid time: {0}")]
    InvalidTime(String),

    #[error("Unsupported file format: {0}")]
    UnsupportedFormat(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Protocol not found: {0}")]
    ProtocolNotFound(String),

    #[error("Binding error: {0}")]
    BindingError(String),

    #[error("{0}")]
    Other(String),
}

impl From<wellen::WellenError> for WaveqlError {
    fn from(e: wellen::WellenError) -> Self {
        WaveqlError::FstParse(format!("{e}"))
    }
}

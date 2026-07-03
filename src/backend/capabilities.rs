use crate::backend::types::FileFormat;

#[derive(Debug, Clone)]
pub struct BackendCapabilities {
    pub supports_lazy_load: bool,
    pub supports_slice: bool,
    pub supports_incremental: bool,
    pub format: FileFormat,
    pub description: &'static str,
}

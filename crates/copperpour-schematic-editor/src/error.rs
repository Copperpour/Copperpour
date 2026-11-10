use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Parse error at byte {pos}: {msg}")]
    Parse { pos: usize, msg: String },

    #[error("Missing required field '{0}'")]
    MissingField(&'static str),

    #[error("write conflict: '{0}' changed since it was loaded")]
    Conflict(PathBuf),

    /// The file was replaced but did not read back as written.
    #[error("readback mismatch: '{0}' was replaced but did not read back as written")]
    ReadbackMismatch(PathBuf),

    #[error("KiCad editor lock blocks write to '{path}': {lock_path}")]
    KiCadEditorLocked { path: PathBuf, lock_path: PathBuf },
}

pub type Result<T> = std::result::Result<T, Error>;

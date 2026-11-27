use std::path::PathBuf;
use thiserror::Error;

/// Which phase of a durable multi-file transaction raised
/// [`SexpError::TransactionConflict`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionConflictKind {
    /// A target of this transaction did not match its expected before image.
    ///
    /// This check runs before the transaction's journal is persisted, so no
    /// target file was modified.
    BeforeImage,
    /// An older journal in the transaction directory is still pending, has a
    /// target whose content matches neither its before image nor its
    /// replacement, or recovery stopped before publishing any target.
    ///
    /// Nothing of the current call was written, and the older journal remains
    /// on disk for inspection, recovery, or abandonment.
    Recovery,
    /// A conflict during publication or readback of a new journal, or a
    /// recovery conflict after replay already published an entry.
    ///
    /// Target files may already have been modified. The journal path is
    /// diagnostic: a failed cleanup sync may follow a successful unlink.
    AfterImage,
}

#[derive(Debug, Error)]
pub enum SexpError {
    #[error("Parse error at offset {offset}: {message}")]
    Parse { offset: usize, message: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Missing node: {0}")]
    MissingNode(String),

    #[error("Invalid value: {0}")]
    InvalidValue(String),

    /// The document no longer matches the revision the caller edited.
    ///
    /// Callers must reload and reapply their operation instead of overwriting
    /// the newer document.
    #[error("write conflict: {path} changed since it was read")]
    Conflict { path: PathBuf },

    /// A revision-checked write found a non-regular target before staging.
    /// No replacement was attempted.
    #[error("write target is not a regular file: {path}")]
    NonRegularWriteTarget { path: PathBuf },

    /// The write replaced the document, but a directory sync or readback
    /// could not establish the saved result.
    ///
    /// Unlike [`SexpError::Conflict`], the file was replaced, so this is an
    /// uncertain outcome, never a refusal: tools report it as
    /// `mutation_outcome_uncertain`.
    #[error("readback mismatch: {path} was replaced but could not be verified")]
    ReadbackMismatch { path: PathBuf },

    /// KiCad owns, or may still own, the design document through its sibling
    /// lock.
    ///
    /// KiCad lock files identify only a username and hostname, so their
    /// presence cannot be distinguished reliably from a stale lock. Writers
    /// must fail closed and leave both the document and the lock untouched.
    #[error(
        "KiCad editor lock blocks write to {}: {}",
        crate::paths::display_path(.path),
        crate::paths::display_path(.lock_path)
    )]
    KiCadEditorLocked {
        path: PathBuf,
        lock_path: PathBuf,
        /// Inspection failures are retained so callers can distinguish an
        /// observed lock from a lock whose absence could not be established.
        inspection_error: Option<String>,
    },

    /// A revision-aware command found that one of its target items no longer
    /// matches the exact item revision on which the command was prepared.
    #[error("item conflict in {path}: {item}: {reason}")]
    ItemConflict {
        path: PathBuf,
        item: String,
        reason: String,
    },

    /// A durable multi-file transaction encountered a conflict during
    /// before-image verification, older-journal recovery, or after-image
    /// verification.
    #[error(
        "transaction conflict in {}: {}: {reason}",
        crate::paths::display_path(.journal),
        crate::paths::display_path(.path)
    )]
    TransactionConflict {
        path: PathBuf,
        journal: PathBuf,
        kind: TransactionConflictKind,
        reason: String,
    },
}

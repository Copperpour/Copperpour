//! Durable, exact-precondition transactions spanning several project files.
//!
//! A write-ahead journal is persisted before the first target is changed. On
//! restart, recovery rolls the transaction forward only while every target
//! still equals either its recorded before image or intended replacement.
//! Divergent content is never overwritten and leaves the journal available for
//! explicit resolution.

use crate::writer::{
    ensure_kicad_design_document_is_closed, lock_document_exclusive, parent_directory,
    read_string_unlocked, sync_parent_directory, write_atomic_unlocked_with,
    write_new_atomic_unlocked_private, write_new_atomic_unlocked_with, ExclusiveDocumentLock,
};
use crate::SexpError;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const JOURNAL_VERSION: u32 = 1;
const ABSENCE_GUARD_VERSION: u32 = 2;
const JOURNAL_PREFIX: &str = ".copperpour-transaction-";
/// Written before the rename to Copperpour; such journals are still recovered.
const LEGACY_JOURNAL_PREFIX: &str = ".konnect-transaction-";
const JOURNAL_PREFIXES: [&str; 2] = [JOURNAL_PREFIX, LEGACY_JOURNAL_PREFIX];
const JOURNAL_SUFFIX: &str = ".json";

/// One exact file transition in a durable project transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTransition {
    path: PathBuf,
    expected: Option<String>,
    replacement: Option<String>,
    guard: bool,
}

impl FileTransition {
    /// Replace an existing file only when it still equals `expected`.
    #[must_use]
    pub fn replace(
        path: impl Into<PathBuf>,
        expected: impl Into<String>,
        replacement: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            expected: Some(expected.into()),
            replacement: Some(replacement.into()),
            guard: false,
        }
    }

    /// Create a new file without replacing an existing path.
    #[must_use]
    pub fn create(path: impl Into<PathBuf>, replacement: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            expected: None,
            replacement: Some(replacement.into()),
            guard: false,
        }
    }

    /// Hold an existing file that the change was planned from but does not
    /// rewrite: it is locked and must still equal `expected` when the
    /// transaction commits, and it is never written.
    ///
    /// The journal records a guard as an entry whose before and after images
    /// are equal, which version-1 recovery already reads as applied while the
    /// file is untouched and as divergent once it changes. A guard that changed
    /// after a crash therefore stops recovery from completing a plan whose
    /// premises no longer hold, and releases that predate guards read the
    /// journal the same way.
    #[must_use]
    pub fn guard(path: impl Into<PathBuf>, expected: impl Into<String>) -> Self {
        let expected = expected.into();
        Self {
            path: path.into(),
            replacement: Some(expected.clone()),
            expected: Some(expected),
            guard: true,
        }
    }

    /// Require this path to remain absent through commit and crash recovery.
    /// Nothing is ever created at the guarded path.
    #[must_use]
    pub fn guard_absent(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            expected: None,
            replacement: None,
            guard: true,
        }
    }

    /// Target path of this transition.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Result of a successfully committed multi-file transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionCommit {
    /// Stable identifier used by the write-ahead journal.
    pub id: String,
    /// Number of files written; guards are not counted.
    pub files: usize,
}

/// Result of rolling one persisted transaction forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryOutcome {
    /// Stable transaction identifier.
    pub id: String,
    /// Files that were still at their before image and were completed.
    pub completed_files: usize,
}

/// Current relationship between a transaction target and its journal images.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionTargetState {
    /// The target still matches its recorded before image and can be completed.
    Pending,
    /// The target already matches its intended replacement.
    Applied,
    /// The target matches neither image and will never be overwritten automatically.
    Divergent,
}

/// Redacted status for one target in a persisted transaction journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionTargetStatus {
    /// Path relative to the inspected project directory.
    pub path: PathBuf,
    /// Content relationship without exposing either stored schematic image.
    pub state: TransactionTargetState,
}

/// Redacted status for one persisted transaction journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionStatus {
    /// Stable transaction identifier.
    pub id: String,
    /// Active journal path.
    pub journal: PathBuf,
    /// Per-target states without journal contents.
    pub targets: Vec<TransactionTargetStatus>,
}

/// Result of explicitly abandoning a journal that cannot be recovered safely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbandonedTransaction {
    /// Stable transaction identifier supplied by the caller.
    pub id: String,
    /// Former active journal path.
    pub journal: PathBuf,
    /// Inactive evidence file retained beside the project.
    pub abandoned_journal: PathBuf,
    /// Whether the journal parsed and passed containment validation.
    pub was_valid: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Journal {
    version: u32,
    id: String,
    entries: Vec<JournalEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalEntry {
    /// Path relative to the canonical journal directory.
    #[serde(with = "journal_path_serde")]
    path: PathBuf,
    #[serde(deserialize_with = "deserialize_image")]
    expected: Option<String>,
    #[serde(deserialize_with = "deserialize_image")]
    replacement: Option<String>,
}

// Both keys must be present. Only explicit null images can encode an absent path.
fn deserialize_image<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::deserialize(deserializer)
}

impl JournalEntry {
    /// A read-set guard: locked and checked, never written.
    fn is_guard(&self) -> bool {
        self.expected == self.replacement
    }

    fn matches_image(&self, path: &Path, image: Option<&str>) -> Result<bool, SexpError> {
        if self.replacement.is_none() {
            return path_is_absent(path);
        }
        Ok(current_content(path)?.as_deref() == image)
    }

    fn state(&self, path: &Path) -> Result<TransactionTargetState, SexpError> {
        if self.replacement.is_none() {
            return Ok(if path_is_absent(path)? {
                TransactionTargetState::Applied
            } else {
                TransactionTargetState::Divergent
            });
        }
        let current = current_content(path)?;
        Ok(if current == self.replacement {
            TransactionTargetState::Applied
        } else if current == self.expected {
            TransactionTargetState::Pending
        } else {
            TransactionTargetState::Divergent
        })
    }
}

mod journal_path_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::path::{Path, PathBuf};

    #[derive(Serialize, Deserialize)]
    struct EncodedPath {
        encoding: String,
        hex: String,
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StoredPath {
        Unicode(String),
        Encoded(EncodedPath),
    }

    pub fn serialize<S>(path: &Path, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if let Some(path) = path.to_str() {
            return serializer.serialize_str(path);
        }

        let (encoding, bytes) = native_bytes(path);
        EncodedPath {
            encoding: encoding.to_owned(),
            hex: crate::hex::encode_hex(&bytes),
        }
        .serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<PathBuf, D::Error>
    where
        D: Deserializer<'de>,
    {
        match StoredPath::deserialize(deserializer)? {
            StoredPath::Unicode(path) => Ok(PathBuf::from(path)),
            StoredPath::Encoded(path) => {
                let bytes = crate::hex::decode_hex(&path.hex).map_err(|reason| {
                    serde::de::Error::custom(format!("encoded transaction path {reason}"))
                })?;
                path_from_native_bytes(&path.encoding, bytes).map_err(serde::de::Error::custom)
            }
        }
    }

    #[cfg(unix)]
    fn native_bytes(path: &Path) -> (&'static str, Vec<u8>) {
        use std::os::unix::ffi::OsStrExt;
        ("unix-bytes-v1", path.as_os_str().as_bytes().to_vec())
    }

    #[cfg(unix)]
    fn path_from_native_bytes(encoding: &str, bytes: Vec<u8>) -> Result<PathBuf, String> {
        use std::os::unix::ffi::OsStringExt;
        if encoding != "unix-bytes-v1" {
            return Err(format!(
                "transaction path encoding '{encoding}' is not supported on this platform"
            ));
        }
        Ok(std::ffi::OsString::from_vec(bytes).into())
    }

    #[cfg(windows)]
    fn native_bytes(path: &Path) -> (&'static str, Vec<u8>) {
        use std::os::windows::ffi::OsStrExt;
        let bytes = path
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect();
        ("windows-wide-le-v1", bytes)
    }

    #[cfg(windows)]
    fn path_from_native_bytes(encoding: &str, bytes: Vec<u8>) -> Result<PathBuf, String> {
        use std::os::windows::ffi::OsStringExt;
        if encoding != "windows-wide-le-v1" || !bytes.len().is_multiple_of(2) {
            return Err(format!(
                "transaction path encoding '{encoding}' is not supported on this platform"
            ));
        }
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        Ok(std::ffi::OsString::from_wide(&units).into())
    }
}

/// Commit exact file transitions under one durable write-ahead journal.
///
/// Target paths must live inside `journal_directory`. Existing project-local
/// transaction journals are recovered before an unguarded transaction starts.
/// A transaction with read-set guards instead refuses until any older journal
/// is resolved explicitly: recovery could otherwise invalidate its read set.
/// Locks are acquired in stable path order to prevent cooperating writers from
/// deadlocking. The journal scan holds those file locks, not a project-wide
/// lock, so disjoint transactions are not serialized.
///
/// # Errors
///
/// Returns a conflict when a target does not match its expected content, a
/// transaction conflict when recovery encounters divergent content, or an I/O
/// error when journal or target durability cannot be established.
pub fn commit_file_transaction(
    journal_directory: impl AsRef<Path>,
    transitions: Vec<FileTransition>,
) -> Result<TransactionCommit, SexpError> {
    try_commit_file_transaction(journal_directory, transitions).map_err(|failure| failure.error)
}

/// A failed [`try_commit_file_transaction`].
#[derive(Debug)]
pub struct TransactionFailure {
    pub error: SexpError,
    /// The journal that reached disk, so this transaction's targets may
    /// already hold their replacements and recovery may still complete them.
    /// `None` means none of its targets was touched. Boxed, so the error
    /// stays under clippy's `result_large_err` limit where `PathBuf` is
    /// larger, as on Windows.
    pub journal: Option<Box<Path>>,
}

/// [`commit_file_transaction`], reporting whether a failure came before any
/// target could change.
///
/// # Errors
///
/// The same errors as [`commit_file_transaction`], with the journal left
/// behind, if any.
pub fn try_commit_file_transaction(
    journal_directory: impl AsRef<Path>,
    transitions: Vec<FileTransition>,
) -> Result<TransactionCommit, TransactionFailure> {
    // Everything up to the journal leaves the targets untouched.
    let prepare = || -> Result<_, SexpError> {
        let root = canonical_directory(journal_directory.as_ref())?;
        let guarded = transitions.iter().any(|transition| transition.guard);
        if !guarded {
            recover_file_transactions(&root)?;
        }
        let entries = normalize_transitions(&root, transitions)?;
        let id = uuid::Uuid::new_v4().to_string();
        let journal = Journal {
            version: if entries.iter().any(|entry| entry.replacement.is_none()) {
                ABSENCE_GUARD_VERSION
            } else {
                JOURNAL_VERSION
            },
            id,
            entries,
        };
        let journal_path = journal_path(&root, &journal.id);
        let locks = lock_entries(&root, &journal.entries)?;
        if guarded {
            if let Some(pending) = active_journal_paths(&root)?.into_iter().next() {
                return Err(transaction_conflict(
                    &pending,
                    &root,
                    "resolve the pending transaction before committing a guarded transaction",
                ));
            }
        }
        ensure_entries_are_closed(&root, &journal.entries)?;
        verify_before_images(&root, &journal_path, &journal.entries)?;
        Ok((root, journal, journal_path, locks))
    };
    let (root, journal, journal_path, _locks) = prepare().map_err(|error| TransactionFailure {
        error,
        journal: None,
    })?;
    // The journal is renamed into place, so a failed persist may still have
    // left it behind.
    persist_journal(&journal_path, &journal).map_err(|error| TransactionFailure {
        error,
        journal: std::fs::symlink_metadata(&journal_path)
            .is_ok()
            .then(|| journal_path.clone().into_boxed_path()),
    })?;

    (|| -> Result<(), SexpError> {
        for entry in journal.entries.iter().filter(|entry| !entry.is_guard()) {
            apply_entry(&root, &journal_path, entry, &journal.entries)?;
        }
        verify_after_images(&root, &journal_path, &journal.entries)?;
        remove_journal(&journal_path)
    })()
    .map_err(|error| TransactionFailure {
        error,
        journal: Some(journal_path.into_boxed_path()),
    })?;

    Ok(TransactionCommit {
        files: journal
            .entries
            .iter()
            .filter(|entry| !entry.is_guard())
            .count(),
        id: journal.id,
    })
}

/// Recover all project-local write-ahead journals in stable filename order.
///
/// Recovery is idempotent. Files already at their replacement are left alone;
/// files still at their before image are completed. Any other content is
/// preserved and reported as a transaction conflict.
///
/// # Errors
///
/// Returns a transaction conflict for divergent content, an invalid-value
/// error for malformed or unsupported journals, or an I/O error.
pub fn recover_file_transactions(
    journal_directory: impl AsRef<Path>,
) -> Result<Vec<RecoveryOutcome>, SexpError> {
    let root = canonical_directory(journal_directory.as_ref())?;
    let journals = active_journal_paths(&root)?;

    let mut outcomes = Vec::with_capacity(journals.len());
    for path in journals {
        outcomes.push(recover_journal(&root, &path)?);
    }
    Ok(outcomes)
}

/// Inspect every active transaction journal without exposing stored file images.
///
/// Target files are locked while each status is sampled. Malformed, unsupported,
/// or path-unsafe journals return an error rather than being trusted.
///
/// # Errors
///
/// Returns an invalid-value error for an invalid journal, a transaction error
/// for unsafe target paths, or an I/O error while reading or locking files.
pub fn inspect_file_transactions(
    journal_directory: impl AsRef<Path>,
) -> Result<Vec<TransactionStatus>, SexpError> {
    let root = canonical_directory(journal_directory.as_ref())?;
    let mut journals = active_journal_paths(&root)?;
    let mut statuses = Vec::with_capacity(journals.len());
    for path in journals.drain(..) {
        let journal = read_validated_journal(&root, &path)?;
        let _locks = lock_entries(&root, &journal.entries)?;
        statuses.push(inspect_journal(&root, &path, &journal)?);
    }
    Ok(statuses)
}

/// Recover one active transaction selected by its exact journal identifier.
///
/// # Errors
///
/// Returns an invalid-value error for an unsafe identifier or missing journal,
/// and otherwise the same errors as [`recover_file_transactions`].
pub fn recover_file_transaction(
    journal_directory: impl AsRef<Path>,
    id: &str,
) -> Result<RecoveryOutcome, SexpError> {
    let root = canonical_directory(journal_directory.as_ref())?;
    let path = existing_journal_path(&root, id)?;
    recover_journal(&root, &path)
}

/// Make one unrecoverable journal inactive without modifying any target file.
///
/// Valid journals may be abandoned only when at least one target is divergent;
/// safely recoverable journals must use [`recover_file_transaction`] instead.
/// Invalid journals can also be abandoned because their target paths are never
/// trusted or opened. The journal is retained beside the project with an
/// `.abandoned.json` suffix for explicit later inspection or deletion.
///
/// # Errors
///
/// Returns an invalid-value error for an unsafe identifier, missing journal,
/// safely recoverable journal, or an existing abandoned evidence file.
pub fn abandon_file_transaction(
    journal_directory: impl AsRef<Path>,
    id: &str,
) -> Result<AbandonedTransaction, SexpError> {
    let root = canonical_directory(journal_directory.as_ref())?;
    let path = existing_journal_path(&root, id)?;

    let mut held_locks = None;
    let was_valid = match read_validated_journal(&root, &path) {
        Ok(journal) => {
            held_locks = Some(lock_entries(&root, &journal.entries)?);
            let status = inspect_journal(&root, &path, &journal)?;
            if status
                .targets
                .iter()
                .all(|target| target.state != TransactionTargetState::Divergent)
            {
                return Err(SexpError::InvalidValue(format!(
                    "transaction '{id}' is safely recoverable; run transaction recover instead"
                )));
            }
            true
        }
        Err(SexpError::Io(error)) => return Err(SexpError::Io(error)),
        Err(_) => false,
    };

    let abandoned_journal = abandoned_journal_path(&path);
    if abandoned_journal.exists() {
        return Err(SexpError::InvalidValue(format!(
            "abandoned journal already exists: {}",
            crate::paths::display_path(&abandoned_journal)
        )));
    }
    std::fs::rename(&path, &abandoned_journal)?;
    sync_parent_directory(&root)?;
    drop(held_locks);
    Ok(AbandonedTransaction {
        id: id.to_owned(),
        journal: path,
        abandoned_journal,
        was_valid,
    })
}

fn recover_journal(root: &Path, journal_path: &Path) -> Result<RecoveryOutcome, SexpError> {
    let journal = read_validated_journal(root, journal_path)?;
    let _locks = lock_entries(root, &journal.entries)?;
    ensure_entries_are_closed(root, &journal.entries)?;
    let mut pending = Vec::new();
    for entry in &journal.entries {
        let path = root.join(&entry.path);
        match entry.state(&path)? {
            TransactionTargetState::Applied => {}
            TransactionTargetState::Pending => pending.push(entry),
            TransactionTargetState::Divergent => {
                return Err(transaction_conflict(
                    journal_path,
                    &path,
                    "content matches neither the before image nor replacement",
                ))
            }
        }
    }
    for entry in &pending {
        apply_entry(root, journal_path, entry, &journal.entries)?;
    }
    verify_after_images(root, journal_path, &journal.entries)?;
    remove_journal(journal_path)?;
    Ok(RecoveryOutcome {
        id: journal.id,
        completed_files: pending.len(),
    })
}

fn active_journal_paths(root: &Path) -> Result<Vec<PathBuf>, SexpError> {
    let mut journals = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if is_journal_path(&path) {
            journals.push(path);
        }
    }
    journals.sort();
    Ok(journals)
}

fn read_validated_journal(root: &Path, path: &Path) -> Result<Journal, SexpError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(SexpError::InvalidValue(format!(
            "transaction journal must be a regular file, not a symlink or directory: {}",
            path.display()
        )));
    }
    let source = std::fs::read_to_string(path)?;
    let journal: Journal = serde_json::from_str(&source).map_err(|error| {
        SexpError::InvalidValue(format!(
            "invalid transaction journal {}: {error}",
            path.display()
        ))
    })?;
    validate_transaction_id(&journal.id)?;
    if path.parent() != Some(root) || journal_id(path) != Some(journal.id.as_str()) {
        return Err(SexpError::InvalidValue(format!(
            "transaction journal ID does not match its filename: {}",
            path.display()
        )));
    }
    if !matches!(journal.version, JOURNAL_VERSION | ABSENCE_GUARD_VERSION) {
        return Err(SexpError::InvalidValue(format!(
            "unsupported transaction journal version {} in {}",
            journal.version,
            path.display()
        )));
    }
    if journal.entries.iter().any(|entry| {
        entry.replacement.is_none()
            && (journal.version != ABSENCE_GUARD_VERSION || entry.expected.is_some())
    }) {
        return Err(SexpError::InvalidValue(format!(
            "invalid absence guard in transaction journal {}",
            path.display()
        )));
    }
    validate_journal_entries(root, &journal.entries)?;
    Ok(journal)
}

fn inspect_journal(
    root: &Path,
    path: &Path,
    journal: &Journal,
) -> Result<TransactionStatus, SexpError> {
    let mut targets = Vec::with_capacity(journal.entries.len());
    for entry in &journal.entries {
        let state = entry.state(&root.join(&entry.path))?;
        targets.push(TransactionTargetStatus {
            path: entry.path.clone(),
            state,
        });
    }
    Ok(TransactionStatus {
        id: journal.id.clone(),
        journal: path.to_path_buf(),
        targets,
    })
}

fn normalize_transitions(
    root: &Path,
    transitions: Vec<FileTransition>,
) -> Result<Vec<JournalEntry>, SexpError> {
    if transitions.is_empty() {
        return Err(SexpError::InvalidValue(
            "file transaction needs at least one transition".to_owned(),
        ));
    }
    let mut seen = HashSet::with_capacity(transitions.len());
    let mut entries = Vec::with_capacity(transitions.len());
    for transition in transitions {
        let relative = normalize_target(root, &transition.path)?;
        if !seen.insert(relative.clone()) {
            return Err(SexpError::InvalidValue(format!(
                "duplicate transaction target {}",
                transition.path.display()
            )));
        }
        if !transition.guard && transition.expected == transition.replacement {
            return Err(SexpError::InvalidValue(format!(
                "transaction target {} is unchanged",
                transition.path.display()
            )));
        }
        entries.push(JournalEntry {
            path: relative,
            expected: transition.expected,
            replacement: transition.replacement,
        });
    }
    if entries.iter().all(JournalEntry::is_guard) {
        return Err(SexpError::InvalidValue(
            "file transaction needs at least one file to change, not only guards".to_owned(),
        ));
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(entries)
}

fn validate_journal_entries(root: &Path, entries: &[JournalEntry]) -> Result<(), SexpError> {
    if entries.is_empty() {
        return Err(SexpError::InvalidValue(
            "transaction journal has no entries".to_owned(),
        ));
    }
    let mut seen = HashSet::with_capacity(entries.len());
    for entry in entries {
        let normalized = normalize_target(root, &root.join(&entry.path))?;
        if normalized != entry.path || !seen.insert(normalized) {
            return Err(SexpError::InvalidValue(
                "transaction journal contains an unsafe or duplicate path".to_owned(),
            ));
        }
    }
    Ok(())
}

fn canonical_directory(path: &Path) -> Result<PathBuf, SexpError> {
    let canonical = path.canonicalize()?;
    if !canonical.is_dir() {
        return Err(SexpError::InvalidValue(format!(
            "transaction journal root is not a directory: {}",
            path.display()
        )));
    }
    Ok(canonical)
}

fn normalize_target(root: &Path, path: &Path) -> Result<PathBuf, SexpError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let file_name = absolute.file_name().ok_or_else(|| {
        SexpError::InvalidValue(format!(
            "transaction target has no filename: {}",
            path.display()
        ))
    })?;
    let parent = absolute.parent().ok_or_else(|| {
        SexpError::InvalidValue(format!(
            "transaction target has no parent: {}",
            path.display()
        ))
    })?;
    let canonical_parent = parent.canonicalize()?;
    if !canonical_parent.starts_with(root) {
        return Err(SexpError::InvalidValue(format!(
            "transaction target escapes journal root: {}",
            path.display()
        )));
    }
    canonical_parent
        .join(file_name)
        .strip_prefix(root)
        .map(Path::to_path_buf)
        .map_err(|_| {
            SexpError::InvalidValue(format!(
                "transaction target escapes journal root: {}",
                path.display()
            ))
        })
}

fn lock_entries(
    root: &Path,
    entries: &[JournalEntry],
) -> Result<Vec<ExclusiveDocumentLock>, SexpError> {
    entries
        .iter()
        .map(|entry| lock_document_exclusive(&root.join(&entry.path)))
        .collect()
}

fn ensure_entries_are_closed(root: &Path, entries: &[JournalEntry]) -> Result<(), SexpError> {
    for entry in entries {
        ensure_kicad_design_document_is_closed(&root.join(&entry.path))?;
    }
    Ok(())
}

fn verify_before_images(
    root: &Path,
    journal: &Path,
    entries: &[JournalEntry],
) -> Result<(), SexpError> {
    for entry in entries {
        let path = root.join(&entry.path);
        if !entry.matches_image(&path, entry.expected.as_deref())? {
            return Err(transaction_conflict(
                journal,
                &path,
                "content changed before the transaction committed",
            ));
        }
    }
    Ok(())
}

fn verify_after_images(
    root: &Path,
    journal: &Path,
    entries: &[JournalEntry],
) -> Result<(), SexpError> {
    // Check guards after reading the written targets so a slow readback
    // cannot unnecessarily widen the gap between guard check and cleanup.
    let writes = entries.iter().filter(|entry| !entry.is_guard());
    let guards = entries.iter().filter(|entry| entry.is_guard());
    for entry in writes.chain(guards) {
        #[cfg(test)]
        if !entry.is_guard() {
            BEFORE_TARGET_READBACK.with(|hook| {
                if let Some(hook) = hook.take() {
                    hook();
                }
            });
        }
        let path = root.join(&entry.path);
        if !entry.matches_image(&path, entry.replacement.as_deref())? {
            return Err(transaction_conflict(
                journal,
                &path,
                "replacement did not remain durable",
            ));
        }
    }
    Ok(())
}

fn apply_entry(
    root: &Path,
    journal_path: &Path,
    entry: &JournalEntry,
    entries: &[JournalEntry],
) -> Result<(), SexpError> {
    let replacement = entry.replacement.as_deref().ok_or_else(|| {
        SexpError::InvalidValue("an absence guard cannot be published as a write".into())
    })?;
    let path = root.join(&entry.path);
    let check_guards = || {
        #[cfg(test)]
        BEFORE_PUBLISH.with(|hook| {
            if let Some(hook) = hook.take() {
                hook();
            }
        });
        for guard in entries.iter().filter(|entry| entry.is_guard()) {
            let path = root.join(&guard.path);
            ensure_kicad_design_document_is_closed(&path)?;
            if !guard.matches_image(&path, guard.expected.as_deref())? {
                return Err(transaction_conflict(
                    journal_path,
                    &path,
                    "guard content changed before publishing a transaction target",
                ));
            }
        }
        Ok(())
    };
    if entry.expected.is_some() {
        write_atomic_unlocked_with(&path, replacement, check_guards)
    } else {
        write_new_atomic_unlocked_with(&path, replacement, check_guards)
    }
}

#[cfg(test)]
thread_local! {
    static BEFORE_PUBLISH: std::cell::Cell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::Cell::new(None) };
    static BEFORE_TARGET_READBACK: std::cell::Cell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::Cell::new(None) };
}

fn path_is_absent(path: &Path) -> Result<bool, SexpError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error.into()),
    }
}

fn current_content(path: &Path) -> Result<Option<String>, SexpError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(SexpError::InvalidValue(format!(
                "transaction target must not be a symlink: {}",
                path.display()
            )))
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    match read_string_unlocked(path) {
        Ok(content) => Ok(Some(content)),
        Err(SexpError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn persist_journal(path: &Path, journal: &Journal) -> Result<(), SexpError> {
    let source = serde_json::to_string(journal).map_err(|error| {
        SexpError::InvalidValue(format!("could not serialize journal: {error}"))
    })?;
    // A journal holds complete before and after images of the project, so it
    // is created owner-only rather than at the ordinary create mode.
    write_new_atomic_unlocked_private(path, &source)
}

fn remove_journal(path: &Path) -> Result<(), SexpError> {
    std::fs::remove_file(path)?;
    sync_parent_directory(parent_directory(path))
}

fn journal_path(root: &Path, id: &str) -> PathBuf {
    prefixed_journal_path(root, JOURNAL_PREFIX, id)
}

fn prefixed_journal_path(root: &Path, prefix: &str, id: &str) -> PathBuf {
    root.join(format!("{prefix}{id}{JOURNAL_SUFFIX}"))
}

/// The active journal for `id`, under either prefix.
fn existing_journal_path(root: &Path, id: &str) -> Result<PathBuf, SexpError> {
    validate_transaction_id(id)?;
    let mut found = JOURNAL_PREFIXES
        .iter()
        .map(|prefix| prefixed_journal_path(root, prefix, id))
        .filter(|path| path.is_file());
    match (found.next(), found.next()) {
        (Some(path), None) => Ok(path),
        (None, _) => Err(SexpError::InvalidValue(format!(
            "transaction journal '{id}' was not found in {}",
            root.display()
        ))),
        (Some(first), Some(second)) => Err(SexpError::InvalidValue(format!(
            "transaction '{id}' has two journals, {} and {}; move the one you do not want out of the project",
            first.display(),
            second.display()
        ))),
    }
}

/// Keeps the active journal's prefix, so an old journal stays recognizable.
fn abandoned_journal_path(journal: &Path) -> PathBuf {
    journal.with_extension("abandoned.json")
}

fn validate_transaction_id(id: &str) -> Result<(), SexpError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(SexpError::InvalidValue(
            "transaction ID must contain only ASCII letters, digits, '-' or '_'".to_owned(),
        ));
    }
    Ok(())
}

/// The ID in a journal's filename, under either prefix.
fn journal_id(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    JOURNAL_PREFIXES
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))?
        .strip_suffix(JOURNAL_SUFFIX)
}

fn is_journal_path(path: &Path) -> bool {
    journal_id(path)
        .is_some_and(|id| !id.ends_with(".abandoned") && validate_transaction_id(id).is_ok())
}

fn transaction_conflict(journal: &Path, path: &Path, reason: &str) -> SexpError {
    SexpError::TransactionConflict {
        path: path.to_path_buf(),
        journal: journal.to_path_buf(),
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schematic::{format_hierarchical_sheet, HierarchicalSheetSpec};
    use crate::{prepare_command, ItemAnchor, SchematicCommand};

    #[test]
    fn transaction_replaces_and_creates_files_together() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let parent = directory.path().join("root.kicad_sch");
        let child = directory.path().join("child.kicad_sch");
        std::fs::write(&parent, "parent before").expect("write parent");

        let outcome = commit_file_transaction(
            directory.path(),
            vec![
                FileTransition::replace(&parent, "parent before", "parent after"),
                FileTransition::create(&child, "child after"),
            ],
        )
        .expect("transaction commits");

        assert_eq!(outcome.files, 2);
        assert_eq!(std::fs::read_to_string(parent).unwrap(), "parent after");
        assert_eq!(std::fs::read_to_string(child).unwrap(), "child after");
        assert!(recover_file_transactions(directory.path())
            .unwrap()
            .is_empty());
    }

    #[cfg(unix)]
    fn non_unicode_journal_fixture(root: &Path) -> (Journal, PathBuf) {
        use std::os::unix::ffi::OsStringExt;

        let relative = PathBuf::from(std::ffi::OsString::from_vec(
            b"sheet-\xff.kicad_sch".to_vec(),
        ));
        let journal = Journal {
            version: JOURNAL_VERSION,
            id: "non-unicode-fixture".to_owned(),
            entries: vec![JournalEntry {
                path: relative.clone(),
                expected: None,
                replacement: Some("created".to_owned()),
            }],
        };
        (journal, root.join(relative))
    }

    #[test]
    fn a_refused_transaction_reports_that_no_journal_was_written() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let target = directory.path().join("root.kicad_sch");
        std::fs::write(&target, "saved meanwhile").unwrap();

        let failure = try_commit_file_transaction(
            directory.path(),
            vec![FileTransition::replace(&target, "read earlier", "after")],
        )
        .expect_err("a changed target is refused");
        assert!(
            matches!(failure.error, SexpError::TransactionConflict { .. }),
            "{:?}",
            failure.error
        );
        assert_eq!(failure.journal, None);
        assert!(inspect_file_transactions(directory.path())
            .unwrap()
            .is_empty());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "saved meanwhile");
    }

    /// A target whose directory refuses the replacement fails after the
    /// journal reached disk.
    #[cfg(unix)]
    #[test]
    fn a_transaction_failing_after_its_journal_reports_it_persisted() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let sealed = directory.path().join("sealed");
        std::fs::create_dir(&sealed).unwrap();
        let first = directory.path().join("a.kicad_sch");
        let second = sealed.join("b.kicad_sch");
        std::fs::write(&first, "a before").unwrap();
        std::fs::write(&second, "b before").unwrap();
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o555)).unwrap();
        if std::fs::write(sealed.join("probe"), "").is_ok() {
            return; // Permissions are not enforced (running as root).
        }

        let failure = try_commit_file_transaction(
            directory.path(),
            vec![
                FileTransition::replace(&first, "a before", "a after"),
                FileTransition::replace(&second, "b before", "b after"),
            ],
        )
        .expect_err("the sealed target cannot be replaced");
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o755)).unwrap();

        let journal = failure
            .journal
            .unwrap_or_else(|| panic!("{:?}", failure.error));
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "a after");
        let name = journal.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with(".copperpour-transaction-"), "{name}");
        let active = inspect_file_transactions(directory.path()).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].journal.as_path(), &*journal);
    }

    #[cfg(unix)]
    #[test]
    fn transaction_journal_codec_round_trips_a_non_unicode_target_path() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        let (journal, _) = non_unicode_journal_fixture(&root);
        let path = journal_path(&root, &journal.id);
        persist_journal(&path, &journal).expect("journal serializes");

        let decoded = read_validated_journal(&root, &path).expect("journal deserializes");
        assert_eq!(decoded.entries[0].path, journal.entries[0].path);
        remove_journal(&path).unwrap();
    }

    #[cfg(unix)]
    const JOURNAL_MODE_PROBE: &str = "transaction::tests::report_journal_mode";
    #[cfg(unix)]
    const PROBE_JOURNAL_ID: &str = "mode-probe";
    #[cfg(unix)]
    const PROBE_SHEET: &str = "sheet.kicad_sch";

    #[cfg(unix)]
    #[test]
    #[ignore = "probe: run by transaction_journal_is_created_private under an explicit umask"]
    fn report_journal_mode() {
        let root = crate::writer::mode_probe::directory();
        let journal = Journal {
            version: JOURNAL_VERSION,
            id: PROBE_JOURNAL_ID.to_owned(),
            entries: vec![JournalEntry {
                path: PathBuf::from(PROBE_SHEET),
                expected: None,
                replacement: Some("created".to_owned()),
            }],
        };

        let path = journal_path(&root, &journal.id);
        persist_journal(&path, &journal).expect("journal serializes");
        // The design file the parent compares the journal against, created by
        // the same transaction machinery in the same run — the two creation
        // policies have to differ. The journal is deliberately left in place
        // for the parent to inspect.
        apply_entry(&root, &path, &journal.entries[0], &journal.entries).expect("entry applies");
    }

    #[cfg(unix)]
    #[test]
    fn transaction_journal_is_created_private() {
        use crate::writer::mode_probe;

        // 0o000 and 0o277 are the two ways the umask can break this: the first
        // would widen a journal that followed the ordinary policy to 0o666,
        // the second would narrow one created at 0o600 down to 0o400.
        for (umask, design) in [("000", 0o666), ("277", 0o400)] {
            let directory = mode_probe::run_under_umask(JOURNAL_MODE_PROBE, umask);
            let root = directory.path();

            mode_probe::assert_owner_only(
                "a transaction journal",
                mode_probe::mode_of(&journal_path(root, PROBE_JOURNAL_ID)),
                umask,
            );
            assert_eq!(
                mode_probe::mode_of(&root.join(PROBE_SHEET)),
                design,
                "a design file created beside the journal follows the umask under umask {umask}"
            );
        }
    }

    // Linux filesystems accept arbitrary non-NUL filename bytes. macOS APIs
    // reject ill-formed UTF-8 with EILSEQ, so only the codec/identity contract
    // is portable there rather than creation of such a path.
    #[cfg(target_os = "linux")]
    #[test]
    fn transaction_recovers_a_non_unicode_target_path_on_linux() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        let (journal, target) = non_unicode_journal_fixture(&root);
        let path = journal_path(&root, &journal.id);
        persist_journal(&path, &journal).expect("journal serializes");

        recover_file_transaction(&root, &journal.id).expect("non-Unicode recovery commits");

        assert_eq!(std::fs::read_to_string(target).unwrap(), "created");
        assert!(recover_file_transactions(directory.path())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn stale_precondition_changes_nothing_and_leaves_no_journal() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let parent = directory.path().join("root.kicad_sch");
        let child = directory.path().join("child.kicad_sch");
        std::fs::write(&parent, "external edit").expect("write parent");

        let error = commit_file_transaction(
            directory.path(),
            vec![
                FileTransition::replace(&parent, "old parent", "new parent"),
                FileTransition::create(&child, "new child"),
            ],
        )
        .expect_err("stale transaction conflicts");

        assert!(matches!(error, SexpError::TransactionConflict { .. }));
        assert_eq!(std::fs::read_to_string(parent).unwrap(), "external edit");
        assert!(!child.exists());
        assert!(recover_file_transactions(directory.path())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn editor_lock_changes_nothing_and_leaves_no_journal() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let parent = directory.path().join("root.kicad_sch");
        let child = directory.path().join("child.kicad_sch");
        let lock = directory.path().join("~root.kicad_sch.lck");
        std::fs::write(&parent, "parent before").expect("write parent");
        std::fs::write(&lock, "not parseable").expect("write editor lock");

        let error = commit_file_transaction(
            directory.path(),
            vec![
                FileTransition::replace(&parent, "parent before", "parent after"),
                FileTransition::create(&child, "child after"),
            ],
        )
        .expect_err("editor lock conflicts");

        assert!(matches!(error, SexpError::KiCadEditorLocked { .. }));
        assert_eq!(std::fs::read_to_string(parent).unwrap(), "parent before");
        assert!(!child.exists());
        assert!(active_journal_paths(directory.path()).unwrap().is_empty());
        assert!(lock.exists());
    }

    #[test]
    fn recovery_finishes_a_partially_applied_transaction() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        let parent = root.join("root.kicad_sch");
        let child = root.join("child.kicad_sch");
        std::fs::write(&parent, "parent after").expect("simulate first write");
        let journal = Journal {
            version: JOURNAL_VERSION,
            id: "crash-fixture".to_owned(),
            entries: vec![
                JournalEntry {
                    path: PathBuf::from("root.kicad_sch"),
                    expected: Some("parent before".to_owned()),
                    replacement: Some("parent after".to_owned()),
                },
                JournalEntry {
                    path: PathBuf::from("child.kicad_sch"),
                    expected: None,
                    replacement: Some("child after".to_owned()),
                },
            ],
        };
        let journal_path = journal_path(&root, &journal.id);
        persist_journal(&journal_path, &journal).expect("persist crash journal");

        let outcomes = recover_file_transactions(&root).expect("recovery succeeds");

        assert_eq!(outcomes[0].completed_files, 1);
        assert_eq!(std::fs::read_to_string(parent).unwrap(), "parent after");
        assert_eq!(std::fs::read_to_string(child).unwrap(), "child after");
        assert!(!journal_path.exists());
    }

    /// A crash journal holding a read-set guard exactly as
    /// [`FileTransition::guard`] records it: equal before and after images.
    fn guarded_crash_journal(root: &Path, id: &str) -> PathBuf {
        let journal = Journal {
            version: JOURNAL_VERSION,
            id: id.to_owned(),
            entries: vec![
                JournalEntry {
                    path: PathBuf::from("dest.kicad_sch"),
                    expected: Some("dest before".to_owned()),
                    replacement: Some("dest after".to_owned()),
                },
                JournalEntry {
                    path: PathBuf::from("root.kicad_sch"),
                    expected: Some("root guard".to_owned()),
                    replacement: Some("root guard".to_owned()),
                },
                JournalEntry {
                    path: PathBuf::from("source.kicad_sch"),
                    expected: Some("source before".to_owned()),
                    replacement: Some("source after".to_owned()),
                },
            ],
        };
        let path = journal_path(root, &journal.id);
        persist_journal(&path, &journal).expect("persist probe journal");
        path
    }

    #[test]
    fn a_guard_is_journaled_with_equal_images() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("root.kicad_sch"), "root").unwrap();
        std::fs::write(root.join("sheet.kicad_sch"), "before").unwrap();
        let entries = normalize_transitions(
            &root,
            vec![
                FileTransition::guard(root.join("root.kicad_sch"), "root"),
                FileTransition::replace(root.join("sheet.kicad_sch"), "before", "after"),
            ],
        )
        .unwrap();
        let guard = &entries[0];
        assert_eq!(guard.path, PathBuf::from("root.kicad_sch"));
        assert_eq!(guard.expected.as_deref(), Some("root"));
        assert_eq!(guard.replacement.as_deref(), Some("root"));
        assert!(guard.is_guard() && !entries[1].is_guard());
    }

    #[cfg(unix)]
    #[test]
    fn a_guarded_commit_writes_only_the_changes_and_never_rewrites_the_guard() {
        use std::os::unix::fs::MetadataExt;
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        let guard = root.join("root.kicad_sch");
        let sheet = root.join("sheet.kicad_sch");
        std::fs::write(&guard, "root").unwrap();
        std::fs::write(&sheet, "before").unwrap();
        let inode = std::fs::metadata(&guard).unwrap().ino();

        let commit = commit_file_transaction(
            &root,
            vec![
                FileTransition::guard(&guard, "root"),
                FileTransition::replace(&sheet, "before", "after"),
            ],
        )
        .expect("guarded commit");

        assert_eq!(commit.files, 1);
        assert_eq!(std::fs::read_to_string(&sheet).unwrap(), "after");
        assert_eq!(std::fs::read_to_string(&guard).unwrap(), "root");
        assert_eq!(std::fs::metadata(&guard).unwrap().ino(), inode);
        assert!(active_journal_paths(&root).unwrap().is_empty());
    }

    #[test]
    fn a_changed_guard_refuses_the_commit_before_any_journal_or_write() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        let guard = root.join("root.kicad_sch");
        let sheet = root.join("sheet.kicad_sch");
        std::fs::write(&guard, "root saved meanwhile").unwrap();
        std::fs::write(&sheet, "before").unwrap();

        let failure = try_commit_file_transaction(
            &root,
            vec![
                FileTransition::guard(&guard, "root"),
                FileTransition::replace(&sheet, "before", "after"),
            ],
        )
        .expect_err("a changed guard refuses");

        assert!(failure.journal.is_none());
        assert!(matches!(
            failure.error,
            SexpError::TransactionConflict { .. }
        ));
        assert_eq!(std::fs::read_to_string(&sheet).unwrap(), "before");
        assert_eq!(
            std::fs::read_to_string(&guard).unwrap(),
            "root saved meanwhile"
        );
        assert!(active_journal_paths(&root).unwrap().is_empty());
    }

    #[test]
    fn guarded_commit_does_not_recover_an_older_transaction() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let guard = root.join("root.kicad_sch");
        let sheet = root.join("sheet.kicad_sch");
        std::fs::write(&guard, "before").unwrap();
        std::fs::write(&sheet, "untouched").unwrap();
        let pending = legacy_journal(&root, "pending-guard-target", "root.kicad_sch");
        let original_journal = std::fs::read(&pending).unwrap();
        let failure = try_commit_file_transaction(
            &root,
            vec![
                FileTransition::guard(&guard, "before"),
                FileTransition::replace(&sheet, "untouched", "candidate"),
            ],
        )
        .unwrap_err();
        assert!(failure.journal.is_none());
        assert!(matches!(
            failure.error,
            SexpError::TransactionConflict { .. }
        ));
        assert_eq!(std::fs::read_to_string(&guard).unwrap(), "before");
        assert_eq!(std::fs::read_to_string(&sheet).unwrap(), "untouched");
        assert_eq!(active_journal_paths(&root).unwrap(), vec![pending.clone()]);
        assert_eq!(std::fs::read(pending).unwrap(), original_journal);
    }

    #[test]
    fn guards_alone_are_not_a_transaction_and_unchanged_replacements_stay_refused() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        let file = root.join("root.kicad_sch");
        std::fs::write(&file, "root").unwrap();

        let guards_only =
            commit_file_transaction(&root, vec![FileTransition::guard(&file, "root")]);
        assert!(matches!(guards_only, Err(SexpError::InvalidValue(_))));
        let unchanged =
            commit_file_transaction(&root, vec![FileTransition::replace(&file, "root", "root")]);
        assert!(matches!(unchanged, Err(SexpError::InvalidValue(_))));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "root");
        assert!(active_journal_paths(&root).unwrap().is_empty());
    }

    #[test]
    fn recovery_completes_past_an_untouched_guard() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("source.kicad_sch"), "source after").unwrap();
        std::fs::write(root.join("dest.kicad_sch"), "dest before").unwrap();
        std::fs::write(root.join("root.kicad_sch"), "root guard").unwrap();
        let journal_path = guarded_crash_journal(&root, "guard-untouched");

        let statuses = inspect_file_transactions(&root).expect("inspect accepts guard");
        let guard = statuses[0]
            .targets
            .iter()
            .find(|target| target.path == Path::new("root.kicad_sch"))
            .unwrap();
        assert_eq!(guard.state, TransactionTargetState::Applied);

        let outcomes = recover_file_transactions(&root).expect("recovery accepts guard");
        assert_eq!(outcomes[0].completed_files, 1);
        assert_eq!(
            std::fs::read_to_string(root.join("dest.kicad_sch")).unwrap(),
            "dest after"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("root.kicad_sch")).unwrap(),
            "root guard"
        );
        assert!(!journal_path.exists());
    }

    #[test]
    fn recovery_stops_on_a_changed_guard_without_writing() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("source.kicad_sch"), "source after").unwrap();
        std::fs::write(root.join("dest.kicad_sch"), "dest before").unwrap();
        std::fs::write(root.join("root.kicad_sch"), "root edited later").unwrap();
        let journal_path = guarded_crash_journal(&root, "guard-changed");

        let statuses = inspect_file_transactions(&root).expect("inspect accepts guard");
        let guard = statuses[0]
            .targets
            .iter()
            .find(|target| target.path == Path::new("root.kicad_sch"))
            .unwrap();
        assert_eq!(guard.state, TransactionTargetState::Divergent);

        let error = recover_file_transactions(&root).expect_err("changed guard blocks");
        assert!(matches!(error, SexpError::TransactionConflict { .. }));
        assert_eq!(
            std::fs::read_to_string(root.join("dest.kicad_sch")).unwrap(),
            "dest before"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("root.kicad_sch")).unwrap(),
            "root edited later"
        );
        assert!(journal_path.exists());
    }

    #[test]
    fn a_late_guard_editor_lock_blocks_commit_and_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let guard = root.join("root.kicad_sch");
        let guard_lock = root.join("~root.kicad_sch.lck");
        let sheet = root.join("sheet.kicad_sch");
        std::fs::write(&guard, "root").unwrap();
        std::fs::write(&sheet, "before").unwrap();
        let arm_editor_lock = || {
            let lock = guard_lock.clone();
            BEFORE_PUBLISH.with(|hook| {
                hook.set(Some(Box::new(move || {
                    std::fs::write(lock, "late editor lock").unwrap();
                })));
            });
        };
        arm_editor_lock();
        let failure = try_commit_file_transaction(
            &root,
            vec![
                FileTransition::guard(&guard, "root"),
                FileTransition::replace(&sheet, "before", "after"),
            ],
        )
        .unwrap_err();
        assert!(
            matches!(failure.error, SexpError::KiCadEditorLocked { path, .. } if path == guard)
        );
        let journal = failure.journal.unwrap();
        let original_journal = std::fs::read(&journal).unwrap();
        assert_eq!(read_validated_journal(&root, &journal).unwrap().version, 1);
        assert_eq!(std::fs::read_to_string(&sheet).unwrap(), "before");

        std::fs::remove_file(&guard_lock).unwrap();
        arm_editor_lock();
        let error = recover_file_transactions(&root).unwrap_err();
        assert!(matches!(error, SexpError::KiCadEditorLocked { path, .. } if path == guard));
        assert_eq!(std::fs::read_to_string(&sheet).unwrap(), "before");
        assert_eq!(std::fs::read(&journal).unwrap(), original_journal);

        std::fs::remove_file(&guard_lock).unwrap();
        assert_eq!(
            recover_file_transactions(&root).unwrap()[0].completed_files,
            1
        );
        assert_eq!(std::fs::read_to_string(&sheet).unwrap(), "after");
        assert_eq!(std::fs::read_to_string(&guard).unwrap(), "root");
        assert!(!journal.exists());
    }

    #[test]
    fn absence_guard_blocks_recovery_if_the_board_appears() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let board = root.join("project.kicad_pcb");
        let sheet = root.join("sheet.kicad_sch");
        let lock = root.join("~sheet.kicad_sch.lck");
        std::fs::write(&sheet, "before").unwrap();
        let late_lock = lock.clone();
        BEFORE_PUBLISH.with(|hook| {
            hook.set(Some(Box::new(move || {
                std::fs::write(late_lock, "editor").unwrap();
            })));
        });
        let failure = try_commit_file_transaction(
            &root,
            vec![
                FileTransition::guard_absent(&board),
                FileTransition::replace(&sheet, "before", "after"),
            ],
        )
        .unwrap_err();
        let journal = failure.journal.unwrap();
        let saved = std::fs::read(&journal).unwrap();
        let parsed = read_validated_journal(&root, &journal).unwrap();
        assert_eq!(parsed.version, ABSENCE_GUARD_VERSION);
        let board_status = || {
            inspect_file_transactions(&root).unwrap()[0]
                .targets
                .iter()
                .find(|target| target.path == Path::new("project.kicad_pcb"))
                .unwrap()
                .state
        };
        assert!(!board.exists());
        assert_eq!(std::fs::read_to_string(&sheet).unwrap(), "before");
        std::fs::remove_file(lock).unwrap();
        let board_source = b"\xff";
        std::fs::write(&board, board_source).unwrap();
        assert_eq!(board_status(), TransactionTargetState::Divergent);
        let error = recover_file_transactions(&root).unwrap_err();
        assert!(matches!(error, SexpError::TransactionConflict { path, .. } if path == board));
        assert_eq!(std::fs::read_to_string(&sheet).unwrap(), "before");
        assert_eq!(std::fs::read(&board).unwrap(), board_source);
        assert_eq!(std::fs::read(&journal).unwrap(), saved);

        std::fs::remove_file(&board).unwrap();
        assert_eq!(board_status(), TransactionTargetState::Applied);
        assert_eq!(
            recover_file_transactions(&root).unwrap()[0].completed_files,
            1
        );
        assert_eq!(std::fs::read_to_string(&sheet).unwrap(), "after");
        assert!(!board.exists());
        assert!(!journal.exists());
    }

    #[test]
    fn v2_journal_with_omitted_image_refuses_without_replaying() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let sheet = root.join("sheet.kicad_sch");
        std::fs::write(&sheet, "before").unwrap();
        let journal = journal_path(&root, "missing-image");
        for field in ["expected", "replacement"] {
            let mut entry = serde_json::json!({
                "path": "create.kicad_sch",
                "expected": null,
                "replacement": null
            });
            entry.as_object_mut().unwrap().remove(field);
            let content = serde_json::json!({
                "id": "missing-image",
                "version": ABSENCE_GUARD_VERSION,
                "entries": [
                    {"path": "sheet.kicad_sch", "expected": "before", "replacement": "after"},
                    entry
                ]
            })
            .to_string();
            std::fs::write(&journal, &content).unwrap();
            let error = recover_file_transactions(&root).unwrap_err();
            assert!(error
                .to_string()
                .contains(&format!("missing field `{field}`")));
            assert_eq!(std::fs::read_to_string(&sheet).unwrap(), "before");
            assert!(!root.join("create.kicad_sch").exists());
            assert_eq!(std::fs::read_to_string(&journal).unwrap(), content);
        }
    }

    #[test]
    fn invalid_absence_guard_images_refuse_without_replaying() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let sheet = root.join("sheet.kicad_sch");
        std::fs::write(&sheet, "before").unwrap();
        let journal = journal_path(&root, "invalid-absence");
        for (version, expected) in [
            (JOURNAL_VERSION, None),
            (ABSENCE_GUARD_VERSION, Some("present")),
        ] {
            let content = serde_json::json!({
                "id": "invalid-absence",
                "version": version,
                "entries": [
                    {"path": "sheet.kicad_sch", "expected": "before", "replacement": "after"},
                    {"path": "project.kicad_pcb", "expected": expected, "replacement": null}
                ]
            })
            .to_string();
            std::fs::write(&journal, &content).unwrap();
            let error = recover_file_transactions(&root).unwrap_err();
            assert!(
                error.to_string().contains("invalid absence guard"),
                "{error}"
            );
            assert_eq!(std::fs::read_to_string(&sheet).unwrap(), "before");
            assert!(!root.join("project.kicad_pcb").exists());
            assert_eq!(std::fs::read_to_string(&journal).unwrap(), content);
        }
    }

    #[test]
    fn absence_guard_refuses_an_occupied_path_before_journaling() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let sheet = root.join("sheet.kicad_sch");
        let board = root.join("project.kicad_pcb");
        std::fs::write(&sheet, "before").unwrap();
        std::fs::create_dir(&board).unwrap();
        let failure = try_commit_file_transaction(
            &root,
            vec![
                FileTransition::replace(&sheet, "before", "after"),
                FileTransition::guard_absent(&board),
            ],
        )
        .unwrap_err();
        assert!(failure.journal.is_none());
        assert!(
            matches!(failure.error, SexpError::TransactionConflict { path, .. } if path == board)
        );
        assert_eq!(std::fs::read_to_string(&sheet).unwrap(), "before");
        assert!(board.is_dir());
        assert!(active_journal_paths(&root).unwrap().is_empty());
    }

    #[test]
    fn absence_guard_refuses_an_occupied_path_at_publish() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let sheet = root.join("sheet.kicad_sch");
        let board = root.join("project.kicad_pcb");
        std::fs::write(&sheet, "before").unwrap();
        let late_board = board.clone();
        BEFORE_PUBLISH.with(|hook| {
            hook.set(Some(Box::new(move || {
                std::fs::write(late_board, "appeared").unwrap();
            })));
        });
        let failure = try_commit_file_transaction(
            &root,
            vec![
                FileTransition::replace(&sheet, "before", "after"),
                FileTransition::guard_absent(&board),
            ],
        )
        .unwrap_err();
        assert!(
            matches!(failure.error, SexpError::TransactionConflict { path, .. } if path == board)
        );
        assert!(failure.journal.unwrap().is_file());
        assert_eq!(std::fs::read_to_string(&sheet).unwrap(), "before");
        assert_eq!(std::fs::read_to_string(&board).unwrap(), "appeared");
    }

    #[test]
    fn absence_guard_is_rechecked_after_target_readback() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let sheet = root.join("sheet.kicad_sch");
        let board = root.join("project.kicad_pcb");
        std::fs::write(&sheet, "before").unwrap();
        let arm_board_restore = || {
            let board = board.clone();
            BEFORE_TARGET_READBACK.with(|hook| {
                hook.set(Some(Box::new(move || {
                    std::fs::write(board, "restored").unwrap();
                })));
            });
        };
        arm_board_restore();
        let failure = try_commit_file_transaction(
            &root,
            vec![
                FileTransition::guard_absent(&board),
                FileTransition::replace(&sheet, "before", "after"),
            ],
        )
        .unwrap_err();
        assert!(
            matches!(failure.error, SexpError::TransactionConflict { path, .. } if path == board)
        );
        let journal = failure.journal.unwrap();
        let saved = std::fs::read(&journal).unwrap();
        assert_eq!(std::fs::read_to_string(&sheet).unwrap(), "after");
        assert_eq!(std::fs::read_to_string(&board).unwrap(), "restored");

        std::fs::remove_file(&board).unwrap();
        arm_board_restore();
        let error = recover_file_transactions(&root).unwrap_err();
        assert!(matches!(error, SexpError::TransactionConflict { path, .. } if path == board));
        assert_eq!(std::fs::read(&journal).unwrap(), saved);
        assert_eq!(std::fs::read_to_string(&sheet).unwrap(), "after");
        assert_eq!(std::fs::read_to_string(&board).unwrap(), "restored");
    }

    #[test]
    fn a_guard_changed_during_commit_keeps_the_real_journal_for_inspection() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let guard = root.join("root.kicad_sch");
        let sheet = root.join("sheet.kicad_sch");
        std::fs::write(&guard, "root").unwrap();
        std::fs::write(&sheet, "before").unwrap();
        let external = guard.clone();
        BEFORE_PUBLISH.with(|hook| {
            hook.set(Some(Box::new(move || {
                std::fs::write(external, "external edit").unwrap();
            })));
        });
        let failure = try_commit_file_transaction(
            &root,
            vec![
                FileTransition::guard(&guard, "root"),
                FileTransition::replace(&sheet, "before", "after"),
            ],
        )
        .unwrap_err();
        assert!(
            matches!(failure.error, SexpError::TransactionConflict { path, .. } if path == guard)
        );
        let journal = failure.journal.unwrap();
        let original_journal = std::fs::read(&journal).unwrap();
        assert_eq!(std::fs::read_to_string(&sheet).unwrap(), "before");
        let error = recover_file_transactions(&root).unwrap_err();
        assert!(matches!(error, SexpError::TransactionConflict { path, .. } if path == guard));
        assert_eq!(std::fs::read_to_string(&guard).unwrap(), "external edit");
        assert_eq!(std::fs::read_to_string(&sheet).unwrap(), "before");
        assert_eq!(std::fs::read(journal).unwrap(), original_journal);
    }

    #[test]
    fn recovery_defers_to_an_editor_lock_without_changing_the_journal() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        let parent = root.join("root.kicad_sch");
        let lock = root.join("~root.kicad_sch.lck");
        std::fs::write(&parent, "parent before").expect("write parent");
        std::fs::write(&lock, "stale-looking lock").expect("write editor lock");
        let journal = Journal {
            version: JOURNAL_VERSION,
            id: "locked-recovery".to_owned(),
            entries: vec![JournalEntry {
                path: PathBuf::from("root.kicad_sch"),
                expected: Some("parent before".to_owned()),
                replacement: Some("parent after".to_owned()),
            }],
        };
        let journal_path = journal_path(&root, &journal.id);
        persist_journal(&journal_path, &journal).expect("persist crash journal");
        let journal_before = std::fs::read(&journal_path).expect("read journal");

        let error = recover_file_transactions(&root).expect_err("editor lock conflicts");

        assert!(matches!(error, SexpError::KiCadEditorLocked { .. }));
        assert_eq!(std::fs::read_to_string(parent).unwrap(), "parent before");
        assert_eq!(std::fs::read(&journal_path).unwrap(), journal_before);
        assert!(lock.exists());
    }

    #[test]
    fn recovery_preserves_divergent_content_and_journal() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        let parent = root.join("root.kicad_sch");
        std::fs::write(&parent, "external edit").expect("write divergence");
        let journal = Journal {
            version: JOURNAL_VERSION,
            id: "conflict-fixture".to_owned(),
            entries: vec![JournalEntry {
                path: PathBuf::from("root.kicad_sch"),
                expected: Some("parent before".to_owned()),
                replacement: Some("parent after".to_owned()),
            }],
        };
        let journal_path = journal_path(&root, &journal.id);
        persist_journal(&journal_path, &journal).expect("persist crash journal");

        let error = recover_file_transactions(&root).expect_err("divergence conflicts");

        assert!(matches!(error, SexpError::TransactionConflict { .. }));
        assert_eq!(std::fs::read_to_string(parent).unwrap(), "external edit");
        assert!(journal_path.exists());
    }

    #[test]
    fn inspection_reports_redacted_target_states() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("pending.kicad_sch"), "before").unwrap();
        std::fs::write(root.join("applied.kicad_sch"), "after").unwrap();
        std::fs::write(root.join("divergent.kicad_sch"), "external").unwrap();
        let journal = Journal {
            version: JOURNAL_VERSION,
            id: "inspection-fixture".to_owned(),
            entries: vec![
                JournalEntry {
                    path: PathBuf::from("pending.kicad_sch"),
                    expected: Some("before".to_owned()),
                    replacement: Some("after".to_owned()),
                },
                JournalEntry {
                    path: PathBuf::from("applied.kicad_sch"),
                    expected: Some("before".to_owned()),
                    replacement: Some("after".to_owned()),
                },
                JournalEntry {
                    path: PathBuf::from("divergent.kicad_sch"),
                    expected: Some("before".to_owned()),
                    replacement: Some("after".to_owned()),
                },
            ],
        };
        persist_journal(&journal_path(&root, &journal.id), &journal).unwrap();

        let statuses = inspect_file_transactions(&root).unwrap();

        assert_eq!(statuses.len(), 1);
        let states: Vec<_> = statuses[0]
            .targets
            .iter()
            .map(|target| target.state)
            .collect();
        assert_eq!(
            states,
            [
                TransactionTargetState::Pending,
                TransactionTargetState::Applied,
                TransactionTargetState::Divergent,
            ]
        );
    }

    #[test]
    fn targeted_recovery_only_recovers_the_selected_journal() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        for id in ["first", "second"] {
            let target = format!("{id}.kicad_sch");
            std::fs::write(root.join(&target), "before").unwrap();
            let journal = Journal {
                version: JOURNAL_VERSION,
                id: id.to_owned(),
                entries: vec![JournalEntry {
                    path: PathBuf::from(&target),
                    expected: Some("before".to_owned()),
                    replacement: Some("after".to_owned()),
                }],
            };
            persist_journal(&journal_path(&root, id), &journal).unwrap();
        }

        recover_file_transaction(&root, "first").unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("first.kicad_sch")).unwrap(),
            "after"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("second.kicad_sch")).unwrap(),
            "before"
        );
        assert!(!journal_path(&root, "first").exists());
        assert!(journal_path(&root, "second").exists());
    }

    #[test]
    fn abandon_unwedges_a_divergent_transaction_without_touching_targets() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        let target = root.join("root.kicad_sch");
        std::fs::write(&target, "external edit").unwrap();
        let journal = Journal {
            version: JOURNAL_VERSION,
            id: "abandon-fixture".to_owned(),
            entries: vec![JournalEntry {
                path: PathBuf::from("root.kicad_sch"),
                expected: Some("before".to_owned()),
                replacement: Some("after".to_owned()),
            }],
        };
        persist_journal(&journal_path(&root, &journal.id), &journal).unwrap();

        let outcome = abandon_file_transaction(&root, &journal.id).unwrap();

        assert!(outcome.was_valid);
        assert_eq!(std::fs::read_to_string(target).unwrap(), "external edit");
        assert!(!outcome.journal.exists());
        assert!(outcome.abandoned_journal.exists());
        assert!(recover_file_transactions(&root).unwrap().is_empty());
    }

    #[test]
    fn abandon_refuses_a_safely_recoverable_transaction() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("root.kicad_sch"), "before").unwrap();
        let journal = Journal {
            version: JOURNAL_VERSION,
            id: "recoverable-fixture".to_owned(),
            entries: vec![JournalEntry {
                path: PathBuf::from("root.kicad_sch"),
                expected: Some("before".to_owned()),
                replacement: Some("after".to_owned()),
            }],
        };
        let path = journal_path(&root, &journal.id);
        persist_journal(&path, &journal).unwrap();

        let error = abandon_file_transaction(&root, &journal.id).unwrap_err();

        assert!(matches!(error, SexpError::InvalidValue(_)));
        assert!(path.exists());
    }

    #[test]
    fn forceful_abandonment_can_quarantine_a_malformed_journal() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        let path = journal_path(&root, "malformed-fixture");
        std::fs::write(&path, "not json").unwrap();

        let outcome = abandon_file_transaction(&root, "malformed-fixture").unwrap();

        assert!(!outcome.was_valid);
        assert!(!path.exists());
        assert!(outcome.abandoned_journal.exists());
        assert!(recover_file_transactions(&root).unwrap().is_empty());
    }

    /// A journal a Konnect-named release left behind, restated as a literal
    /// rather than read from the prefix constants.
    fn legacy_journal(root: &Path, id: &str, target: &str) -> PathBuf {
        let path = root.join(format!(".konnect-transaction-{id}.json"));
        let journal = Journal {
            version: JOURNAL_VERSION,
            id: id.to_owned(),
            entries: vec![JournalEntry {
                path: PathBuf::from(target),
                expected: Some("before".to_owned()),
                replacement: Some("after".to_owned()),
            }],
        };
        persist_journal(&path, &journal).unwrap();
        path
    }

    #[test]
    fn a_new_transaction_first_completes_a_legacy_journal() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("old.kicad_sch"), "before").unwrap();
        let legacy = legacy_journal(&root, "legacy-pending", "old.kicad_sch");
        let statuses = inspect_file_transactions(&root).unwrap();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].journal, legacy);

        let target = root.join("new.kicad_sch");
        commit_file_transaction(&root, vec![FileTransition::create(&target, "new")]).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("old.kicad_sch")).unwrap(),
            "after"
        );
        assert!(!legacy.exists());
    }

    #[test]
    fn a_legacy_journal_is_recovered_by_id() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("root.kicad_sch"), "before").unwrap();
        let legacy = legacy_journal(&root, "legacy-by-id", "root.kicad_sch");

        let outcome = recover_file_transaction(&root, "legacy-by-id").unwrap();

        assert_eq!(outcome.completed_files, 1);
        assert_eq!(
            std::fs::read_to_string(root.join("root.kicad_sch")).unwrap(),
            "after"
        );
        assert!(!legacy.exists());
    }

    #[test]
    fn an_abandoned_legacy_journal_keeps_its_prefix() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("root.kicad_sch"), "external edit").unwrap();
        let legacy = legacy_journal(&root, "legacy-divergent", "root.kicad_sch");

        let outcome = abandon_file_transaction(&root, "legacy-divergent").unwrap();

        assert!(outcome.was_valid);
        assert_eq!(outcome.journal, legacy);
        assert_eq!(
            outcome.abandoned_journal,
            root.join(".konnect-transaction-legacy-divergent.abandoned.json")
        );
        assert!(outcome.abandoned_journal.exists());
        assert!(inspect_file_transactions(&root).unwrap().is_empty());
    }

    #[test]
    fn one_id_under_both_prefixes_is_refused() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("root.kicad_sch"), "external edit").unwrap();
        let legacy = legacy_journal(&root, "twice", "root.kicad_sch");
        let current = root.join(".copperpour-transaction-twice.json");
        std::fs::copy(&legacy, &current).unwrap();

        for error in [
            recover_file_transaction(&root, "twice").unwrap_err(),
            abandon_file_transaction(&root, "twice").unwrap_err(),
        ] {
            assert!(
                matches!(&error, SexpError::InvalidValue(reason) if reason.contains("two journals")),
                "{error:?}"
            );
        }
        assert!(legacy.exists() && current.exists());
    }

    #[test]
    fn transaction_rejects_targets_outside_the_journal_root() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let outside = tempfile::tempdir().expect("outside directory");
        let target = outside.path().join("outside.kicad_sch");

        let error = commit_file_transaction(
            directory.path(),
            vec![FileTransition::create(target, "content")],
        )
        .expect_err("outside target rejected");

        assert!(matches!(error, SexpError::InvalidValue(_)));
    }

    #[cfg(unix)]
    #[test]
    fn recovery_rejects_a_symlink_target_without_touching_its_destination() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        let outside = tempfile::NamedTempFile::new().expect("outside file");
        std::fs::write(outside.path(), "outside before").unwrap();
        symlink(outside.path(), root.join("linked.kicad_sch")).unwrap();
        let journal = Journal {
            version: JOURNAL_VERSION,
            id: "symlink-target".to_owned(),
            entries: vec![JournalEntry {
                path: PathBuf::from("linked.kicad_sch"),
                expected: Some("outside before".to_owned()),
                replacement: Some("outside after".to_owned()),
            }],
        };
        let path = journal_path(&root, &journal.id);
        persist_journal(&path, &journal).unwrap();

        let error = recover_file_transaction(&root, &journal.id).unwrap_err();

        assert!(matches!(error, SexpError::InvalidValue(_)));
        assert_eq!(
            std::fs::read_to_string(outside.path()).unwrap(),
            "outside before"
        );
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_journal_is_not_followed_and_can_be_quarantined() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().unwrap();
        let outside = tempfile::NamedTempFile::new().expect("outside journal");
        std::fs::write(outside.path(), "private outside content").unwrap();
        let path = journal_path(&root, "symlink-journal");
        symlink(outside.path(), &path).unwrap();

        let error = inspect_file_transactions(&root).unwrap_err();
        assert!(matches!(error, SexpError::InvalidValue(_)));

        let outcome = abandon_file_transaction(&root, "symlink-journal").unwrap();
        assert!(!outcome.was_valid);
        assert_eq!(
            std::fs::read_to_string(outside.path()).unwrap(),
            "private outside content"
        );
        assert!(outcome.abandoned_journal.is_symlink());
    }

    #[test]
    fn hierarchy_link_and_inverse_restore_both_files_exactly() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let parent = directory.path().join("root.kicad_sch");
        let child = directory.path().join("child.kicad_sch");
        let parent_before = r#"(kicad_sch
	(version 20250101)
	(uuid "root-uuid")
	(sheet_instances (path "/" (page "1")))
)"#;
        let child_before = r#"(kicad_sch
	(version 20250101)
	(uuid "child-root")
	(symbol
		(lib_id "Device:R")
		(at 10 20 0)
		(unit 1)
		(property "Reference" "R1" (at 10 20 0))
		(uuid "child-symbol")
	)
)"#;
        std::fs::write(&parent, parent_before).expect("write parent");
        std::fs::write(&child, child_before).expect("write child");
        let sheet_block = format_hierarchical_sheet(HierarchicalSheetSpec {
            name: "Child",
            file: "child.kicad_sch",
            x: 20.0,
            y: 30.0,
            width: 80.0,
            height: 50.0,
            project_name: "demo",
            parent_instance_path: "/root-uuid",
            page: "2",
        });
        let parent_command = SchematicCommand::insert_item(
            parent_before,
            sheet_block,
            ItemAnchor::BeforeFooter,
            "Link child",
        )
        .expect("parent command prepares")
        .requiring_unchanged_document();
        let sheet_id = parent_command.changes[0].id.to_string();
        let child_command = SchematicCommand::ensure_symbol_instance_path(
            child_before,
            "demo",
            &format!("/root-uuid/{sheet_id}"),
            "Link child symbols",
        )
        .expect("child command prepares")
        .expect("child needs patching");
        let (parent_after, parent_outcome) =
            prepare_command(&parent, parent_before, &parent_command).expect("parent applies");
        let (child_after, child_outcome) =
            prepare_command(&child, child_before, &child_command).expect("child applies");

        commit_file_transaction(
            directory.path(),
            vec![
                FileTransition::replace(&parent, parent_before, &parent_after),
                FileTransition::replace(&child, child_before, &child_after),
            ],
        )
        .expect("link transaction commits");

        let (parent_restored, _) = prepare_command(
            &parent,
            &std::fs::read_to_string(&parent).unwrap(),
            &parent_outcome.inverse,
        )
        .expect("parent inverse applies");
        let (child_restored, _) = prepare_command(
            &child,
            &std::fs::read_to_string(&child).unwrap(),
            &child_outcome.inverse,
        )
        .expect("child inverse applies");
        commit_file_transaction(
            directory.path(),
            vec![
                FileTransition::replace(&parent, &parent_after, parent_restored),
                FileTransition::replace(&child, &child_after, child_restored),
            ],
        )
        .expect("inverse transaction commits");

        assert_eq!(std::fs::read_to_string(parent).unwrap(), parent_before);
        assert_eq!(std::fs::read_to_string(child).unwrap(), child_before);
    }
}

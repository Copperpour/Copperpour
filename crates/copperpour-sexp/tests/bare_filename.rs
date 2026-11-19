use std::path::{Path, PathBuf};
use std::sync::Mutex;

use copperpour_sexp::writer::{
    kicad_editor_lock_path, write_atomic, write_atomic_if_unchanged, write_new_atomic,
    write_new_atomic_verified,
};
use copperpour_sexp::{commit_file_transaction, FileTransition, SexpError};

/// Process-wide cwd lock for this integration test binary.
static CWD_LOCK: Mutex<()> = Mutex::new(());

struct CwdGuard {
    previous: PathBuf,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl CwdGuard {
    fn enter(dir: &Path) -> Self {
        let lock = CWD_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::current_dir().expect("read current_dir");
        std::env::set_current_dir(dir).expect("set_current_dir");
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.previous);
    }
}

#[test]
fn write_atomic_accepts_a_bare_filename() {
    let dir = tempfile::tempdir().unwrap();
    let _cwd = CwdGuard::enter(dir.path());

    let bare = Path::new("board.kicad_pcb");
    std::fs::write(bare, "(kicad_pcb (version 1))\n").unwrap();

    write_atomic(bare, "(kicad_pcb (version 2))\n").unwrap();
    assert_eq!(
        std::fs::read_to_string(bare).unwrap(),
        "(kicad_pcb (version 2))\n"
    );
}

#[test]
fn write_atomic_if_unchanged_accepts_a_bare_filename() {
    let dir = tempfile::tempdir().unwrap();
    let _cwd = CwdGuard::enter(dir.path());

    let bare = Path::new("board.kicad_pcb");
    let before = "(kicad_pcb (version 1))\n";
    let after = "(kicad_pcb (version 2))\n";
    std::fs::write(bare, before).unwrap();

    write_atomic_if_unchanged(bare, before, after).unwrap();
    assert_eq!(std::fs::read_to_string(bare).unwrap(), after);

    let err = write_atomic_if_unchanged(bare, before, "(kicad_pcb (version 3))\n").unwrap_err();
    assert!(matches!(err, SexpError::Conflict { .. }), "got {err:?}");
    assert_eq!(std::fs::read_to_string(bare).unwrap(), after);
}

#[test]
fn write_new_atomic_variants_accept_a_bare_filename() {
    let dir = tempfile::tempdir().unwrap();
    let _cwd = CwdGuard::enter(dir.path());

    let created = Path::new("new_a.kicad_sch");
    write_new_atomic(created, "(kicad_sch (version 1))\n").unwrap();
    assert_eq!(
        std::fs::read_to_string(created).unwrap(),
        "(kicad_sch (version 1))\n"
    );

    let verified = Path::new("new_b.kicad_sch");
    write_new_atomic_verified(verified, "(kicad_sch (version 2))\n").unwrap();
    assert_eq!(
        std::fs::read_to_string(verified).unwrap(),
        "(kicad_sch (version 2))\n"
    );
}

#[test]
fn kicad_editor_lock_path_resolves_nonexistent_bare_filename_in_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let _cwd = CwdGuard::enter(dir.path());

    let canonical_dir = dir.path().canonicalize().unwrap();
    let lock = kicad_editor_lock_path(Path::new("absent.kicad_sch")).unwrap();
    assert_eq!(lock, canonical_dir.join("~absent.kicad_sch.lck"));
}

#[test]
fn commit_file_transaction_accepts_a_bare_target_filename() {
    let dir = tempfile::tempdir().unwrap();
    let _cwd = CwdGuard::enter(dir.path());

    let bare = Path::new("sheet.kicad_sch");
    let before = "(kicad_sch (version 1))\n";
    let after = "(kicad_sch (version 2))\n";
    std::fs::write(bare, before).unwrap();

    commit_file_transaction(
        Path::new("."),
        vec![FileTransition::replace(bare, before, after)],
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(bare).unwrap(), after);
}

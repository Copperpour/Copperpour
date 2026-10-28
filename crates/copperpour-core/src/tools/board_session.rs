//! Process-lifetime safety memory for boards positively observed through KiCad IPC.
//!
//! An unreachable transport is ambiguous after a board was live: KiCad may have
//! crashed with unsaved state. Remembering that observation lets file-fallback
//! tools fail closed instead of editing a potentially stale save (#240).
//!
//! Authority can return to the file without a server restart. KiCad's exact-board
//! lock file records how the editor went away: a close through KiCad's own path
//! (nothing unsaved, or the user decided) removes the lock, while a crash or a kill
//! leaves it behind. So when a board was live *with its lock present*, and that lock
//! is later gone, KiCad closed the document itself and the saved file is
//! authoritative. Any other case - lock still present, or never seen - keeps
//! refusing, including when inspecting the lock fails.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::live_board::{editor_lock, EditorLock};

#[derive(Clone, Default)]
pub(crate) struct BoardSessionMemory {
    /// Board -> whether the exact-board KiCad lock was present when it was observed
    /// live. A board observed without a lock yields no close evidence (#671).
    observed_live: Arc<Mutex<HashMap<PathBuf, bool>>>,
}

impl BoardSessionMemory {
    pub(crate) fn observe_live(&self, board: &Path) {
        self.observed_live
            .lock()
            .expect("board-session memory poisoned")
            .insert(
                board_key(board),
                matches!(editor_lock(board), EditorLock::Present(_)),
            );
    }

    /// Whether this process currently holds a live observation for `board`.
    ///
    /// Read gates word their own refusals from it; authority to fall back to the
    /// file is [`Self::authorize_file_fallback`], which can release an
    /// observation a clean editor close has made moot.
    pub(crate) fn was_observed_live(&self, board: &Path) -> bool {
        self.observed_live
            .lock()
            .expect("board-session memory poisoned")
            .contains_key(&board_key(board))
    }

    /// Whether the closed-board file fallback may proceed for `board`.
    ///
    /// Returns `true` when the board was never observed live (no reason to doubt
    /// the file), or when it was observed live with its lock present and that lock
    /// is now absent - KiCad closed the document through its own path, so the saved
    /// file is authoritative and the observation is cleared. Refuses (returns
    /// `false`) while the lock is still present (crash, kill, or an unanswered
    /// close prompt) and when no lock was recorded at observation time.
    pub(crate) fn authorize_file_fallback(&self, board: &Path) -> bool {
        self.authorize_file_fallback_with(board, || editor_lock(board))
    }

    fn authorize_file_fallback_with(
        &self,
        board: &Path,
        inspect: impl FnOnce() -> EditorLock,
    ) -> bool {
        let key = board_key(board);
        let mut observed = self
            .observed_live
            .lock()
            .expect("board-session memory poisoned");
        let Some(lock_was_present) = observed.get(&key).copied() else {
            return true;
        };
        // Inspect under the memory lock so this evidence cannot clear a newer
        // live observation recorded concurrently.
        if lock_was_present && matches!(inspect(), EditorLock::Absent) {
            observed.remove(&key);
            return true;
        }
        false
    }
}

/// Prefer filesystem identity. If the path cannot be canonicalized, retain a
/// stable absolute lexical spelling instead of silently dropping the safety
/// observation.
fn board_key(board: &Path) -> PathBuf {
    board.canonicalize().unwrap_or_else(|_| {
        if board.is_absolute() {
            board.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(board))
                .unwrap_or_else(|_| board.to_path_buf())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board_with_lock(dir: &Path, name: &str) -> PathBuf {
        let board = dir.join(name);
        std::fs::write(&board, "").unwrap();
        std::fs::write(dir.join(format!("~{name}.lck")), "").unwrap();
        board
    }

    #[test]
    fn observations_are_sticky_idempotent_and_board_specific() {
        let dir = tempfile::tempdir().unwrap();
        let board_a = dir.path().join("a.kicad_pcb");
        let board_b = dir.path().join("b.kicad_pcb");
        std::fs::write(&board_a, "").unwrap();
        std::fs::write(&board_b, "").unwrap();
        let memory = BoardSessionMemory::default();

        assert!(!memory.was_observed_live(&board_a));
        memory.observe_live(&board_a);
        memory.observe_live(&board_a);

        assert!(memory.was_observed_live(&board_a));
        assert!(!memory.was_observed_live(&board_b));
    }

    #[test]
    fn canonical_equivalent_paths_share_one_observation() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        std::fs::write(&board, "").unwrap();
        let equivalent = dir.path().join("subdir").join("..").join("board.kicad_pcb");
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        let memory = BoardSessionMemory::default();

        memory.observe_live(&equivalent);

        assert!(memory.was_observed_live(&board));
    }

    #[test]
    fn fresh_memory_does_not_inherit_observations() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        std::fs::write(&board, "").unwrap();
        let first = BoardSessionMemory::default();
        first.observe_live(&board);

        assert!(!BoardSessionMemory::default().was_observed_live(&board));
    }

    #[test]
    fn never_observed_board_allows_file_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        std::fs::write(&board, "").unwrap();
        let memory = BoardSessionMemory::default();

        assert!(memory.authorize_file_fallback(&board));
    }

    #[test]
    fn lock_seen_then_gone_allows_file_fallback_and_clears_the_observation() {
        // Save then a clean close: KiCad removes the lock on exit (#671).
        let dir = tempfile::tempdir().unwrap();
        let board = board_with_lock(dir.path(), "board.kicad_pcb");
        let memory = BoardSessionMemory::default();
        memory.observe_live(&board);

        std::fs::remove_file(dir.path().join("~board.kicad_pcb.lck")).unwrap();

        assert!(memory.authorize_file_fallback(&board));
        assert!(!memory.was_observed_live(&board));
    }

    #[test]
    fn lock_present_refuses_even_when_ipc_is_gone() {
        // Edit then kill: the lock remains, so the save is not authoritative.
        let dir = tempfile::tempdir().unwrap();
        let board = board_with_lock(dir.path(), "board.kicad_pcb");
        let memory = BoardSessionMemory::default();
        memory.observe_live(&board);

        assert!(!memory.authorize_file_fallback(&board));
        assert!(memory.was_observed_live(&board));
    }

    #[test]
    fn unreadable_lock_does_not_release_an_observation() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_with_lock(dir.path(), "board.kicad_pcb");
        let memory = BoardSessionMemory::default();
        memory.observe_live(&board);
        let lock = super::super::live_board::editor_lock_with(&board, |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "denied",
            ))
        });
        assert!(!memory.authorize_file_fallback_with(&board, || lock));
        assert!(memory.was_observed_live(&board));
    }

    #[test]
    fn a_later_live_observation_starts_a_new_lock_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_with_lock(dir.path(), "board.kicad_pcb");
        let memory = BoardSessionMemory::default();
        memory.observe_live(&board);
        std::fs::remove_file(dir.path().join("~board.kicad_pcb.lck")).unwrap();
        // A new live binding without a lock invalidates earlier close evidence.
        memory.observe_live(&board);
        assert!(!memory.authorize_file_fallback(&board));
    }

    #[test]
    fn lock_never_observed_refuses() {
        // No lock at observation time means no close evidence to rely on.
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("board.kicad_pcb");
        std::fs::write(&board, "").unwrap();
        let memory = BoardSessionMemory::default();
        memory.observe_live(&board);

        assert!(!memory.authorize_file_fallback(&board));
    }

    #[test]
    fn another_board_closing_does_not_clear_this_one() {
        let dir = tempfile::tempdir().unwrap();
        let board_a = board_with_lock(dir.path(), "a.kicad_pcb");
        let board_b = board_with_lock(dir.path(), "b.kicad_pcb");
        let memory = BoardSessionMemory::default();
        memory.observe_live(&board_a);
        memory.observe_live(&board_b);

        std::fs::remove_file(dir.path().join("~a.kicad_pcb.lck")).unwrap();

        assert!(memory.authorize_file_fallback(&board_a));
        assert!(!memory.authorize_file_fallback(&board_b));
    }
}

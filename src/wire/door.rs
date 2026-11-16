//! The NAMES of the daemon's local doors on Windows, derived from the state dir —
//! defined once, here, because two programs derive them: the daemon, which
//! creates the named pipes, and the SDK, which opens the ingest
//! pipe with no daemon to ask. A second spelling would be a door the SDK
//! knocks on and nobody opened.
//!
//! On Unix a door is a FILE in the state dir (`exec.sock`, `ingest.sock`,
//! 0600). Windows has no socket file to put there, so each door is a named
//! pipe in the machine-wide pipe namespace, and the name must be unique per
//! state dir — two users, or two daemons of one user, on one machine must not
//! meet on one name: `phloemd[-<door>]-<id>`, `id` = the first 16 hex of
//! sha256 over the CANONICAL state dir (`std::fs::canonicalize`, so two
//! spellings of one directory name one pipe). The exec door keeps its
//! original name, `phloemd-<id>`; every later door says what it is.
//!
//! The name is not the capability: the daemon creates each pipe with an
//! owner-only DACL (`D:P(A;;GA;;;OW)`), refuses remote clients, and claims the
//! first instance, so another account can neither open the door nor have
//! created it first (the daemon serves the door the same way).
//!
//! Pure string work plus the one `canonicalize`, so it is testable on every
//! platform even though only Windows uses it.

use sha2::Digest as _;
use std::path::Path;

/// The first 16 hex of sha256 over the canonical `state_dir` — what makes a
/// door's pipe name this state dir's alone.
pub fn state_dir_id(state_dir: &Path) -> String {
    let canon = std::fs::canonicalize(state_dir).unwrap_or_else(|_| state_dir.to_path_buf());
    let digest = sha2::Sha256::digest(canon.to_string_lossy().as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// `\\.\pipe\phloemd-<id>` — the execution tier's pipe (`exec.sock`'s
/// Windows form).
pub fn exec_pipe_name(state_dir: &Path) -> String {
    format!(r"\\.\pipe\phloemd-{}", state_dir_id(state_dir))
}

/// `\\.\pipe\phloemd-ingest-<id>` — the SDK door's pipe (`ingest.sock`'s
/// Windows form): PHLM frames in, one welcome/refused frame out.
pub fn ingest_pipe_name(state_dir: &Path) -> String {
    format!(r"\\.\pipe\phloemd-ingest-{}", state_dir_id(state_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_door_is_named_per_state_dir_and_the_two_never_meet() {
        let a = std::env::temp_dir().join("phloem-door-a");
        let b = std::env::temp_dir().join("phloem-door-b");
        let id = state_dir_id(&a);
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(exec_pipe_name(&a), format!(r"\\.\pipe\phloemd-{id}"));
        assert_eq!(ingest_pipe_name(&a), format!(r"\\.\pipe\phloemd-ingest-{id}"));
        assert_ne!(exec_pipe_name(&a), ingest_pipe_name(&a));
        // Negative control: another state dir is another name.
        assert_ne!(ingest_pipe_name(&a), ingest_pipe_name(&b));
    }

    #[test]
    fn two_spellings_of_one_existing_dir_name_one_pipe() {
        // An existing directory every platform has, spelled twice.
        let dir = std::env::temp_dir();
        let roundabout = dir.join(".").join(".");
        assert_ne!(dir.as_os_str(), roundabout.as_os_str(), "two spellings");
        assert_eq!(ingest_pipe_name(&dir), ingest_pipe_name(&roundabout));
    }
}

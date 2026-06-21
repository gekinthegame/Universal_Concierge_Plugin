//! Locate Cursor's on-disk SQLite state database.
//!
//! Cursor stores chats in `state.vscdb` (globalStorage), table `cursorDiskKV` or `ItemTable`.
//! We locate the database file and count keys starting with `composerData:`.

use std::path::{Path, PathBuf};

/// One discovered Cursor database file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorSession {
    /// Absolute path to the `state.vscdb` file.
    pub file: PathBuf,
    /// Active sessions found within the database.
    pub session_count: usize,
}

/// The home directory, from `HOME`/`USERPROFILE`.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Scan roots: default SQLite database path candidates.
pub fn scan_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = home_dir() {
        if cfg!(target_os = "macos") {
            roots.push(
                home.join("Library/Application Support/Cursor/User/globalStorage/state.vscdb"),
            );
        } else if cfg!(target_os = "windows") {
            if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
                roots.push(appdata.join("Cursor/User/globalStorage/state.vscdb"));
            }
        } else {
            roots.push(home.join(".config/Cursor/User/globalStorage/state.vscdb"));
        }
    }
    if let Some(extra) = std::env::var_os("CONCIERGE_CURSOR_ROOTS") {
        for part in extra.to_string_lossy().split(':') {
            let part = part.trim();
            if !part.is_empty() {
                roots.push(PathBuf::from(part));
            }
        }
    }
    roots
}

/// Query database to count composerData entries.
pub fn count_sessions(db_path: &Path) -> usize {
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ) else {
        return 0;
    };

    let mut count = 0;
    for table in &["cursorDiskKV", "ItemTable"] {
        let query = format!(
            "SELECT COUNT(*) FROM {} WHERE key LIKE 'composerData:%'",
            table
        );
        if let Ok(c) = conn.query_row(&query, [], |row| row.get::<_, usize>(0)) {
            count += c;
            break; // If we successfully queried cursorDiskKV, we don't need to query ItemTable
        }
    }
    count
}

/// Discover all Cursor state database files.
pub fn discover() -> Vec<CursorSession> {
    let mut sessions = Vec::new();
    for root in scan_roots() {
        if root.exists() && root.is_file() {
            let count = count_sessions(&root);
            sessions.push(CursorSession {
                file: root,
                session_count: count,
            });
        }
    }
    sessions
}

/// Total sessions across all discovered databases.
pub fn total_sessions(sessions: &[CursorSession]) -> usize {
    sessions.iter().map(|s| s.session_count).sum()
}

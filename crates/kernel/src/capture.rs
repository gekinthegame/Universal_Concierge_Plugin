//! Kernel-owned harness capture loops.
//!
//! These mirror the GUI's consent-gated capture behavior, but live in the daemon
//! so capture can survive GUI restarts once callers opt into the kernel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use concierge_core::MemCli;

use crate::state::KernelState;

pub fn spawn_all(state: Arc<KernelState>) {
    spawn_claude_code_capture(Arc::clone(&state));
    spawn_aider_capture(Arc::clone(&state));
    spawn_codex_capture(Arc::clone(&state));
    spawn_gemini_capture(Arc::clone(&state));
    spawn_continue_capture(Arc::clone(&state));
    spawn_antigravity_capture(Arc::clone(&state));
    spawn_openclaw_capture(Arc::clone(&state));
    spawn_cline_capture(Arc::clone(&state));
    spawn_cursor_capture(Arc::clone(&state));
    spawn_opendevin_capture(Arc::clone(&state));
    spawn_copilot_capture(state);
}

fn flag_path(mem: &MemCli, flag: &str) -> Option<PathBuf> {
    mem.store_dir().ok().map(|dir| dir.join(flag))
}

fn attached(mem: &MemCli, flag: &str) -> bool {
    flag_path(mem, flag)
        .map(|path| path.exists())
        .unwrap_or(false)
}

fn atomic_local_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let tmp = path.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), nonce));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)?;
        use std::io::Write;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}

fn capture_offsets_path(mem: &MemCli) -> Option<PathBuf> {
    mem.store_dir()
        .ok()
        .map(|dir| dir.join("capture-offsets.json"))
}

fn load_capture_offsets(mem: &MemCli) -> HashMap<PathBuf, u64> {
    let Some(path) = capture_offsets_path(mem) else {
        return Default::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Default::default();
    };
    serde_json::from_str::<std::collections::BTreeMap<String, u64>>(&text)
        .map(|map| {
            map.into_iter()
                .map(|(k, v)| (PathBuf::from(k), v))
                .collect()
        })
        .unwrap_or_default()
}

fn save_capture_offsets(mem: &MemCli, offsets: &HashMap<PathBuf, u64>) {
    let Some(path) = capture_offsets_path(mem) else {
        return;
    };
    let map: std::collections::BTreeMap<String, u64> = offsets
        .iter()
        .map(|(k, v)| (k.display().to_string(), *v))
        .collect();
    if let Ok(text) = serde_json::to_string(&map) {
        let _ = atomic_local_write(&path, text.as_bytes());
    }
}

fn spawn_claude_code_capture(state: Arc<KernelState>) {
    use concierge_adapter_claude_code::{capture_once, discovery, seed_offsets};
    let Some(projects_dir) = discovery::claude_projects_dir() else {
        return;
    };
    let mem = state.mem();
    let base = mem.working_dir().to_path_buf();
    std::thread::spawn(move || {
        let mut offsets = load_capture_offsets(&mem);
        seed_offsets(&projects_dir, &mut offsets);
        save_capture_offsets(&mem, &offsets);
        loop {
            if attached(&mem, "capture-claude-code") {
                let ingested = capture_once(&projects_dir, &mut offsets, &mem, &base);
                if !ingested.is_empty() {
                    save_capture_offsets(&mem, &offsets);
                    let _ = state.append_index_records(&ingested);
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
    });
}

fn spawn_aider_capture(state: Arc<KernelState>) {
    use concierge_adapter_aider::{capture_once, seed_lens};
    let mem = state.mem();
    let base = mem.working_dir().to_path_buf();
    std::thread::spawn(move || {
        let mut lens: HashMap<PathBuf, u64> = HashMap::new();
        seed_lens(&mut lens);
        loop {
            if attached(&mem, "capture-aider") {
                let ingested = capture_once(&mut lens, &mem, &base);
                let _ = state.append_index_records(&ingested);
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    });
}

macro_rules! harness_capture {
    ($flag:literal, $spawn:ident, $cap:path, $seed:path, $stagger:expr) => {
        fn $spawn(state: Arc<KernelState>) {
            let mem = state.mem();
            let base = mem.working_dir().to_path_buf();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs($stagger));
                let mut lens: HashMap<PathBuf, u64> = HashMap::new();
                $seed(&mut lens);
                loop {
                    if attached(&mem, $flag) {
                        let ingested = $cap(&mut lens, &mem, &base);
                        let _ = state.append_index_records(&ingested);
                    }
                    std::thread::sleep(std::time::Duration::from_secs(20));
                }
            });
        }
    };
}

harness_capture!(
    "capture-codex",
    spawn_codex_capture,
    concierge_adapter_codex::capture_once,
    concierge_adapter_codex::seed_lens,
    4
);
harness_capture!(
    "capture-gemini",
    spawn_gemini_capture,
    concierge_adapter_gemini::capture_once,
    concierge_adapter_gemini::seed_lens,
    10
);
harness_capture!(
    "capture-continue",
    spawn_continue_capture,
    concierge_adapter_continue::capture_once,
    concierge_adapter_continue::seed_lens,
    16
);
harness_capture!(
    "capture-antigravity",
    spawn_antigravity_capture,
    concierge_adapter_antigravity::capture_once,
    concierge_adapter_antigravity::seed_lens,
    22
);
harness_capture!(
    "capture-openclaw",
    spawn_openclaw_capture,
    concierge_adapter_openclaw::capture_once,
    concierge_adapter_openclaw::seed_lens,
    28
);
harness_capture!(
    "capture-cline",
    spawn_cline_capture,
    concierge_adapter_cline::capture_once,
    concierge_adapter_cline::seed_lens,
    34
);
harness_capture!(
    "capture-cursor",
    spawn_cursor_capture,
    concierge_adapter_cursor::capture_once,
    concierge_adapter_cursor::seed_lens,
    40
);
harness_capture!(
    "capture-opendevin",
    spawn_opendevin_capture,
    concierge_adapter_opendevin::capture_once,
    concierge_adapter_opendevin::seed_lens,
    46
);
harness_capture!(
    "capture-copilot",
    spawn_copilot_capture,
    concierge_adapter_copilot::capture_once,
    concierge_adapter_copilot::seed_lens,
    52
);

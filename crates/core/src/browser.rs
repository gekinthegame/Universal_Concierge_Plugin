//! Read bookmarks from the user's wallet browser (Brave or Opera) → memory
//! (Pillar A, Decision 0033). **Read-only** — we never modify the browser's files.
//! Both browsers are Chromium, so both keep a `Bookmarks` JSON in their profile;
//! Opera's path differs and its custom bookmark UI may not always populate the
//! Chromium file, so Opera support is best-effort (an empty/absent file → 0 synced).

use std::collections::HashSet;
use std::path::PathBuf;

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// A supported Chromium wallet browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Browser {
    Brave,
    Opera,
}

impl Browser {
    pub fn label(self) -> &'static str {
        match self {
            Browser::Brave => "Brave",
            Browser::Opera => "Opera",
        }
    }
}

/// One bookmark flattened from the nested Chromium roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
    pub folder: String,
    /// Unix seconds the user added it (0 if unknown).
    pub added_unix: u64,
}

/// Stable per-URL key for dedup + day-event keys.
pub fn url_key(url: &str) -> String {
    Sha256::digest(url.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Candidate `Bookmarks` file locations for a browser, across OSes (env vars only
/// exist on their platform, so non-matching candidates simply don't resolve).
fn candidate_files(browser: Browser) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
    let localappdata = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let mut out = Vec::new();
    let mut profile = |base: PathBuf| {
        out.push(base.join("Default").join("Bookmarks"));
        out.push(base.join("Bookmarks"));
    };
    match browser {
        Browser::Brave => {
            if let Some(h) = &home {
                profile(h.join("Library/Application Support/BraveSoftware/Brave-Browser"));
                profile(h.join(".config/BraveSoftware/Brave-Browser"));
            }
            if let Some(l) = &localappdata {
                profile(l.join("BraveSoftware/Brave-Browser/User Data"));
            }
        }
        Browser::Opera => {
            if let Some(h) = &home {
                profile(h.join("Library/Application Support/com.operasoftware.Opera"));
                profile(h.join(".config/opera"));
            }
            if let Some(a) = &appdata {
                profile(a.join("Opera Software/Opera Stable"));
            }
        }
    }
    out
}

/// The first existing `Bookmarks` file for a browser, if any.
pub fn bookmarks_file(browser: Browser) -> Option<PathBuf> {
    candidate_files(browser).into_iter().find(|p| p.exists())
}

// ── Chromium `Bookmarks` JSON (only the fields we need) ──────────────────────

#[derive(Deserialize)]
struct BookmarksFile {
    roots: Roots,
}
#[derive(Deserialize)]
struct Roots {
    #[serde(default)]
    bookmark_bar: Option<BNode>,
    #[serde(default)]
    other: Option<BNode>,
    #[serde(default)]
    synced: Option<BNode>,
}
#[derive(Deserialize)]
struct BNode {
    #[serde(rename = "type", default)]
    node_type: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    date_added: Option<String>,
    #[serde(default)]
    children: Vec<BNode>,
}

/// Parse a Chromium `Bookmarks` JSON string into a flat, URL-deduplicated list.
pub fn parse_bookmarks(json: &str) -> Result<Vec<Bookmark>, String> {
    let file: BookmarksFile =
        serde_json::from_str(json).map_err(|e| format!("parse Bookmarks: {e}"))?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for root in [file.roots.bookmark_bar, file.roots.other, file.roots.synced]
        .into_iter()
        .flatten()
    {
        let folder = root.name.clone();
        walk(&root, &folder, &mut out, &mut seen);
    }
    Ok(out)
}

fn walk(node: &BNode, folder: &str, out: &mut Vec<Bookmark>, seen: &mut HashSet<String>) {
    if node.node_type == "url" {
        if let Some(url) = &node.url {
            if !url.is_empty() && seen.insert(url.clone()) {
                out.push(Bookmark {
                    url: url.clone(),
                    title: if node.name.is_empty() { url.clone() } else { node.name.clone() },
                    folder: folder.to_string(),
                    added_unix: node
                        .date_added
                        .as_deref()
                        .and_then(chromium_micros_to_unix)
                        .unwrap_or(0),
                });
            }
        }
    }
    for child in &node.children {
        let next = if child.node_type == "folder" { child.name.as_str() } else { folder };
        walk(child, next, out, seen);
    }
}

/// Chromium stores `date_added` as microseconds since 1601-01-01 UTC, as a string.
fn chromium_micros_to_unix(s: &str) -> Option<u64> {
    let micros: u64 = s.trim().parse().ok()?;
    (micros / 1_000_000).checked_sub(11_644_473_600) // 1601 → 1970 offset
}

/// Read + merge bookmarks from every supported browser (deduplicated by URL).
pub fn read_bookmarks() -> Vec<Bookmark> {
    // Test / explicit-override hook: read a single specified `Bookmarks` file.
    if let Some(path) = std::env::var_os("CONCIERGE_BOOKMARKS_FILE") {
        return std::fs::read_to_string(path)
            .ok()
            .and_then(|json| parse_bookmarks(&json).ok())
            .unwrap_or_default();
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for browser in [Browser::Brave, Browser::Opera] {
        let Some(path) = bookmarks_file(browser) else { continue };
        let Ok(json) = std::fs::read_to_string(&path) else { continue };
        let Ok(marks) = parse_bookmarks(&json) else { continue };
        for m in marks {
            if seen.insert(m.url.clone()) {
                out.push(m);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "version": 1,
      "roots": {
        "bookmark_bar": { "type": "folder", "name": "Bookmarks bar", "children": [
          { "type": "url", "name": "IPFS paper", "url": "https://ipfs.tech/paper",
            "date_added": "13350000000000000" },
          { "type": "folder", "name": "Research", "children": [
            { "type": "url", "name": "libp2p", "url": "https://libp2p.io" },
            { "type": "url", "name": "dup", "url": "https://ipfs.tech/paper" }
          ]}
        ]},
        "other": { "type": "folder", "name": "Other bookmarks", "children": [
          { "type": "url", "name": "", "url": "https://example.com" }
        ]}
      }
    }"#;

    #[test]
    fn parses_and_dedupes_and_tracks_folders() {
        let marks = parse_bookmarks(SAMPLE).unwrap();
        let urls: Vec<&str> = marks.iter().map(|m| m.url.as_str()).collect();
        assert!(urls.contains(&"https://ipfs.tech/paper"));
        assert!(urls.contains(&"https://libp2p.io"));
        assert!(urls.contains(&"https://example.com"));
        // The duplicate URL appears once.
        assert_eq!(urls.iter().filter(|u| **u == "https://ipfs.tech/paper").count(), 1);
        // Folder is tracked; empty title falls back to the URL.
        let libp2p = marks.iter().find(|m| m.url == "https://libp2p.io").unwrap();
        assert_eq!(libp2p.folder, "Research");
        let ex = marks.iter().find(|m| m.url == "https://example.com").unwrap();
        assert_eq!(ex.title, "https://example.com");
        // Chromium timestamp converts into a sane unix range (the 2020s).
        let paper = marks.iter().find(|m| m.url == "https://ipfs.tech/paper").unwrap();
        assert!(paper.added_unix > 1_600_000_000);
    }

    #[test]
    fn url_key_is_stable_hex() {
        let k = url_key("https://ipfs.tech/paper");
        assert_eq!(k.len(), 64);
        assert_eq!(k, url_key("https://ipfs.tech/paper"));
        assert_ne!(k, url_key("https://libp2p.io"));
    }

    #[test]
    fn detection_is_callable() {
        // Environment-dependent: just must not panic, and any returned path exists.
        for b in [Browser::Brave, Browser::Opera] {
            if let Some(p) = bookmarks_file(b) {
                assert!(p.exists());
            }
        }
    }
}

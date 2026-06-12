//! Media front-ends for "publish your own catalog" (FUTURE_VISION §3). Given a
//! folder of the user's own media, write an `index.html` so the published UnixFS
//! directory is a browsable gallery / player — pure string templating, no deps.
//! For a plain site (`SiteKind::Site`) we assume the folder already has its own
//! `index.html` and leave it untouched.

use std::path::Path;

use crate::error::{Error, Result};

/// What kind of front-end to stage before publishing a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteKind {
    /// The folder is already a website (has its own `index.html`).
    Site,
    /// Generate an image/video grid over the folder's media.
    Gallery,
    /// Generate a single media player with a playlist over the folder's media.
    Player,
}

impl SiteKind {
    pub fn parse(value: &str) -> SiteKind {
        match value {
            "gallery" => SiteKind::Gallery,
            "player" => SiteKind::Player,
            _ => SiteKind::Site,
        }
    }
}

const IMAGE_EXT: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "svg", "avif"];
const VIDEO_EXT: &[&str] = &["mp4", "webm", "mov", "mkv"];
const AUDIO_EXT: &[&str] = &["mp3", "wav", "ogg", "flac", "m4a"];

fn ext_of(name: &str) -> String {
    name.rsplit('.').next().unwrap_or("").to_ascii_lowercase()
}

fn is_media(name: &str) -> bool {
    let e = ext_of(name);
    IMAGE_EXT.contains(&e.as_str())
        || VIDEO_EXT.contains(&e.as_str())
        || AUDIO_EXT.contains(&e.as_str())
}

/// Top-level media file names in `folder` (sorted), excluding any existing
/// `index.html`. Only the directory's own entries — sites stay flat + simple.
fn media_files(folder: &Path) -> Result<Vec<String>> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .map_err(|e| Error::Io(format!("read folder: {e}")))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| is_media(name))
        .collect();
    names.sort();
    Ok(names)
}

fn esc(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn tile(name: &str) -> String {
    let e = ext_of(name);
    let src = esc(name);
    if VIDEO_EXT.contains(&e.as_str()) {
        format!("<video class=\"tile\" src=\"{src}\" controls preload=\"metadata\"></video>")
    } else if AUDIO_EXT.contains(&e.as_str()) {
        format!("<div class=\"tile audio\"><div class=\"cap\">{src}</div><audio src=\"{src}\" controls></audio></div>")
    } else {
        format!("<a class=\"tile\" href=\"{src}\" target=\"_blank\"><img loading=\"lazy\" src=\"{src}\" alt=\"{src}\"></a>")
    }
}

const STYLE: &str = "<style>\
:root{color-scheme:dark}\
body{margin:0;background:#0a0a14;color:#e8e6f0;font:15px/1.5 -apple-system,Segoe UI,Roboto,sans-serif}\
header{padding:26px 22px;border-bottom:1px solid #221f3a}\
h1{margin:0;font-size:20px;letter-spacing:.04em}\
.sub{color:#8a86a8;font-size:12px;margin-top:5px}\
.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(220px,1fr));gap:12px;padding:16px}\
.tile{display:block;border:1px solid #221f3a;border-radius:8px;overflow:hidden;background:#13111f}\
.tile img,.tile video{display:block;width:100%;height:100%;object-fit:cover}\
.tile.audio{padding:14px}\
.cap{font:12px monospace;color:#a855f7;margin-bottom:8px;overflow:hidden;text-overflow:ellipsis}\
audio{width:100%}\
.player{max-width:900px;margin:24px auto;padding:0 16px}\
.stage video,.stage img,.stage audio{width:100%;border-radius:10px;background:#000}\
.list{margin-top:14px;display:flex;flex-direction:column;gap:6px}\
.list button{text-align:left;padding:9px 12px;border:1px solid #221f3a;border-radius:6px;background:#13111f;color:#e8e6f0;font:13px monospace;cursor:pointer}\
.list button:hover{border-color:#a855f7}\
footer{padding:18px 22px;color:#56516f;font:11px monospace;border-top:1px solid #221f3a}\
</style>";

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>{t}</title>{STYLE}</head><body>{body}\
<footer>Published from a Concierge node · sovereign, no middleman</footer></body></html>",
        t = esc(title)
    )
}

/// Build a responsive grid gallery `index.html` over the folder's media.
fn gallery_html(title: &str, files: &[String]) -> String {
    let tiles: String = files.iter().map(|name| tile(name)).collect();
    let body = format!(
        "<header><h1>{t}</h1><div class=\"sub\">{n} item(s)</div></header><div class=\"grid\">{tiles}</div>",
        t = esc(title),
        n = files.len()
    );
    page(title, &body)
}

/// Build a single-stage player + clickable playlist `index.html`.
fn player_html(title: &str, files: &[String]) -> String {
    let first = files.first().cloned().unwrap_or_default();
    let e = ext_of(&first);
    let stage = if VIDEO_EXT.contains(&e.as_str()) {
        format!("<video id=\"stage\" src=\"{}\" controls autoplay></video>", esc(&first))
    } else if AUDIO_EXT.contains(&e.as_str()) {
        format!("<audio id=\"stage\" src=\"{}\" controls autoplay></audio>", esc(&first))
    } else {
        format!("<img id=\"stage\" src=\"{}\" alt=\"\">", esc(&first))
    };
    let list: String = files
        .iter()
        .map(|name| format!("<button data-src=\"{s}\">{s}</button>", s = esc(name)))
        .collect();
    let script = "<script>\
const stage=document.getElementById('stage');\
document.querySelectorAll('.list button').forEach(b=>b.onclick=()=>{\
const s=b.dataset.src;const ext=s.split('.').pop().toLowerCase();\
const vid=['mp4','webm','mov','mkv'].includes(ext);const aud=['mp3','wav','ogg','flac','m4a'].includes(ext);\
const tag=vid?'video':aud?'audio':'img';\
const el=document.createElement(tag);el.id='stage';el.src=s;\
if(vid||aud){el.controls=true;el.autoplay=true;}\
stage.replaceWith(el);});\
</script>";
    let body = format!(
        "<div class=\"player\"><header style=\"padding:0 0 14px;border:0\"><h1>{t}</h1></header>\
<div class=\"stage\">{stage}</div><div class=\"list\">{list}</div></div>{script}",
        t = esc(title)
    );
    page(title, &body)
}

/// Stage a front-end into `folder` before publishing. For `Gallery`/`Player`,
/// writes (overwrites) `index.html` over the folder's media. For `Site`, requires
/// an existing `index.html` and leaves the folder untouched.
pub fn write_index(folder: &Path, kind: SiteKind, title: &str) -> Result<()> {
    match kind {
        SiteKind::Site => {
            if !folder.join("index.html").is_file() {
                return Err(Error::Io(
                    "folder has no index.html — pick Gallery/Player to generate one, or add your own index.html".to_string(),
                ));
            }
            Ok(())
        }
        SiteKind::Gallery | SiteKind::Player => {
            let files = media_files(folder)?;
            if files.is_empty() {
                return Err(Error::Io("no media files found in the folder".to_string()));
            }
            let html = if kind == SiteKind::Gallery {
                gallery_html(title, &files)
            } else {
                player_html(title, &files)
            };
            std::fs::write(folder.join("index.html"), html)
                .map_err(|e| Error::Io(format!("write index.html: {e}")))
        }
    }
}

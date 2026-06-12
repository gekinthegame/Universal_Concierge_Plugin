//! Whole-plan worker handoff helpers.
//!
//! The REPL/session owns the orchestration. This module owns the worker
//! contract: the Concierge writes a plan and manifest, the worker builds the
//! first draft one file at a time, and audit-driven repair may use project-bound
//! tools on concrete cleanup findings.

use crate::blockstore::LocalBlocks;
use crate::cid::Cid;
use crate::node::{Blob, Edge, EdgeRel, FileRef, Node, Source};
use crate::store::{Store, SystemClock};
use anyhow::Context;
use atomic_write_file::AtomicWriteFile;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use walkdir::WalkDir;

/// A safety cap on the optional Concierge file guidance, so a runaway manifest
/// cannot bloat the worker prompt forever.
pub(crate) const MAX_MANIFEST_FILES: usize = 200;

/// Cap on scoped repair re-entry laps before giving up. A no-progress guard
/// (the audit must shrink each lap) usually stops sooner.
pub(crate) const MAX_REPAIR_LAPS: usize = 3;

const TOOL_READ_LIMIT: usize = 80_000;
const TOOL_COMMAND_TAIL_BYTES: usize = 12_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkFile {
    pub path: String,
    pub cid: Cid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkReport {
    pub plan: Cid,
    pub worker_response: Cid,
    pub files: Vec<WorkFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkTrace {
    pub label: String,
    pub model: Option<String>,
    pub body: String,
    pub stream: bool,
}

impl WorkTrace {
    pub(crate) fn stage(
        label: impl Into<String>,
        model: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            model: Some(model.into()),
            body: body.into(),
            stream: false,
        }
    }

    pub(crate) fn note(label: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            model: None,
            body: body.into(),
            stream: false,
        }
    }
}

/// A file the worker produced: its project-relative path and full contents.
/// Recorded so the harness can persist a memory node after writing to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileWrite {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectToolResult {
    pub output: String,
    pub ok: bool,
    pub done: bool,
    pub write: Option<FileWrite>,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolRequest {
    pub name: String,
    pub arguments: serde_json::Value,
}

/// The Concierge's manifest prompt. From the goal and the plan it just wrote,
/// the Concierge lists every file the project needs as newline-delimited
/// project-relative paths, dependencies before dependents.
pub(crate) fn manifest_prompt_text(goal: &str, plan: &str) -> String {
    format!(
        "You are the Concierge. List every file the project needs to satisfy the \
         goal and plan below. Return a plain newline-delimited list of \
         project-relative file paths. Order the files so a file appears after \
         the files it depends on.\n\n\
         Goal:\n{goal}\n\n\
         Plan:\n{plan}"
    )
}

/// Parse the Concierge's manifest reply into an ordered, de-duplicated list of
/// project-relative paths. Lenient: tolerates bullets, numbering, backticks and
/// blank lines, and drops anything that does not look like a file path.
pub(crate) fn parse_manifest(reply: &str) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for line in reply.lines() {
        let Some(path) = clean_manifest_line(line) else {
            continue;
        };
        if looks_like_path(&path) && !paths.contains(&path) {
            paths.push(path);
        }
        if paths.len() >= MAX_MANIFEST_FILES {
            break;
        }
    }
    paths
}

fn clean_manifest_line(line: &str) -> Option<String> {
    let mut s = line.trim();
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(bullet) {
            s = rest.trim_start();
            break;
        }
    }
    s = strip_ordinal(s);
    let s = s.trim().trim_matches('`').trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Strip a leading `12. ` or `12) ` list ordinal, if present.
fn strip_ordinal(s: &str) -> &str {
    let trimmed = s.trim_start();
    let digit_len = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digit_len == 0 {
        return s;
    }
    let rest = &trimmed[digit_len..];
    match rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')) {
        Some(after) => after.trim_start(),
        None => s,
    }
}

/// The per-file worker prompt. The worker never sees the raw user prompt — only
/// the plan, the full manifest, the files already written, and the single file
/// it must write now. Each already-written file is listed with the exact names
/// it exports, so the worker imports real symbols instead of guessing a
/// neighbour's API. It is asked for exactly one fenced code block: the complete
/// contents of that file.
pub(crate) fn file_prompt_text(
    plan: &str,
    manifest: &[String],
    written: &[FileWrite],
    target: &str,
) -> String {
    let manifest_list = manifest.join("\n");
    let written_list = if written.is_empty() {
        "(none yet)".to_string()
    } else {
        written
            .iter()
            .map(|file| {
                let symbols = public_symbols(&file.content);
                if symbols.is_empty() {
                    format!("{} (nothing public detected)", file.path)
                } else {
                    format!("{} — defines: {}", file.path, symbols.join(", "))
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "You are the workhorse building one file of a larger project.\n\n\
         Project plan:\n{plan}\n\n\
         Full file manifest (every file the project will contain):\n{manifest_list}\n\n\
         Files already written and public names available for imports:\n{written_list}\n\n\
         Write the complete contents of this one file:\n{target}\n\n\
         Return a single fenced code block containing the entire file body."
    )
}

fn looks_like_path(s: &str) -> bool {
    !s.is_empty()
        && !s.contains(char::is_whitespace)
        && (s.contains('/') || s.contains('.'))
        && !s.starts_with('#')
}

/// Alignment prompt for repair re-entry. The original goal, original plan, and
/// original manifest stay authoritative; deterministic audit evidence supplies
/// the concrete mismatch list.
pub(crate) struct AlignmentPrompt<'a> {
    pub goal: &'a str,
    pub project_plan: &'a str,
    pub manifest: &'a [String],
    pub written: &'a [FileWrite],
    pub target: &'a str,
    pub current: &'a str,
    pub findings: &'a str,
    pub audit: &'a str,
}

pub(crate) fn alignment_prompt_text(input: AlignmentPrompt<'_>) -> String {
    let manifest_list = input.manifest.join("\n");
    let project_files = if input.written.is_empty() {
        "(none)".to_string()
    } else {
        input
            .written
            .iter()
            .map(|file| {
                let symbols = public_symbols(&file.content);
                if symbols.is_empty() {
                    format!("{} (nothing public detected)", file.path)
                } else {
                    format!("{} — defines: {}", file.path, symbols.join(", "))
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "You are the workhorse re-entering the river for an alignment pass. \
         Bring the current project into agreement with the original goal, \
         original plan, original manifest, current file map, and deterministic \
         audit findings. Align imports, exports, scaffold metadata, entrypoints, \
         dependencies, styling, and implementation details across the whole \
         project.\n\n\
         Original goal:\n{}\n\n\
         Original project plan:\n{}\n\n\
         Original file manifest:\n{manifest_list}\n\n\
         Current project files and public symbols:\n{project_files}\n\n\
         Full audit findings:\n{}\n\n\
         File for this alignment step: {}\n\n\
         Current contents:\n{}\n\n\
         Findings for this file:\n{}\n\n\
         Tool request option: return a JSON object with `name` and `arguments` \
         for read_file, write_file, edit_file, run_command, or list_files.\n\n\
         File response option: return a single fenced code block containing the \
         complete aligned file body.",
        input.goal, input.project_plan, input.audit, input.target, input.current, input.findings,
    )
}

/// Best-effort extraction of the public names a source file defines, so later
/// files import real symbols instead of guessing a neighbour's API. Line-based
/// and deliberately lenient, and not tied to one language: it recognises
/// explicit visibility markers (`export`, `pub`, `pub(crate)`, `public`) in
/// front of a declaration keyword, plus the public-by-position declarations of
/// languages like Python and Go (top-level `def`/`class`/`func`). It is never
/// complete — an unrecognised form simply contributes no name.
pub(crate) fn public_symbols(content: &str) -> Vec<String> {
    // Keywords whose following identifier names the declaration.
    const DECL: &[&str] = &[
        "class ",
        "interface ",
        "enum ",
        "struct ",
        "record ",
        "trait ",
        "function ",
        "fn ",
        "def ",
        "func ",
        "const ",
        "let ",
        "var ",
        "type ",
        "namespace ",
        "mod ",
        "static ",
        "union ",
        "val ",
    ];
    // Declarations that are public by position (no visibility keyword) when
    // written at column zero, as in Python and Go.
    const IMPLICIT: &[&str] = &["def ", "class ", "func "];

    let mut names: Vec<String> = Vec::new();
    let push = |name: &str, names: &mut Vec<String>| {
        if !name.is_empty() && !names.iter().any(|existing| existing == name) {
            names.push(name.to_string());
        }
    };

    for raw in content.lines() {
        let line = raw.trim_start();

        // Grouped exports / re-exports: `export { a, b as c }`.
        if let Some(inner) = line
            .strip_prefix("export ")
            .map(str::trim_start)
            .and_then(|rest| rest.strip_prefix('{'))
        {
            if let Some(end) = inner.find('}') {
                for part in inner[..end].split(',') {
                    // `a as b` exports `b`; otherwise the token itself.
                    let name = part.split_whitespace().last().unwrap_or("");
                    push(&leading_ident(name), &mut names);
                }
            }
            continue;
        }

        // The declaration body: after an explicit visibility marker, or — for an
        // unindented Python/Go declaration — the line itself.
        let decl = match strip_visibility(line) {
            Some(rest) => Some(strip_modifiers(rest)),
            None if !raw.starts_with([' ', '\t']) => {
                let bare = line.strip_prefix("async ").unwrap_or(line);
                IMPLICIT
                    .iter()
                    .any(|kw| bare.starts_with(kw))
                    .then_some(bare)
            }
            None => None,
        };

        if let Some(decl) = decl {
            if let Some(rest) = DECL.iter().find_map(|kw| decl.strip_prefix(kw)) {
                push(&leading_ident(rest.trim_start()), &mut names);
            }
        }
    }
    names
}

/// Strip a leading public-visibility marker, returning the remainder if one was
/// present (`export`, `export default`, `public`, `pub`, `pub(crate)`, …).
fn strip_visibility(line: &str) -> Option<&str> {
    for vis in ["export default ", "export ", "public ", "pub "] {
        if let Some(rest) = line.strip_prefix(vis) {
            return Some(rest.trim_start());
        }
    }
    // `pub(crate)`, `pub(super)`, `pub(in path)` …
    line.strip_prefix("pub(")
        .and_then(|rest| rest.find(')').map(|close| rest[close + 1..].trim_start()))
}

/// Drop declaration modifiers that can sit between visibility and the keyword.
fn strip_modifiers(s: &str) -> &str {
    let mut s = s.trim_start();
    loop {
        match [
            "default ",
            "abstract ",
            "async ",
            "declare ",
            "static ",
            "final ",
            "readonly ",
        ]
        .iter()
        .find_map(|m| s.strip_prefix(m))
        {
            Some(rest) => s = rest.trim_start(),
            None => return s,
        }
    }
}

/// The leading identifier of `s` (ASCII letters, digits, `_`, `$`), if any.
fn leading_ident(s: &str) -> String {
    s.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
        .collect()
}

/// Pull the file body out of the worker's reply: the contents of the first
/// fenced code block. If there is no fence, fall back to the whole trimmed reply
/// (the worker may have returned the raw file).
pub(crate) fn extract_file_body(reply: &str) -> String {
    let mut in_block = false;
    let mut found_fence = false;
    let mut body = String::new();
    for line in reply.lines() {
        let is_fence = line.trim_start().starts_with("```");
        if !in_block {
            if is_fence {
                in_block = true;
                found_fence = true;
            }
            continue;
        }
        if is_fence {
            return body;
        }
        body.push_str(line);
        body.push('\n');
    }
    if found_fence {
        body
    } else {
        reply.trim().to_string()
    }
}

/// Validate a project-relative path and write the file to disk, creating parent
/// directories. Returns the record so the file can later be persisted as a node.
pub(crate) fn write_workspace_file(
    workspace_root: &Path,
    path: &str,
    content: &str,
) -> anyhow::Result<FileWrite> {
    let destination = workspace_path(workspace_root, path)?;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = AtomicWriteFile::open(&destination)?;
    file.write_all(content.as_bytes())?;
    file.commit()?;
    Ok(FileWrite {
        path: path.to_string(),
        content: content.to_string(),
    })
}

pub(crate) fn execute_project_tool(
    workspace_root: &Path,
    name: &str,
    arguments: &serde_json::Value,
) -> ProjectToolResult {
    match execute_project_tool_inner(workspace_root, name, arguments) {
        Ok(result) => result,
        Err(err) => ProjectToolResult {
            output: err.to_string(),
            ok: false,
            done: false,
            write: None,
            label: Some(format!("Tool error: {name}")),
        },
    }
}

pub(crate) fn parse_tool_request(reply: &str) -> Option<ToolRequest> {
    let text = extract_json_body(reply)?;
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let object = value.as_object()?;
    let name = object
        .get("name")
        .or_else(|| object.get("tool"))
        .and_then(|value| value.as_str())?;
    if !is_project_tool_name(name) {
        return None;
    }
    let arguments = object
        .get("arguments")
        .or_else(|| object.get("args"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    Some(ToolRequest {
        name: name.to_string(),
        arguments,
    })
}

fn is_project_tool_name(name: &str) -> bool {
    matches!(
        name,
        "list_files" | "read_file" | "write_file" | "edit_file" | "run_command" | "finish"
    )
}

fn extract_json_body(reply: &str) -> Option<&str> {
    let trimmed = reply.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed);
    }
    let mut lines = reply.lines();
    let first = lines.next()?.trim_start();
    if !first.starts_with("```") {
        return None;
    }
    let mut body = "";
    let mut end_seen = false;
    for line in lines {
        if line.trim_start().starts_with("```") {
            end_seen = true;
            break;
        }
        if body.is_empty() {
            body = line;
        } else {
            return None;
        }
    }
    let body = body.trim();
    (end_seen && body.starts_with('{') && body.ends_with('}')).then_some(body)
}

fn execute_project_tool_inner(
    workspace_root: &Path,
    name: &str,
    arguments: &serde_json::Value,
) -> anyhow::Result<ProjectToolResult> {
    let args = tool_args_object(arguments)?;
    match name {
        "list_files" => {
            let files = project_file_map(workspace_root)?;
            Ok(ProjectToolResult {
                output: files,
                ok: true,
                done: false,
                write: None,
                label: Some("Listed files".to_string()),
            })
        }
        "read_file" => {
            let path = required_string(&args, "path")?;
            let full_path = workspace_path(workspace_root, path)?;
            let content = fs::read_to_string(&full_path)
                .with_context(|| format!("read {}", display_rel(path)))?;
            let output = if content.len() > TOOL_READ_LIMIT {
                format!(
                    "{}\n\n[truncated after {} bytes]",
                    &content[..TOOL_READ_LIMIT],
                    TOOL_READ_LIMIT
                )
            } else {
                content
            };
            Ok(ProjectToolResult {
                output,
                ok: true,
                done: false,
                write: None,
                label: Some(format!("Read {}", display_rel(path))),
            })
        }
        "write_file" => {
            let path = required_string(&args, "path")?;
            let content = required_string(&args, "content")?;
            let write = write_workspace_file(workspace_root, path, content)?;
            Ok(ProjectToolResult {
                output: format!("wrote {} ({} bytes)", display_rel(path), content.len()),
                ok: true,
                done: false,
                write: Some(write),
                label: Some(format!("Wrote {}", display_rel(path))),
            })
        }
        "edit_file" => {
            let path = required_string(&args, "path")?;
            let old = required_string(&args, "old")?;
            let new = required_string(&args, "new")?;
            if old.is_empty() {
                anyhow::bail!("old text cannot be empty");
            }
            let replace_all = optional_bool(&args, "replace_all").unwrap_or(false);
            let full_path = workspace_path(workspace_root, path)?;
            let current = fs::read_to_string(&full_path)
                .with_context(|| format!("read {}", display_rel(path)))?;
            if !current.contains(old) {
                anyhow::bail!("old text not found in {}", display_rel(path));
            }
            let updated = if replace_all {
                current.replace(old, new)
            } else {
                current.replacen(old, new, 1)
            };
            let write = write_workspace_file(workspace_root, path, &updated)?;
            Ok(ProjectToolResult {
                output: format!("edited {} ({} bytes)", display_rel(path), updated.len()),
                ok: true,
                done: false,
                write: Some(write),
                label: Some(format!("Edited {}", display_rel(path))),
            })
        }
        "run_command" => {
            let command = parse_tool_command(&args)?;
            let result = run_workspace_command(workspace_root, &command)?;
            Ok(ProjectToolResult {
                output: result.render(),
                ok: result.exit_code == Some(0) && !result.timed_out,
                done: false,
                write: None,
                label: Some(format!("Ran {}", command.display())),
            })
        }
        "finish" => {
            let summary = optional_string(&args, "summary")
                .filter(|summary| !summary.trim().is_empty())
                .unwrap_or("finished");
            Ok(ProjectToolResult {
                output: summary.to_string(),
                ok: true,
                done: true,
                write: None,
                label: Some("Worker finished".to_string()),
            })
        }
        other => anyhow::bail!("unknown project tool: {other}"),
    }
}

pub(crate) fn upsert_write(writes: &mut Vec<FileWrite>, write: FileWrite) {
    if let Some(existing) = writes
        .iter_mut()
        .find(|existing| existing.path == write.path)
    {
        *existing = write;
    } else {
        writes.push(write);
    }
}

fn tool_args_object(
    arguments: &serde_json::Value,
) -> anyhow::Result<serde_json::Map<String, serde_json::Value>> {
    match arguments {
        serde_json::Value::Object(map) => Ok(map.clone()),
        serde_json::Value::String(s) => match serde_json::from_str::<serde_json::Value>(s)? {
            serde_json::Value::Object(map) => Ok(map),
            _ => anyhow::bail!("tool arguments string must decode to a JSON object"),
        },
        serde_json::Value::Null => Ok(serde_json::Map::new()),
        _ => anyhow::bail!("tool arguments must be a JSON object"),
    }
}

fn required_string<'a>(
    args: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> anyhow::Result<&'a str> {
    optional_string(args, key).ok_or_else(|| anyhow::anyhow!("missing string argument `{key}`"))
}

fn optional_string<'a>(
    args: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<&'a str> {
    args.get(key).and_then(|value| value.as_str())
}

fn optional_bool(args: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<bool> {
    args.get(key).and_then(|value| value.as_bool())
}

fn optional_u64(args: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<u64> {
    args.get(key).and_then(|value| value.as_u64())
}

fn display_rel(path: &str) -> String {
    path.trim().trim_start_matches("./").to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceCommand {
    program: String,
    args: Vec<String>,
    timeout: Duration,
    allow_network: bool,
}

impl WorkspaceCommand {
    fn display(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceCommandResult {
    command: String,
    isolation: &'static str,
    exit_code: Option<i32>,
    timed_out: bool,
    stdout_tail: String,
    stderr_tail: String,
}

impl WorkspaceCommandResult {
    fn render(&self) -> String {
        format!(
            "command: {}\nisolation: {}\nexit_code: {}\ntimed_out: {}\nstdout_tail:\n{}\nstderr_tail:\n{}",
            self.command,
            self.isolation,
            self.exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.timed_out,
            self.stdout_tail,
            self.stderr_tail
        )
    }
}

fn parse_tool_command(
    args: &serde_json::Map<String, serde_json::Value>,
) -> anyhow::Result<WorkspaceCommand> {
    let timeout = Duration::from_secs(
        optional_u64(args, "timeout_seconds")
            .unwrap_or(120)
            .clamp(1, 600),
    );
    let (program, command_args) = if let Some(command) = optional_string(args, "command") {
        split_simple_command(command)?
    } else {
        let program = required_string(args, "program")?.to_string();
        let command_args = match args.get("args") {
            Some(serde_json::Value::Array(values)) => values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(ToString::to_string)
                        .ok_or_else(|| anyhow::anyhow!("args entries must be strings"))
                })
                .collect::<anyhow::Result<Vec<_>>>()?,
            Some(serde_json::Value::String(s)) => split_simple_command(s)?.1,
            Some(_) => anyhow::bail!("args must be an array of strings or a string"),
            None => Vec::new(),
        };
        (program, command_args)
    };
    validate_program(&program)?;
    let allow_network = is_dependency_install(&program, &command_args);
    Ok(WorkspaceCommand {
        program,
        args: command_args,
        timeout,
        allow_network,
    })
}

fn split_simple_command(command: &str) -> anyhow::Result<(String, Vec<String>)> {
    if command.contains(['\n', '\r', '|', '&', ';', '>', '<', '`', '$']) {
        anyhow::bail!("run_command does not accept shell operators; use program and args");
    }
    let parts = split_words(command)?;
    let Some((program, args)) = parts.split_first() else {
        anyhow::bail!("command cannot be empty");
    };
    Ok((program.clone(), args.to_vec()))
}

fn split_words(input: &str) -> anyhow::Result<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for c in input.chars() {
        match (quote, c) {
            (Some(q), ch) if ch == q => quote = None,
            (Some(_), ch) => current.push(ch),
            (None, '\'' | '"') => quote = Some(c),
            (None, ch) if ch.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            (None, ch) => current.push(ch),
        }
    }
    if let Some(q) = quote {
        anyhow::bail!("unterminated quote {q:?} in command");
    }
    if !current.is_empty() {
        words.push(current);
    }
    Ok(words)
}

fn validate_program(program: &str) -> anyhow::Result<()> {
    let name = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program);
    if matches!(
        name,
        "rm" | "rmdir"
            | "mv"
            | "chmod"
            | "chown"
            | "sudo"
            | "su"
            | "bash"
            | "sh"
            | "zsh"
            | "fish"
            | "osascript"
            | "open"
    ) {
        anyhow::bail!("program `{name}` is not allowed by the project tool sandbox");
    }
    Ok(())
}

fn is_dependency_install(program: &str, args: &[String]) -> bool {
    let name = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program);
    matches!(name, "npm" | "pnpm" | "yarn" | "bun")
        && args
            .first()
            .is_some_and(|arg| matches!(arg.as_str(), "install" | "add"))
}

fn run_workspace_command(
    workspace_root: &Path,
    command: &WorkspaceCommand,
) -> anyhow::Result<WorkspaceCommandResult> {
    fs::create_dir_all(workspace_root)?;
    let tool_dir = workspace_root.join(".concierge_tool");
    let home = tool_dir.join("home");
    let tmp = tool_dir.join("tmp");
    let npm_cache = tool_dir.join("npm-cache");
    let cargo_home = tool_dir.join("cargo-home");
    let go_cache = tool_dir.join("go-cache");
    let go_mod_cache = tool_dir.join("go-mod-cache");
    let pycache = tool_dir.join("pycache");
    for dir in [
        &home,
        &tmp,
        &npm_cache,
        &cargo_home,
        &go_cache,
        &go_mod_cache,
        &pycache,
    ] {
        fs::create_dir_all(dir)?;
    }

    let out_path = tool_dir.join("stdout.log");
    let err_path = tool_dir.join("stderr.log");
    let stdout = File::create(&out_path)?;
    let stderr = File::create(&err_path)?;
    let (mut process, isolation) = workspace_command_process(workspace_root, command);
    let mut child = process
        .current_dir(workspace_root)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("TMPDIR", &tmp)
        .env("TEMP", &tmp)
        .env("TMP", &tmp)
        .env("CI", "true")
        .env("NPM_CONFIG_CACHE", &npm_cache)
        .env("NPM_CONFIG_AUDIT", "false")
        .env("NPM_CONFIG_FUND", "false")
        .env("CARGO_HOME", &cargo_home)
        .env("CARGO_TARGET_DIR", workspace_root.join("target"))
        .env("GOCACHE", &go_cache)
        .env("GOMODCACHE", &go_mod_cache)
        .env("PYTHONPYCACHEPREFIX", &pycache)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn();

    let started = Instant::now();
    let (exit_code, timed_out) = match child.as_mut() {
        Ok(child) => loop {
            if let Some(status) = child.try_wait()? {
                break (status.code(), false);
            }
            if started.elapsed() >= command.timeout {
                let _ = child.kill();
                let _ = child.wait();
                break (None, true);
            }
            std::thread::sleep(Duration::from_millis(25));
        },
        Err(err) => {
            fs::write(&err_path, format!("failed to spawn command: {err}"))?;
            (None, false)
        }
    };
    let stdout_tail = read_tail(&out_path, TOOL_COMMAND_TAIL_BYTES)?;
    let stderr_tail = read_tail(&err_path, TOOL_COMMAND_TAIL_BYTES)?;
    let _ = fs::remove_file(&out_path);
    let _ = fs::remove_file(&err_path);
    Ok(WorkspaceCommandResult {
        command: command.display(),
        isolation,
        exit_code,
        timed_out,
        stdout_tail,
        stderr_tail,
    })
}

fn workspace_command_process(
    workspace: &Path,
    command: &WorkspaceCommand,
) -> (Command, &'static str) {
    if command_exists("bwrap") {
        let mut process = Command::new("bwrap");
        process.args(build_bwrap_args(
            workspace,
            command.allow_network,
            &command.program,
            &command.args,
        ));
        return (process, "bubblewrap");
    }
    if command_exists("sandbox-exec") && macos_sandbox_usable() {
        let mut process = Command::new("sandbox-exec");
        process
            .arg("-p")
            .arg(build_macos_sandbox_profile(
                workspace,
                command.allow_network,
            ))
            .arg(&command.program)
            .args(&command.args);
        return (process, "sandbox-exec");
    }
    let mut process = Command::new(&command.program);
    process.args(&command.args);
    (process, "workspace")
}

fn build_bwrap_args(
    workspace: &Path,
    allow_network: bool,
    program: &str,
    args: &[String],
) -> Vec<String> {
    let workspace = workspace.to_string_lossy().to_string();
    let mut out = vec![
        "--ro-bind".to_string(),
        "/".to_string(),
        "/".to_string(),
        "--dev".to_string(),
        "/dev".to_string(),
        "--proc".to_string(),
        "/proc".to_string(),
        "--tmpfs".to_string(),
        "/tmp".to_string(),
        "--bind".to_string(),
        workspace.clone(),
        workspace.clone(),
        "--chdir".to_string(),
        workspace.clone(),
        "--die-with-parent".to_string(),
        "--unshare-pid".to_string(),
    ];
    let concierge = Path::new(&workspace).join(".concierge");
    if concierge.exists() {
        out.extend([
            "--ro-bind".to_string(),
            concierge.to_string_lossy().to_string(),
            concierge.to_string_lossy().to_string(),
        ]);
    }
    if !allow_network {
        out.push("--unshare-net".to_string());
    }
    out.push("--".to_string());
    out.push(program.to_string());
    out.extend(args.iter().cloned());
    out
}

fn build_macos_sandbox_profile(workspace: &Path, allow_network: bool) -> String {
    let mut write_filters = format!(
        " (subpath \"{}\")",
        sbpl_string(&workspace.to_string_lossy())
    );
    if let Ok(canonical) = workspace.canonicalize() {
        if canonical != workspace {
            let _ = write!(
                write_filters,
                " (subpath \"{}\")",
                sbpl_string(&canonical.to_string_lossy())
            );
        }
    }
    let concierge = workspace.join(".concierge");
    let concierge_deny = if concierge.exists() {
        format!(
            "\n(deny file-write* (subpath \"{}\"))",
            sbpl_string(&concierge.to_string_lossy())
        )
    } else {
        String::new()
    };
    let network_rule = if allow_network {
        ""
    } else {
        "\n(deny network*)"
    };
    format!(
        "(version 1)\n\
         (allow default)\n\
         (deny file-write*)\n\
         (allow file-write*{write_filters}){concierge_deny}{network_rule}"
    )
}

fn sbpl_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn command_exists(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|path| {
                let candidate = path.join(name);
                candidate.is_file()
            })
        })
        .unwrap_or(false)
}

fn macos_sandbox_usable() -> bool {
    Command::new("sandbox-exec")
        .arg("-p")
        .arg("(version 1) (allow default)")
        .arg("/usr/bin/true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn read_tail(path: &Path, limit: usize) -> anyhow::Result<String> {
    let bytes = fs::read(path)?;
    let start = bytes.len().saturating_sub(limit);
    Ok(String::from_utf8_lossy(&bytes[start..]).to_string())
}

pub(crate) fn project_file_map(root: &Path) -> anyhow::Result<String> {
    if !root.try_exists()? {
        return Ok("(project directory does not exist yet)".to_string());
    }

    let mut paths = Vec::new();
    for entry in WalkDir::new(root)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !is_ignored_project_entry(root, entry.path()))
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry.path().strip_prefix(root).unwrap_or(entry.path());
        paths.push(relative.to_string_lossy().replace('\\', "/"));
        if paths.len() >= 500 {
            break;
        }
    }

    paths.sort();
    if paths.is_empty() {
        Ok("(no files yet)".to_string())
    } else {
        Ok(paths.join("\n"))
    }
}

pub(crate) fn project_file_map_summary(project_files: &str) -> String {
    if project_files.starts_with('(') {
        return project_files.to_string();
    }
    let count = project_files
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    match count {
        0 => "(no files yet)".to_string(),
        1 => "1 file".to_string(),
        n => format!("{n} files"),
    }
}

pub(crate) fn work_files_summary(files: &[WorkFile]) -> String {
    if files.is_empty() {
        return "No files written.".to_string();
    }
    let mut summary = format!("Wrote {} file(s):", files.len());
    for file in files {
        writeln!(&mut summary, "\n- {} -> {}", file.path, file.cid)
            .expect("writing to String cannot fail");
    }
    summary
}

/// Persist one written file as memory nodes (a content blob plus a FileRef),
/// derived from the worker's recorded transcript. Returns the FileRef record.
pub(crate) fn persist_file_node(
    store: &Store<LocalBlocks, SystemClock>,
    write: &FileWrite,
    worker_result_cid: Cid,
) -> anyhow::Result<WorkFile> {
    let blob_cid = store.put_node(
        Node::Blob(Blob {
            bytes: write.content.as_bytes().to_vec(),
            media_type: Some("text/plain".to_string()),
        }),
        Source::Derived {
            from: vec![worker_result_cid],
        },
    )?;
    let file_cid = store.put_node_with_edges(
        Node::FileRef(FileRef {
            path: write.path.clone(),
            size: Some(write.content.len() as u64),
            media_type: Some("text/plain".to_string()),
            mtime: None,
            content: blob_cid,
        }),
        Source::Derived {
            from: vec![worker_result_cid],
        },
        vec![Edge {
            rel: EdgeRel::DerivedFrom,
            to: worker_result_cid,
        }],
    )?;
    Ok(WorkFile {
        path: write.path.clone(),
        cid: file_cid,
    })
}

fn is_ignored_project_entry(root: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.components().any(|component| match component {
        Component::Normal(name) => matches!(
            name.to_str(),
            Some(
                ".concierge" | ".concierge_tool" | ".git" | "target" | "node_modules" | ".DS_Store"
            )
        ),
        _ => false,
    })
}

pub(crate) fn workspace_path(root: &Path, path: &str) -> anyhow::Result<PathBuf> {
    let requested = Path::new(path.trim());
    if requested.as_os_str().is_empty() {
        anyhow::bail!("worker file path is empty");
    }
    if requested.is_absolute() {
        anyhow::bail!("worker file path must be relative: {path}");
    }

    let mut clean = PathBuf::new();
    for component in requested.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir => anyhow::bail!("worker file path cannot contain '..': {path}"),
            Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("worker file path must stay inside the project: {path}")
            }
        }
    }
    if clean.as_os_str().is_empty() {
        anyhow::bail!("worker file path is empty");
    }
    if clean.components().next().is_some_and(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some(".concierge" | ".concierge_tool")
        )
    }) {
        anyhow::bail!("worker cannot write into Concierge state directories");
    }
    Ok(root.join(clean))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::NameIndex;
    use tempfile::TempDir;

    #[test]
    fn manifest_prompt_carries_goal_and_plan() {
        let prompt = manifest_prompt_text("build an app", "the plan");
        assert!(prompt.contains("Goal:\nbuild an app"));
        assert!(prompt.contains("Plan:\nthe plan"));
        assert!(prompt.contains("newline-delimited list"));
    }

    #[test]
    fn parse_manifest_is_lenient_and_drops_prose() {
        let reply = "Here are the files:\n\
                     - src/app.ts\n\
                     1. src/services/Pricing.ts\n\
                     `package.json`\n\
                     This file holds the styles.\n\
                     src/app.ts\n\
                     # heading\n\
                     README.md";
        let manifest = parse_manifest(reply);
        assert_eq!(
            manifest,
            vec![
                "src/app.ts".to_string(),
                "src/services/Pricing.ts".to_string(),
                "package.json".to_string(),
                "README.md".to_string(),
            ],
            "bullets/numbers/backticks stripped, prose + headings + dupes dropped"
        );
    }

    #[test]
    fn alignment_prompt_carries_plan_audit_symbols_and_current_contents() {
        let written = [
            FileWrite {
                path: "src/a.ts".to_string(),
                content: "import { Gone } from './b';".to_string(),
            },
            FileWrite {
                path: "src/b.ts".to_string(),
                content: "export const Real = 1;".to_string(),
            },
        ];
        let prompt = alignment_prompt_text(AlignmentPrompt {
            goal: "build an app",
            project_plan: "the plan",
            manifest: &["src/a.ts".to_string(), "src/b.ts".to_string()],
            written: &written,
            target: "src/a.ts",
            current: "import { Gone } from './b';",
            findings: "- imports `Gone` from './b', which does not define it",
            audit: "Broken imports:\n- src/a.ts: imports `Gone` from './b'",
        });
        assert!(prompt.contains("Original goal:\nbuild an app"));
        assert!(prompt.contains("Original project plan:\nthe plan"));
        assert!(prompt.contains("Original file manifest:\nsrc/a.ts\nsrc/b.ts"));
        assert!(prompt.contains("File for this alignment step: src/a.ts"));
        assert!(
            prompt.contains("src/b.ts — defines: Real"),
            "current file symbols shown"
        );
        assert!(prompt.contains("Findings for this file:\n- imports `Gone`"));
        assert!(prompt.contains("Full audit findings:\nBroken imports"));
        assert!(prompt.contains("Current contents:\nimport { Gone } from './b';"));
    }

    #[test]
    fn public_symbols_covers_the_common_typescript_forms() {
        let content = "export class Property {}\n\
                       export interface Contract {}\n\
                       export enum Area { North }\n\
                       export const RATE = 1;\n\
                       export function calc() {}\n\
                       export type Mode = 'full';\n\
                       export default class App {}\n\
                       export { helper, raw as renamed };\n\
                       const private_thing = 2;";
        assert_eq!(
            public_symbols(content),
            vec![
                "Property", "Contract", "Area", "RATE", "calc", "Mode", "App", "helper", "renamed",
            ],
            "every exported name is captured; non-exported names are ignored"
        );
    }

    #[test]
    fn public_symbols_handles_other_languages() {
        let content = "pub fn rust_fn() {}\n\
                       pub struct RustStruct {}\n\
                       pub(crate) enum RustEnum {}\n\
                       def python_func():\n\
                       class PythonClass:\n\
                       func GoFunc() {}\n\
                       public class JavaClass {}\n\
                       fn private_rust() {}\n\
                       let local = 1;";
        assert_eq!(
            public_symbols(content),
            vec![
                "rust_fn",
                "RustStruct",
                "RustEnum",
                "python_func",
                "PythonClass",
                "GoFunc",
                "JavaClass",
            ],
            "public decls across languages captured; private/local ones ignored"
        );
    }

    #[test]
    fn public_symbols_empty_when_nothing_is_public() {
        assert!(public_symbols("const x = 1;\nlet y = 2;").is_empty());
    }

    #[test]
    fn public_symbols_never_invents_a_default_name() {
        // A named default export resolves to its real name...
        assert_eq!(public_symbols("export default class App {}"), vec!["App"]);
        // ...but an unnameable default export contributes no importable name.
        assert!(public_symbols("export default createStore();").is_empty());
        assert!(public_symbols("export default { a: 1 };").is_empty());
    }

    #[test]
    fn extract_file_body_takes_the_first_fenced_block() {
        let reply = "Sure, here is the file:\n\
                     ```ts\nexport const x = 1;\n```\n\
                     Let me know if you need changes.";
        assert_eq!(extract_file_body(reply), "export const x = 1;\n");
    }

    #[test]
    fn extract_file_body_falls_back_to_the_whole_reply_without_a_fence() {
        assert_eq!(
            extract_file_body("  raw file contents  "),
            "raw file contents"
        );
    }

    #[test]
    fn write_workspace_file_writes_and_rejects_unsafe_paths() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let write = write_workspace_file(&workspace, "src/app.ts", "hi\n").unwrap();
        assert_eq!(write.path, "src/app.ts");
        assert_eq!(
            std::fs::read_to_string(workspace.join("src/app.ts")).unwrap(),
            "hi\n"
        );

        let err = write_workspace_file(&workspace, "../escape.txt", "nope")
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot contain '..'"), "got: {err}");
        assert!(!dir.path().join("escape.txt").exists());
    }

    #[test]
    fn project_tool_write_and_read_are_project_bound() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let write = execute_project_tool(
            &workspace,
            "write_file",
            &serde_json::json!({"path": "src/app.ts", "content": "export const x = 1;\n"}),
        );
        assert!(write.ok, "{}", write.output);
        assert_eq!(write.write.as_ref().unwrap().path, "src/app.ts");

        let read = execute_project_tool(
            &workspace,
            "read_file",
            &serde_json::json!({"path": "src/app.ts"}),
        );
        assert!(read.ok, "{}", read.output);
        assert!(read.output.contains("export const x = 1"));

        let escape = execute_project_tool(
            &workspace,
            "write_file",
            &serde_json::json!({"path": "../escape.ts", "content": "no"}),
        );
        assert!(!escape.ok);
        assert!(escape.output.contains("cannot contain '..'"));
        assert!(!dir.path().join("escape.ts").exists());
    }

    #[test]
    fn project_tool_edit_replaces_exact_text() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        write_workspace_file(&workspace, "src/app.ts", "const x = 1;\n").unwrap();
        let edit = execute_project_tool(
            &workspace,
            "edit_file",
            &serde_json::json!({"path": "src/app.ts", "old": "x = 1", "new": "x = 2"}),
        );
        assert!(edit.ok, "{}", edit.output);
        assert_eq!(
            std::fs::read_to_string(workspace.join("src/app.ts")).unwrap(),
            "const x = 2;\n"
        );
    }

    #[test]
    fn parse_tool_request_ignores_regular_json_files() {
        let package_like = r#"{"name":"package.json","arguments":[]}"#;
        assert!(parse_tool_request(package_like).is_none());

        let request = r#"{"name":"write_file","arguments":{"path":"package.json","content":"{}"}}"#;
        assert_eq!(
            parse_tool_request(request).map(|request| request.name),
            Some("write_file".to_string())
        );
    }

    #[test]
    fn run_command_rejects_shell_and_destructive_programs() {
        let dir = TempDir::new().unwrap();
        let shell = execute_project_tool(
            dir.path(),
            "run_command",
            &serde_json::json!({"command": "npm run build && rm -rf ."}),
        );
        assert!(!shell.ok);
        assert!(shell.output.contains("shell operators"));

        let destructive = execute_project_tool(
            dir.path(),
            "run_command",
            &serde_json::json!({"program": "rm", "args": ["-rf", "."]}),
        );
        assert!(!destructive.ok);
        assert!(destructive.output.contains("not allowed"));
    }

    #[test]
    fn persist_file_node_records_a_fileref_derived_from_the_transcript() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(
            LocalBlocks::new(dir.path().join("blocks")),
            NameIndex::load(dir.path().join("names.json")).unwrap(),
        );
        let worker_result_cid = store
            .put_node(
                Node::Blob(Blob {
                    bytes: b"transcript".to_vec(),
                    media_type: Some("text/plain".to_string()),
                }),
                Source::System,
            )
            .unwrap();
        let file = persist_file_node(
            &store,
            &FileWrite {
                path: "src/app.ts".to_string(),
                content: "code".to_string(),
            },
            worker_result_cid,
        )
        .unwrap();
        assert_eq!(file.path, "src/app.ts");
        let record = store.get_node(&file.cid).unwrap();
        assert!(
            record
                .edges
                .iter()
                .any(|edge| edge.rel == EdgeRel::DerivedFrom && edge.to == worker_result_cid)
        );
    }

    #[test]
    fn workspace_paths_must_stay_inside_project() {
        let root = Path::new("/project");
        assert!(workspace_path(root, "/tmp/x").is_err());
        assert!(workspace_path(root, "../x").is_err());
        assert!(workspace_path(root, ".concierge/config.toml").is_err());
        assert!(workspace_path(root, ".concierge_tool/stdout.log").is_err());
        assert_eq!(
            workspace_path(root, "src/app.ts").unwrap(),
            root.join("src/app.ts")
        );
    }

    #[test]
    fn project_file_map_skips_local_state_and_dependency_dirs() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join(".concierge")).unwrap();
        std::fs::create_dir_all(dir.path().join(".concierge_tool")).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        std::fs::write(dir.path().join("src/app.ts"), "app").unwrap();
        std::fs::write(dir.path().join(".concierge/config.toml"), "state").unwrap();
        std::fs::write(dir.path().join(".concierge_tool/stdout.log"), "tool").unwrap();
        std::fs::write(dir.path().join("node_modules/pkg/index.js"), "dep").unwrap();

        let map = project_file_map(dir.path()).unwrap();
        assert_eq!(map, "src/app.ts");
    }
}

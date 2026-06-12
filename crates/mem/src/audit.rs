//! Deterministic, whole-project auditor — no LLM.
//!
//! Runs over the files the worker just wrote (in memory, path + content) and
//! reports four classes of defect:
//!   1. **Empty files** — a written file with no real content (the 0-byte bug).
//!   2. **Phantom local imports** — a file imports a relative module or a named
//!      or default symbol that does not exist among the written files (the
//!      cross-file breakage that recurs every run).
//!   3. **Syntax findings** — folded in per file from `diagnostics`.
//!   4. **Scaffold findings** — common generated-project families with missing
//!      or internally inconsistent entrypoint/config/dependency files, checked
//!      against both existing and written paths.
//!
//! Pure: reads the in-memory file set, mutates nothing. The report drives the
//! optional cleanup model; the auditor itself never regenerates anything.

use crate::design::{DesignFinding, design_findings};
use crate::diagnostics::file_diagnostics;
use crate::work::{FileWrite, public_symbols};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

#[derive(Debug, Clone)]
pub(crate) struct Phantom {
    pub file: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub(crate) struct SyntaxFinding {
    pub file: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ScaffoldFinding {
    pub file: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AuditReport {
    pub empty_files: Vec<String>,
    pub phantom_imports: Vec<Phantom>,
    pub syntax: Vec<SyntaxFinding>,
    pub design: Vec<DesignFinding>,
    pub scaffold: Vec<ScaffoldFinding>,
}

impl AuditReport {
    pub fn is_clean(&self) -> bool {
        self.empty_files.is_empty()
            && self.phantom_imports.is_empty()
            && self.syntax.is_empty()
            && self.design.is_empty()
            && self.scaffold.is_empty()
    }

    /// Total findings across all categories — the metric the repair loop uses to
    /// detect progress (a lap that doesn't shrink this is making none).
    pub fn finding_count(&self) -> usize {
        self.empty_files.len()
            + self.phantom_imports.len()
            + self.syntax.len()
            + self.design.len()
            + self.scaffold.len()
    }

    /// Distinct files implicated by any finding — the repair scope.
    pub fn files(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        for f in &self.empty_files {
            seen.insert(f.clone());
        }
        for p in &self.phantom_imports {
            seen.insert(p.file.clone());
        }
        for s in &self.syntax {
            seen.insert(s.file.clone());
        }
        for d in &self.design {
            seen.insert(d.file.clone());
        }
        for s in &self.scaffold {
            seen.insert(s.file.clone());
        }
        seen.into_iter().collect()
    }

    /// Files to hand back to the workhorse during alignment. This starts with
    /// directly implicated files, then adds concrete missing modules implied by
    /// local import findings.
    pub fn alignment_files(&self, written: &[FileWrite]) -> Vec<String> {
        let written_paths: BTreeSet<&str> = written.iter().map(|file| file.path.as_str()).collect();
        let mut seen: BTreeSet<String> = self.files().into_iter().collect();
        for finding in &self.phantom_imports {
            let Some(spec) = missing_import_spec(&finding.detail) else {
                continue;
            };
            for candidate in implied_module_paths(&finding.file, spec) {
                if !written_paths.contains(candidate.as_str()) {
                    seen.insert(candidate);
                    break;
                }
            }
        }
        seen.into_iter().collect()
    }

    /// The findings that implicate one file, as plain lines — fed to the
    /// cleanup model so it knows exactly what to fix in that file.
    pub fn for_file(&self, path: &str) -> String {
        let mut lines = Vec::new();
        if self.empty_files.iter().any(|f| f == path) {
            lines.push("- file is empty; write its real, complete contents".to_string());
        }
        for p in self.phantom_imports.iter().filter(|p| p.file == path) {
            lines.push(format!("- {}", p.detail));
        }
        for s in self.syntax.iter().filter(|s| s.file == path) {
            lines.push(format!("- {}", s.message));
        }
        for d in self.design.iter().filter(|d| d.file == path) {
            lines.push(format!("- {} ({}): {}", d.name, d.snippet, d.guidance));
        }
        for s in self.scaffold.iter().filter(|s| s.file == path) {
            lines.push(format!("- {}", s.detail));
        }
        lines.join("\n")
    }

    /// Compact diagnostic block; empty when clean.
    pub fn render(&self) -> String {
        if self.is_clean() {
            return String::new();
        }
        let mut out = String::new();
        if !self.empty_files.is_empty() {
            out.push_str("Empty files (must contain real code):\n");
            for f in &self.empty_files {
                let _ = writeln!(out, "- {f}");
            }
        }
        if !self.phantom_imports.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("Broken imports:\n");
            for p in &self.phantom_imports {
                let _ = writeln!(out, "- {}: {}", p.file, p.detail);
            }
        }
        if !self.syntax.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("Syntax findings:\n");
            for s in &self.syntax {
                let _ = writeln!(out, "- {}: {}", s.file, s.message);
            }
        }
        if !self.design.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("Design anti-patterns:\n");
            for d in &self.design {
                let _ = writeln!(
                    out,
                    "- {}:{} [{}] {} ({})",
                    d.file, d.line, d.rule, d.name, d.snippet
                );
            }
        }
        if !self.scaffold.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("Scaffold findings:\n");
            for s in &self.scaffold {
                let _ = writeln!(out, "- {}: {}", s.file, s.detail);
            }
        }
        out
    }
}

fn missing_import_spec(detail: &str) -> Option<&str> {
    detail
        .strip_prefix("imports '")
        .and_then(|rest| rest.split_once("', which no written file provides"))
        .map(|(spec, _)| spec)
}

fn implied_module_paths(importer: &str, spec: &str) -> Vec<String> {
    if !(spec.starts_with("./") || spec.starts_with("../")) {
        return Vec::new();
    }
    let Some(base) = normalize_module_base(importer, spec) else {
        return Vec::new();
    };
    let last = base.rsplit('/').next().unwrap_or(base.as_str());
    if last.contains('.') {
        return vec![base];
    }
    [".ts", ".tsx", ".js", ".jsx", ".json"]
        .into_iter()
        .map(|ext| format!("{base}{ext}"))
        .chain(
            ["index.ts", "index.tsx", "index.js", "index.jsx"]
                .into_iter()
                .map(|name| format!("{base}/{name}")),
        )
        .collect()
}

fn normalize_module_base(importer: &str, spec: &str) -> Option<String> {
    let mut parts: Vec<&str> = importer.split('/').collect();
    parts.pop();
    for part in spec.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Audit the just-written file set.
#[cfg(test)]
pub(crate) fn review(written: &[FileWrite]) -> AuditReport {
    review_with_project_files(written, "")
}

/// Audit the just-written file set with the project file map captured before
/// this work run. Existing paths prevent scaffold checks from demanding files
/// that are already present in the workspace.
pub(crate) fn review_with_project_files(written: &[FileWrite], project_files: &str) -> AuditReport {
    let mut report = AuditReport::default();

    // Exports per path, and the set of written paths, for import resolution.
    let mut exports: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut default_exports: BTreeMap<&str, bool> = BTreeMap::new();
    let mut paths: BTreeSet<&str> = BTreeSet::new();
    for file in written {
        paths.insert(file.path.as_str());
        exports.insert(file.path.as_str(), public_symbols(&file.content));
        default_exports.insert(file.path.as_str(), has_default_export(&file.content));
    }

    for file in written {
        if file.content.trim().is_empty() {
            report.empty_files.push(file.path.clone());
            continue; // nothing else to check in an empty file
        }

        for import in parse_local_imports(&file.content) {
            match resolve(&file.path, &import.spec, &paths) {
                Resolution::Missing => report.phantom_imports.push(Phantom {
                    file: file.path.clone(),
                    detail: format!("imports '{}', which no written file provides", import.spec),
                }),
                Resolution::Found(target) => {
                    if let Some(name) = &import.default {
                        if !default_exports
                            .get(target.as_str())
                            .copied()
                            .unwrap_or(false)
                        {
                            report.phantom_imports.push(Phantom {
                                file: file.path.clone(),
                                detail: format!(
                                    "imports `{name}` as the default from '{}', which has no default export",
                                    import.spec
                                ),
                            });
                        }
                    }
                    if let Some(syms) = exports.get(target.as_str()) {
                        for name in &import.names {
                            if !syms.iter().any(|s| s == name) {
                                report.phantom_imports.push(Phantom {
                                    file: file.path.clone(),
                                    detail: format!(
                                        "imports `{name}` from '{}', which does not define it",
                                        import.spec
                                    ),
                                });
                            }
                        }
                    }
                }
            }
        }

        for message in file_diagnostics(&file.path, &file.content) {
            report.syntax.push(SyntaxFinding {
                file: file.path.clone(),
                message,
            });
        }

        report
            .design
            .extend(design_findings(&file.path, &file.content));
    }

    report
        .scaffold
        .extend(scaffold_findings(written, project_files));

    report
}

fn scaffold_findings(written: &[FileWrite], project_files: &str) -> Vec<ScaffoldFinding> {
    let mut paths = project_paths(project_files);
    for file in written {
        paths.insert(file.path.clone());
    }

    let mut findings = Vec::new();
    if looks_like_react_web_project(written) {
        require_any(
            &mut findings,
            &paths,
            &["package.json"],
            "React/web project scaffold is missing package metadata and scripts",
        );
        require_any(
            &mut findings,
            &paths,
            &["index.html", "public/index.html"],
            "React/web project scaffold is missing an HTML entrypoint",
        );
        if written.iter().any(|file| is_ts_path(&file.path)) {
            require_any(
                &mut findings,
                &paths,
                &["tsconfig.json"],
                "TypeScript project scaffold is missing tsconfig.json",
            );
        }
        require_any(
            &mut findings,
            &paths,
            &[
                "src/main.tsx",
                "src/main.jsx",
                "src/main.ts",
                "src/main.js",
                "src/index.tsx",
                "src/index.jsx",
                "main.tsx",
                "main.jsx",
                "index.tsx",
                "index.jsx",
            ],
            "React/web project scaffold is missing an application entrypoint that mounts the app",
        );
        react_entrypoint_findings(&mut findings, written);
    }

    if looks_like_rust_project(written) {
        require_any(
            &mut findings,
            &paths,
            &["Cargo.toml"],
            "Rust project scaffold is missing Cargo.toml",
        );
    }

    if looks_like_go_project(written) {
        require_any(
            &mut findings,
            &paths,
            &["go.mod"],
            "Go project scaffold is missing go.mod",
        );
    }

    if looks_like_python_project(written) {
        require_any(
            &mut findings,
            &paths,
            &[
                "pyproject.toml",
                "requirements.txt",
                "setup.py",
                "setup.cfg",
                "Pipfile",
            ],
            "Python project scaffold is missing project or dependency metadata",
        );
    }

    findings.extend(package_dependency_findings(written));
    findings.extend(tsconfig_include_findings(written));
    findings.extend(placeholder_findings(written));

    findings
}

fn project_paths(project_files: &str) -> BTreeSet<String> {
    project_files
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('('))
        .map(ToString::to_string)
        .collect()
}

fn require_any(
    findings: &mut Vec<ScaffoldFinding>,
    paths: &BTreeSet<String>,
    candidates: &[&str],
    detail: &str,
) {
    if candidates.iter().any(|path| paths.contains(*path)) {
        return;
    }
    findings.push(ScaffoldFinding {
        file: candidates[0].to_string(),
        detail: detail.to_string(),
    });
}

fn react_entrypoint_findings(findings: &mut Vec<ScaffoldFinding>, written: &[FileWrite]) {
    let Some(index) = written
        .iter()
        .find(|file| file.path == "index.html" || file.path == "public/index.html")
    else {
        return;
    };
    let Some(entrypoint) = react_entrypoint_path(written) else {
        return;
    };
    let expected = format!("src=\"/{entrypoint}\"");
    if index.content.contains("/path/to/your") || !index.content.contains("type=\"module\"") {
        findings.push(ScaffoldFinding {
            file: index.path.clone(),
            detail: format!(
                "HTML entrypoint must load the app with a Vite-style module script for `/{entrypoint}`"
            ),
        });
        return;
    }
    if !index.content.contains(&expected) {
        findings.push(ScaffoldFinding {
            file: index.path.clone(),
            detail: format!(
                "HTML entrypoint does not load the generated app entry `/{entrypoint}`"
            ),
        });
    }
}

fn react_entrypoint_path(written: &[FileWrite]) -> Option<String> {
    [
        "src/main.tsx",
        "src/main.jsx",
        "src/main.ts",
        "src/main.js",
        "src/index.tsx",
        "src/index.jsx",
        "main.tsx",
        "main.jsx",
        "index.tsx",
        "index.jsx",
    ]
    .iter()
    .find(|candidate| written.iter().any(|file| file.path == **candidate))
    .map(|path| (*path).to_string())
}

fn package_dependency_findings(written: &[FileWrite]) -> Vec<ScaffoldFinding> {
    let Some(package_json) = written.iter().find(|file| file.path == "package.json") else {
        return Vec::new();
    };
    let Some(declared) = package_dependencies(&package_json.content) else {
        return Vec::new();
    };

    let mut findings = Vec::new();
    let mut seen = BTreeSet::new();
    for import in external_imports(written) {
        if declared.contains(&import.package) || !seen.insert(import.package.clone()) {
            continue;
        }
        findings.push(ScaffoldFinding {
            file: "package.json".to_string(),
            detail: format!(
                "package.json is missing dependency `{}` imported by {} from `{}`",
                import.package, import.file, import.spec
            ),
        });
    }
    findings
}

fn package_dependencies(content: &str) -> Option<BTreeSet<String>> {
    let root: serde_json::Value = serde_json::from_str(content).ok()?;
    let mut deps = BTreeSet::new();
    for key in [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ] {
        if let Some(obj) = root.get(key).and_then(|value| value.as_object()) {
            deps.extend(obj.keys().cloned());
        }
    }
    Some(deps)
}

struct ExternalImport {
    file: String,
    spec: String,
    package: String,
}

fn external_imports(written: &[FileWrite]) -> Vec<ExternalImport> {
    let mut imports = Vec::new();
    for file in written.iter().filter(|file| is_source_path(&file.path)) {
        for spec in parse_import_specs(&file.content) {
            if spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/') {
                continue;
            }
            let Some(package) = package_name(&spec) else {
                continue;
            };
            imports.push(ExternalImport {
                file: file.path.clone(),
                spec,
                package,
            });
        }
    }
    imports
}

fn package_name(spec: &str) -> Option<String> {
    if spec.is_empty()
        || spec.starts_with("node:")
        || spec.starts_with("data:")
        || spec.starts_with("http:")
        || spec.starts_with("https:")
    {
        return None;
    }
    if spec.starts_with('@') {
        let mut parts = spec.split('/');
        let scope = parts.next()?;
        let name = parts.next()?;
        return Some(format!("{scope}/{name}"));
    }
    spec.split('/').next().map(ToString::to_string)
}

fn tsconfig_include_findings(written: &[FileWrite]) -> Vec<ScaffoldFinding> {
    let Some(tsconfig) = written.iter().find(|file| file.path == "tsconfig.json") else {
        return Vec::new();
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&tsconfig.content) else {
        return Vec::new();
    };
    let Some(include) = root.get("include").and_then(|value| value.as_array()) else {
        return Vec::new();
    };
    let patterns: Vec<String> = include
        .iter()
        .filter_map(|value| value.as_str().map(ToString::to_string))
        .collect();
    if patterns.is_empty() {
        return Vec::new();
    }

    let uncovered: Vec<String> = written
        .iter()
        .filter(|file| is_ts_path(&file.path) && file.path != "tsconfig.json")
        .filter(|file| {
            !patterns
                .iter()
                .any(|pattern| include_pattern_covers(pattern, &file.path))
        })
        .map(|file| file.path.clone())
        .collect();
    if uncovered.is_empty() {
        return Vec::new();
    }

    vec![ScaffoldFinding {
        file: "tsconfig.json".to_string(),
        detail: format!(
            "tsconfig.json include patterns do not cover generated TypeScript files: {}",
            uncovered.join(", ")
        ),
    }]
}

fn include_pattern_covers(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_start_matches("./");
    if pattern == "**/*" || pattern == "." || pattern == path {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/**/*") {
        return path.starts_with(&format!("{prefix}/"));
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        let Some(rest) = path.strip_prefix(&format!("{prefix}/")) else {
            return false;
        };
        return !rest.contains('/');
    }
    if !pattern.contains('*') && path.starts_with(&format!("{pattern}/")) {
        return true;
    }
    if let Some(ext) = pattern.strip_prefix("*.") {
        return !path.contains('/') && path.ends_with(&format!(".{ext}"));
    }
    false
}

fn placeholder_findings(written: &[FileWrite]) -> Vec<ScaffoldFinding> {
    let mut findings = Vec::new();
    for file in written {
        for line in file.content.lines() {
            let trimmed = line.trim();
            let detail = if trimmed.contains("/path/to/your")
                || trimmed.contains("Adjust path to your")
                || trimmed.contains("Replace App with")
            {
                Some(
                    "file contains placeholder scaffold text that must be replaced with working application code",
                )
            } else if trimmed.starts_with("//")
                && trimmed.contains("Implement ")
                && trimmed.contains("logic here")
            {
                Some("file contains placeholder implementation comments instead of working logic")
            } else {
                None
            };
            if let Some(detail) = detail {
                findings.push(ScaffoldFinding {
                    file: file.path.clone(),
                    detail: detail.to_string(),
                });
                break;
            }
        }
    }
    findings
}

fn looks_like_react_web_project(written: &[FileWrite]) -> bool {
    written.iter().any(|file| {
        let path = file.path.as_str();
        is_react_path(path)
            || file.content.contains("from 'react'")
            || file.content.contains("from \"react\"")
            || file.content.contains("React.")
            || file.content.contains("React.FC")
    })
}

fn looks_like_rust_project(written: &[FileWrite]) -> bool {
    written
        .iter()
        .any(|file| file.path.starts_with("src/") && file.path.ends_with(".rs"))
}

fn looks_like_go_project(written: &[FileWrite]) -> bool {
    written.iter().any(|file| file.path.ends_with(".go"))
}

fn looks_like_python_project(written: &[FileWrite]) -> bool {
    written.iter().any(|file| {
        file.path.ends_with(".py")
            && (file.path.starts_with("src/")
                || file.path.contains('/')
                || file.content.contains("from fastapi")
                || file.content.contains("import fastapi")
                || file.content.contains("from flask")
                || file.content.contains("import flask")
                || file.content.contains("from django")
                || file.content.contains("import django"))
    })
}

fn is_react_path(path: &str) -> bool {
    path.ends_with(".tsx") || path.ends_with(".jsx")
}

fn is_ts_path(path: &str) -> bool {
    path.ends_with(".ts") || path.ends_with(".tsx")
}

fn is_source_path(path: &str) -> bool {
    matches!(
        path.rsplit_once('.').map(|(_, ext)| ext),
        Some("ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs")
    )
}

struct LocalImport {
    spec: String,
    names: Vec<String>,
    default: Option<String>,
}

enum Resolution {
    Found(String),
    Missing,
}

/// Parse the relative-path imports on each line. Conservative and line-based:
/// catches the common single-line forms; misses (not mis-flags) exotic ones.
fn parse_local_imports(content: &str) -> Vec<LocalImport> {
    let mut out = Vec::new();
    for line in content.lines() {
        if let Some(spec) = local_import_spec(line) {
            out.push(LocalImport {
                spec,
                names: brace_names(line.trim()),
                default: default_import_name(line.trim()),
            });
        }
    }
    out
}

fn parse_import_specs(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| {
            let l = line.trim();
            let is_import = l.starts_with("import");
            let is_reexport = l.starts_with("export") && l.contains(" from ");
            (is_import || is_reexport).then(|| import_spec(l)).flatten()
        })
        .collect()
}

/// The relative module specifier imported on this line, if it is an import (or
/// re-export) of a local path (`./`, `../`, `/`). `None` for non-import lines
/// and external packages.
fn local_import_spec(line: &str) -> Option<String> {
    let l = line.trim();
    let is_import = l.starts_with("import");
    let is_reexport = l.starts_with("export") && l.contains(" from ");
    if !is_import && !is_reexport {
        return None;
    }
    let spec = import_spec(l)?;
    (spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/')).then_some(spec)
}

/// The quoted module specifier: the string after ` from `, or — for a
/// side-effect `import './x'` — the first quoted string on the line.
fn import_spec(line: &str) -> Option<String> {
    if let Some(idx) = line.find(" from ") {
        quoted(&line[idx + 6..])
    } else {
        quoted(line)
    }
}

/// First single- or double-quoted token in `s`.
fn quoted(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'\'' || b == b'"')?;
    let quote = bytes[start];
    let rest = &s[start + 1..];
    let end = rest.find(quote as char)?;
    Some(rest[..end].to_string())
}

/// Imported names from a complete `{ A, B as C }` on the line (the *imported*
/// name, i.e. before `as`). Empty if there is no complete brace group (default
/// or namespace imports, or a multi-line group — checked only for existence).
fn brace_names(line: &str) -> Vec<String> {
    let Some(open) = line.find('{') else {
        return Vec::new();
    };
    let Some(close_rel) = line[open..].find('}') else {
        return Vec::new();
    };
    line[open + 1..open + close_rel]
        .split(',')
        .filter_map(|part| {
            let token = part.trim();
            let name = token.split_whitespace().next().unwrap_or("");
            let name =
                name.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '$');
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

fn default_import_name(line: &str) -> Option<String> {
    let is_import = line.starts_with("import ");
    if !is_import || !line.contains(" from ") {
        return None;
    }
    let before_from = line.strip_prefix("import")?.split(" from ").next()?.trim();
    let before_from = before_from
        .strip_prefix("type ")
        .unwrap_or(before_from)
        .trim();
    if before_from.is_empty() || before_from.starts_with('{') || before_from.starts_with('*') {
        return None;
    }
    let name = before_from.split(',').next()?.trim();
    let name = name.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '$');
    (!name.is_empty()).then(|| name.to_string())
}

fn has_default_export(content: &str) -> bool {
    content.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("export default ")
            || trimmed.starts_with("export { default")
            || trimmed.contains(", default as ")
    })
}

/// Resolve a relative import against the written file set.
fn resolve(importer: &str, spec: &str, paths: &BTreeSet<&str>) -> Resolution {
    let base_dir = importer.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
    let joined = if let Some(abs) = spec.strip_prefix('/') {
        abs.to_string()
    } else if base_dir.is_empty() {
        spec.to_string()
    } else {
        format!("{base_dir}/{spec}")
    };
    let Some(normalized) = normalize(&joined) else {
        return Resolution::Missing;
    };

    // Direct hit (the spec already names a file), then common resolutions.
    let mut candidates = vec![normalized.clone()];
    for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs"] {
        candidates.push(format!("{normalized}.{ext}"));
        candidates.push(format!("{normalized}/index.{ext}"));
    }
    for candidate in candidates {
        if paths.contains(candidate.as_str()) {
            return Resolution::Found(candidate);
        }
    }
    Resolution::Missing
}

/// Collapse `.` and `..` segments in a slash path (no disk access). Returns
/// `None` when a path climbs above the project root.
fn normalize(path: &str) -> Option<String> {
    let mut stack: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                stack.pop()?;
            }
            other => stack.push(other),
        }
    }
    Some(stack.join("/"))
}

/// One deterministic import-path correction.
#[derive(Debug, Clone)]
pub(crate) struct PathFix {
    pub file: String,
    pub from: String,
    pub to: String,
}

/// Deterministically correct broken relative imports. Two cases, no model:
///
/// - **Path-missing:** `'../../types/models'` from `src/components/Foo.tsx`
///   doesn't resolve but exactly one written file shares its basename →
///   rewrite the path (`'../types/models'`).
/// - **Symbol-elsewhere:** `import { Contract, Area } from '../types/models'`
///   where `models.ts` exists but doesn't export `Area`, and exactly one other
///   written file does (`enums.ts`) → **split** the import, keeping the
///   resolving names and moving the rest to the file that actually defines them.
///
/// Conservative: a rewrite happens only when the target is unambiguous and the
/// corrected specifier resolves. Genuinely-undefined or ambiguous symbols are
/// left for the cleanup model.
pub(crate) fn fix_import_paths(written: &[FileWrite]) -> (Vec<FileWrite>, Vec<PathFix>) {
    let paths: BTreeSet<&str> = written.iter().map(|file| file.path.as_str()).collect();
    let mut by_stem: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    let mut exports: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut symbol_to_files: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for file in written {
        let syms = public_symbols(&file.content);
        for sym in &syms {
            symbol_to_files
                .entry(sym.clone())
                .or_default()
                .push(file.path.as_str());
        }
        exports.insert(file.path.as_str(), syms);
        if let Some(stem) = module_stem(&file.path) {
            by_stem.entry(stem).or_default().push(file.path.as_str());
        }
    }

    let mut fixes = Vec::new();
    let mut result = Vec::with_capacity(written.len());
    for file in written {
        let mut changed = false;
        let mut out: Vec<String> = Vec::new();
        for line in file.content.lines() {
            match rewrite_import_line(
                line,
                &file.path,
                &paths,
                &by_stem,
                &exports,
                &symbol_to_files,
            ) {
                Some((replacement, mut pfixes)) => {
                    out.push(replacement);
                    fixes.append(&mut pfixes);
                    changed = true;
                }
                None => out.push(line.to_string()),
            }
        }
        let content = if changed {
            let mut s = out.join("\n");
            if file.content.ends_with('\n') {
                s.push('\n');
            }
            s
        } else {
            file.content.clone()
        };
        result.push(FileWrite {
            path: file.path.clone(),
            content,
        });
    }
    (result, fixes)
}

/// Rewrite one import line if it's deterministically fixable; otherwise `None`.
/// The replacement may be multiple lines (when an import is split).
fn rewrite_import_line(
    line: &str,
    importer: &str,
    paths: &BTreeSet<&str>,
    by_stem: &BTreeMap<String, Vec<&str>>,
    exports: &BTreeMap<&str, Vec<String>>,
    symbol_to_files: &BTreeMap<String, Vec<&str>>,
) -> Option<(String, Vec<PathFix>)> {
    let spec = local_import_spec(line)?;
    match resolve(importer, &spec, paths) {
        // ── Path-missing: rewrite to the unique basename match. ──
        Resolution::Missing => {
            let stem = spec_stem(&spec)?;
            let candidates = by_stem.get(&stem)?;
            if candidates.len() != 1 || candidates[0] == importer {
                return None;
            }
            let corrected = relative_specifier(importer, candidates[0]);
            if corrected == spec
                || !matches!(resolve(importer, &corrected, paths), Resolution::Found(_))
            {
                return None;
            }
            let new_line = line
                .replace(&format!("'{spec}'"), &format!("'{corrected}'"))
                .replace(&format!("\"{spec}\""), &format!("\"{corrected}\""));
            Some((
                new_line,
                vec![PathFix {
                    file: importer.to_string(),
                    from: spec,
                    to: corrected,
                }],
            ))
        }
        // ── Symbol-elsewhere: split the import to the defining file(s). ──
        Resolution::Found(target) => {
            let ni = parse_named_import(line)?;
            let target_exports = exports.get(target.as_str())?;
            let (mut stays, mut moves): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
            for tok in &ni.tokens {
                if target_exports.iter().any(|s| s == exported_name(tok)) {
                    stays.push(tok);
                } else {
                    moves.push(tok);
                }
            }
            if moves.is_empty() {
                return None; // every symbol resolves here
            }
            // Group each moved symbol under the single other file that defines it.
            let mut groups: Vec<(String, Vec<&str>)> = Vec::new();
            for tok in &moves {
                let defs: Vec<&str> = symbol_to_files
                    .get(exported_name(tok))
                    .map(|files| {
                        files
                            .iter()
                            .copied()
                            .filter(|p| *p != importer && *p != target.as_str())
                            .collect()
                    })
                    .unwrap_or_default();
                if defs.len() != 1 {
                    return None; // undefined or ambiguous — leave for cleanup
                }
                let new_spec = relative_specifier(importer, defs[0]);
                if !matches!(resolve(importer, &new_spec, paths), Resolution::Found(_)) {
                    return None;
                }
                match groups.iter_mut().find(|(s, _)| *s == new_spec) {
                    Some((_, toks)) => toks.push(tok),
                    None => groups.push((new_spec, vec![tok])),
                }
            }

            let q = ni.quote;
            let semi = if ni.semicolon { ";" } else { "" };
            let mut lines = Vec::new();
            if !stays.is_empty() {
                lines.push(format!(
                    "{}{} {{ {} }} from {q}{}{q}{semi}",
                    ni.indent,
                    ni.keyword,
                    stays.join(", "),
                    spec
                ));
            }
            let mut pfixes = Vec::new();
            for (new_spec, toks) in &groups {
                lines.push(format!(
                    "{}{} {{ {} }} from {q}{}{q}{semi}",
                    ni.indent,
                    ni.keyword,
                    toks.join(", "),
                    new_spec
                ));
                pfixes.push(PathFix {
                    file: importer.to_string(),
                    from: spec.clone(),
                    to: new_spec.clone(),
                });
            }
            Some((lines.join("\n"), pfixes))
        }
    }
}

/// A parsed single-line named import (`import [type] { a, b as c } from '…'`).
struct NamedImport<'a> {
    indent: &'a str,
    keyword: &'a str, // "import" or "import type"
    tokens: Vec<&'a str>,
    quote: char,
    semicolon: bool,
}

fn parse_named_import(line: &str) -> Option<NamedImport<'_>> {
    let indent = &line[..line.len() - line.trim_start().len()];
    let t = line.trim_start();
    let rest = t.strip_prefix("import")?;
    let keyword =
        if rest.trim_start().starts_with("type ") || rest.trim_start().starts_with("type{") {
            "import type"
        } else {
            "import"
        };
    let open = line.find('{')?;
    let close = line[open..].find('}')? + open;
    let tokens: Vec<&str> = line[open + 1..close]
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if tokens.is_empty() {
        return None;
    }
    let spec = import_spec(line)?;
    let quote = if line.contains(&format!("\"{spec}\"")) {
        '"'
    } else {
        '\''
    };
    let semicolon = line.trim_end().ends_with(';');
    Some(NamedImport {
        indent,
        keyword,
        tokens,
        quote,
        semicolon,
    })
}

/// The exported name an import token references (`Foo`, `Foo as Bar`,
/// `type Foo` → `Foo`).
fn exported_name(token: &str) -> &str {
    let mut parts = token.split_whitespace();
    let first = parts.next().unwrap_or("");
    if first == "type" {
        parts.next().unwrap_or(first)
    } else {
        first
    }
}

/// A written file's module basename (filename without extension).
fn module_stem(path: &str) -> Option<String> {
    let file = path.rsplit('/').next()?;
    let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);
    (!stem.is_empty()).then(|| stem.to_string())
}

/// An import specifier's final segment without extension (`../../types/models`
/// → `models`).
fn spec_stem(spec: &str) -> Option<String> {
    let last = spec.rsplit('/').next()?;
    let stem = last.rsplit_once('.').map(|(s, _)| s).unwrap_or(last);
    (!stem.is_empty() && stem != "." && stem != "..").then(|| stem.to_string())
}

/// The correct extensionless relative specifier from `importer` to `target`.
fn relative_specifier(importer: &str, target: &str) -> String {
    let mut importer_dir: Vec<&str> = importer.split('/').collect();
    importer_dir.pop(); // drop the filename
    let target_noext = target.rsplit_once('.').map(|(s, _)| s).unwrap_or(target);
    let target_segments: Vec<&str> = target_noext.split('/').collect();

    let mut common = 0;
    while common < importer_dir.len()
        && common < target_segments.len()
        && importer_dir[common] == target_segments[common]
    {
        common += 1;
    }
    let ups = importer_dir.len() - common;
    let down = target_segments[common..].join("/");
    if ups == 0 {
        format!("./{down}")
    } else {
        format!("{}{}", "../".repeat(ups), down)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fw(path: &str, content: &str) -> FileWrite {
        FileWrite {
            path: path.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn clean_project_has_no_findings() {
        let files = vec![
            fw("src/models.ts", "export class Property {}\n"),
            fw(
                "src/app.ts",
                "import { Property } from './models';\nconst p = new Property();\n",
            ),
        ];
        let report = review(&files);
        assert!(report.is_clean(), "got:\n{}", report.render());
    }

    #[test]
    fn empty_file_is_flagged() {
        let files = vec![
            fw("src/a.ts", "   \n"),
            fw("src/b.ts", "export const x = 1;\n"),
        ];
        let report = review(&files);
        assert_eq!(report.empty_files, vec!["src/a.ts"]);
    }

    #[test]
    fn phantom_import_of_missing_file() {
        let files = vec![fw(
            "src/services/store.ts",
            "import { Contracts } from '../data/contracts';\n",
        )];
        let report = review(&files);
        assert_eq!(report.phantom_imports.len(), 1);
        assert!(
            report.phantom_imports[0]
                .detail
                .contains("no written file provides"),
            "got: {}",
            report.phantom_imports[0].detail
        );
    }

    #[test]
    fn alignment_scope_includes_implied_missing_local_module() {
        let files = vec![fw(
            "services/pricingService.ts",
            "import { PriceCalculator } from '../utils/priceCalculator';\n",
        )];
        let report = review(&files);
        assert_eq!(
            report.alignment_files(&files),
            vec![
                "services/pricingService.ts".to_string(),
                "utils/priceCalculator.ts".to_string(),
            ]
        );
    }

    #[test]
    fn phantom_import_of_missing_symbol() {
        let files = vec![
            fw("src/data/contracts.ts", "export const NONE = 1;\n"),
            fw(
                "src/services/store.ts",
                "import { Contracts } from '../data/contracts';\n",
            ),
        ];
        let report = review(&files);
        assert_eq!(report.phantom_imports.len(), 1);
        assert!(
            report.phantom_imports[0]
                .detail
                .contains("does not define it"),
            "got: {}",
            report.phantom_imports[0].detail
        );
    }

    #[test]
    fn existing_symbol_import_is_clean() {
        let files = vec![
            fw("src/data/contracts.ts", "export const Contracts = [];\n"),
            fw(
                "src/services/store.ts",
                "import { Contracts } from '../data/contracts';\n",
            ),
        ];
        assert!(review(&files).phantom_imports.is_empty());
    }

    #[test]
    fn external_packages_are_ignored() {
        let files = vec![fw(
            "src/app.tsx",
            "import React from 'react';\nimport { useState } from 'react';\n",
        )];
        assert!(review(&files).phantom_imports.is_empty());
    }

    #[test]
    fn default_import_requires_a_default_export() {
        let files = vec![
            fw("src/service.ts", "export const Service = 1;\n"),
            fw("src/app.ts", "import Service from './service';\n"),
        ];
        let report = review(&files);
        assert_eq!(report.phantom_imports.len(), 1);
        assert!(
            report.phantom_imports[0]
                .detail
                .contains("has no default export"),
            "got: {}",
            report.phantom_imports[0].detail
        );
    }

    #[test]
    fn over_root_relative_import_is_not_normalized_back_into_project() {
        let files = vec![
            fw("types/index.ts", "export interface Property {}\n"),
            fw(
                "pages/MapView.tsx",
                "import { Property } from '../../types/index';\n",
            ),
        ];
        let report = review(&files);
        assert_eq!(report.phantom_imports.len(), 1);
        assert!(
            report.phantom_imports[0]
                .detail
                .contains("no written file provides"),
            "got: {}",
            report.phantom_imports[0].detail
        );

        let (fixed, fixes) = fix_import_paths(&files);
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].to, "../types/index");
        assert!(
            fixed
                .iter()
                .find(|file| file.path == "pages/MapView.tsx")
                .unwrap()
                .content
                .contains("from '../types/index'")
        );
    }

    #[test]
    fn syntax_findings_are_folded_in() {
        let files = vec![fw("package.json", "{ bad json,\n")];
        let report = review(&files);
        assert_eq!(report.syntax.len(), 1);
        assert!(report.syntax[0].message.contains("invalid JSON"));
    }

    #[test]
    fn fixes_a_relative_path_that_climbs_too_far() {
        let files = vec![
            fw("src/types/models.ts", "export interface Contract {}\n"),
            fw(
                "src/components/ContractsView.tsx",
                "import { Contract } from '../../types/models';\nexport const View = 1;\n",
            ),
        ];
        let (fixed, fixes) = fix_import_paths(&files);
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].file, "src/components/ContractsView.tsx");
        assert_eq!(fixes[0].from, "../../types/models");
        assert_eq!(fixes[0].to, "../types/models");

        let view = fixed
            .iter()
            .find(|f| f.path == "src/components/ContractsView.tsx")
            .unwrap();
        assert!(view.content.contains("from '../types/models'"));
        assert!(
            view.content.ends_with("export const View = 1;\n"),
            "rest preserved"
        );
        // The correction makes the project audit clean.
        assert!(review(&fixed).phantom_imports.is_empty());
    }

    #[test]
    fn fix_leaves_resolving_imports_untouched() {
        let files = vec![
            fw("src/models.ts", "export const X = 1;\n"),
            fw("src/app.ts", "import { X } from './models';\n"),
        ];
        let (_fixed, fixes) = fix_import_paths(&files);
        assert!(fixes.is_empty());
    }

    #[test]
    fn fix_does_not_guess_when_basename_is_ambiguous() {
        let files = vec![
            fw("src/a/models.ts", "export const A = 1;\n"),
            fw("src/b/models.ts", "export const B = 1;\n"),
            fw("src/app.ts", "import { A } from './nowhere/models';\n"),
        ];
        let (_fixed, fixes) = fix_import_paths(&files);
        assert!(
            fixes.is_empty(),
            "ambiguous basename must not be auto-fixed"
        );
    }

    #[test]
    fn fix_does_not_touch_external_packages() {
        let files = vec![fw("src/app.tsx", "import React from 'react';\n")];
        let (_fixed, fixes) = fix_import_paths(&files);
        assert!(fixes.is_empty());
    }

    #[test]
    fn splits_a_mixed_import_to_the_defining_files() {
        // The `housing` run: Contract is in models.ts, the enums are in enums.ts,
        // but all five were imported from models.ts.
        let files = vec![
            fw(
                "src/types/enums.ts",
                "export enum Area { N }\nexport enum PropertyType { Studio }\n",
            ),
            fw("src/types/models.ts", "export interface Contract {}\n"),
            fw(
                "src/services/contractService.ts",
                "import { Contract, PropertyType, Area } from '../types/models';\nexport const x = 1;\n",
            ),
        ];
        let (fixed, fixes) = fix_import_paths(&files);
        let cs = fixed
            .iter()
            .find(|f| f.path == "src/services/contractService.ts")
            .unwrap();
        assert!(
            cs.content
                .contains("import { Contract } from '../types/models';"),
            "resolving symbol stays; got:\n{}",
            cs.content
        );
        assert!(
            cs.content
                .contains("import { PropertyType, Area } from '../types/enums';"),
            "misplaced symbols move to their defining file; got:\n{}",
            cs.content
        );
        assert!(
            cs.content.ends_with("export const x = 1;\n"),
            "rest preserved"
        );
        assert!(!fixes.is_empty());
        // The correction makes the project audit clean.
        assert!(review(&fixed).phantom_imports.is_empty());
    }

    #[test]
    fn retargets_an_import_whose_every_symbol_lives_elsewhere() {
        let files = vec![
            fw(
                "src/types/enums.ts",
                "export enum Area { N }\nexport enum Mode { Full }\n",
            ),
            fw("src/types/models.ts", "export interface Contract {}\n"),
            fw(
                "src/services/x.ts",
                "import { Area, Mode } from '../types/models';\n",
            ),
        ];
        let (fixed, _fixes) = fix_import_paths(&files);
        let x = fixed
            .iter()
            .find(|f| f.path == "src/services/x.ts")
            .unwrap();
        assert!(
            x.content
                .contains("import { Area, Mode } from '../types/enums';")
        );
        assert!(
            !x.content.contains("'../types/models'"),
            "no leftover broken import"
        );
    }

    #[test]
    fn does_not_split_ambiguous_or_undefined_symbols() {
        // `Thing` is defined in two files (ambiguous) — leave it for cleanup.
        let ambiguous = vec![
            fw("a.ts", "export const Thing = 1;\n"),
            fw("b.ts", "export const Thing = 2;\n"),
            fw("c.ts", "export interface Real {}\n"),
            fw("u.ts", "import { Real, Thing } from './c';\n"),
        ];
        let (_f, fixes) = fix_import_paths(&ambiguous);
        assert!(fixes.is_empty(), "ambiguous symbol must not be auto-moved");

        // `Ghost` is exported by no file — untouched.
        let undefined = vec![
            fw("c.ts", "export interface Real {}\n"),
            fw("u.ts", "import { Real, Ghost } from './c';\n"),
        ];
        let (_f2, fixes2) = fix_import_paths(&undefined);
        assert!(fixes2.is_empty());
    }

    #[test]
    fn design_anti_patterns_are_folded_into_the_report() {
        let files = vec![fw(
            "src/App.tsx",
            "export const App = () => <div className=\"bg-blue-600 text-gray-400\">hi</div>;\n",
        )];
        let report = review(&files);
        assert!(!report.is_clean());
        assert_eq!(report.design.len(), 1);
        assert_eq!(report.design[0].rule, "gray-on-color");
        assert!(
            report
                .for_file("src/App.tsx")
                .contains("Gray text on colored background")
        );
        assert!(report.render().contains("Design anti-patterns"));
    }

    #[test]
    fn react_web_project_missing_scaffold_is_flagged() {
        let files = vec![fw(
            "src/App.tsx",
            "import React from 'react';\nexport const App = () => <main />;\n",
        )];
        let report = review(&files);
        assert!(!report.is_clean());
        assert!(
            report
                .scaffold
                .iter()
                .any(|finding| finding.file == "package.json"),
            "missing package metadata should be flagged: {}",
            report.render()
        );
        assert!(
            report
                .scaffold
                .iter()
                .any(|finding| finding.file == "index.html"),
            "missing HTML entrypoint should be flagged: {}",
            report.render()
        );
        assert!(
            report
                .scaffold
                .iter()
                .any(|finding| finding.file == "tsconfig.json"),
            "missing TypeScript config should be flagged: {}",
            report.render()
        );
        assert!(
            report
                .scaffold
                .iter()
                .any(|finding| finding.file == "src/main.tsx"),
            "missing app mount entrypoint should be flagged: {}",
            report.render()
        );
        assert!(report.render().contains("Scaffold findings"));
    }

    #[test]
    fn scaffold_checker_respects_existing_project_files() {
        let files = vec![fw(
            "src/App.tsx",
            "import React from 'react';\nexport const App = () => <main />;\n",
        )];
        let existing = "package.json\nindex.html\ntsconfig.json\nsrc/main.tsx";
        let report = review_with_project_files(&files, existing);
        assert!(
            report.scaffold.is_empty(),
            "existing scaffold should satisfy the checker: {}",
            report.render()
        );
    }

    #[test]
    fn package_json_must_declare_external_imports() {
        let files = vec![
            fw(
                "package.json",
                "{\"dependencies\":{\"react\":\"latest\"},\"devDependencies\":{}}",
            ),
            fw(
                "components/Layout.tsx",
                "import React from 'react';\nimport { AppBar } from '@mui/material';\nimport 'leaflet/dist/leaflet.css';\n",
            ),
        ];
        let report = review(&files);
        assert!(
            report
                .scaffold
                .iter()
                .any(|finding| finding.file == "package.json"
                    && finding.detail.contains("@mui/material")),
            "missing @mui/material should be flagged: {}",
            report.render()
        );
        assert!(
            report
                .scaffold
                .iter()
                .any(|finding| finding.file == "package.json" && finding.detail.contains("leaflet")),
            "missing leaflet should be flagged: {}",
            report.render()
        );
    }

    #[test]
    fn vite_entry_and_tsconfig_include_must_match_written_files() {
        let files = vec![
            fw(
                "package.json",
                "{\"dependencies\":{\"react\":\"latest\",\"react-dom\":\"latest\"},\"devDependencies\":{\"vite\":\"latest\"}}",
            ),
            fw(
                "index.html",
                "<div id=\"root\"></div><script src=\"/path/to/your/bundle.js\"></script>",
            ),
            fw("tsconfig.json", "{\"include\":[\"src/**/*\"]}"),
            fw(
                "main.tsx",
                "import React from 'react';\nexport default function Main() { return <div />; }\n",
            ),
            fw(
                "pages/Dashboard.tsx",
                "import React from 'react';\nexport default function Dashboard() { return <main />; }\n",
            ),
        ];
        let report = review(&files);
        assert!(
            report
                .scaffold
                .iter()
                .any(|finding| finding.file == "index.html"
                    && finding.detail.contains("module script")),
            "bad Vite entrypoint should be flagged: {}",
            report.render()
        );
        assert!(
            report
                .scaffold
                .iter()
                .any(|finding| finding.file == "tsconfig.json"
                    && finding.detail.contains("main.tsx")),
            "tsconfig include gap should be flagged: {}",
            report.render()
        );
    }

    #[test]
    fn placeholder_comments_are_not_clean() {
        let files = vec![fw(
            "services/contract.ts",
            "export function assign() {\n  // Implement matching logic here\n}\n",
        )];
        let report = review(&files);
        assert!(
            report
                .scaffold
                .iter()
                .any(|finding| finding.file == "services/contract.ts"
                    && finding.detail.contains("placeholder implementation")),
            "placeholder logic should be flagged: {}",
            report.render()
        );
    }

    #[test]
    fn python_project_missing_scaffold_is_flagged() {
        let files = vec![fw(
            "src/app/main.py",
            "from fastapi import FastAPI\n\napp = FastAPI()\n",
        )];
        let report = review(&files);
        assert!(!report.is_clean());
        assert!(
            report
                .scaffold
                .iter()
                .any(|finding| finding.file == "pyproject.toml"),
            "missing Python project metadata should be flagged: {}",
            report.render()
        );

        let report = review_with_project_files(&files, "requirements.txt");
        assert!(
            report.scaffold.is_empty(),
            "requirements.txt should satisfy Python project metadata: {}",
            report.render()
        );
    }

    #[test]
    fn report_files_lists_every_implicated_path() {
        let files = vec![
            fw("src/a.ts", ""),
            fw("src/b.ts", "import { Gone } from './missing';\n"),
        ];
        let report = review(&files);
        assert_eq!(report.files(), vec!["src/a.ts", "src/b.ts"]);
        assert!(!report.is_clean());
    }
}

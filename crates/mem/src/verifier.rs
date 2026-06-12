//! Sandboxed real-tool verification for `/work`.
//!
//! This is deliberately narrow: commands are detected from manifests, run from
//! a temporary copy of the workspace, and never come from model text. Failures
//! become evidence for the same Concierge repair river; this module does not
//! rewrite source files.

use crate::config::VerifierConfig;
use anyhow::{Context, Result};
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const OUTPUT_TAIL_BYTES: usize = 12_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifyCommand {
    pub kind: VerifyKind,
    pub program: String,
    pub args: Vec<String>,
    pub timeout: Duration,
}

impl VerifyCommand {
    fn new(
        kind: VerifyKind,
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
        timeout: Duration,
    ) -> Self {
        Self {
            kind,
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            timeout,
        }
    }

    pub(crate) fn display(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerifyKind {
    PackageInstall,
    PackageBuild,
    PackageTest,
    CargoTest,
    GoTest,
    PythonCompile,
    PythonTest,
}

impl VerifyKind {
    fn allows_network(self) -> bool {
        matches!(self, VerifyKind::PackageInstall)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Isolation {
    Bubblewrap,
    MacosSandboxExec,
    WorkspaceCopy,
}

impl Isolation {
    fn label(self) -> &'static str {
        match self {
            Isolation::Bubblewrap => "bubblewrap",
            Isolation::MacosSandboxExec => "sandbox-exec",
            Isolation::WorkspaceCopy => "workspace-copy",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifyStepResult {
    pub command: VerifyCommand,
    pub isolation: Isolation,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout_tail: String,
    pub stderr_tail: String,
}

impl VerifyStepResult {
    pub(crate) fn passed(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out
    }

    pub(crate) fn render(&self) -> String {
        format!(
            "command: {}\nisolation: {}\nexit_code: {}\ntimed_out: {}\nstdout_tail:\n{}\nstderr_tail:\n{}",
            self.command.display(),
            self.isolation.label(),
            self.exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.timed_out,
            self.stdout_tail,
            self.stderr_tail
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerificationReport {
    pub skipped: Option<String>,
    pub steps: Vec<VerifyStepResult>,
}

impl VerificationReport {
    pub(crate) fn skipped(reason: impl Into<String>) -> Self {
        Self {
            skipped: Some(reason.into()),
            steps: Vec::new(),
        }
    }

    pub(crate) fn passed(&self) -> bool {
        self.skipped.is_none() && self.steps.iter().all(VerifyStepResult::passed)
    }

    pub(crate) fn failed(&self) -> bool {
        self.skipped.is_none() && self.steps.iter().any(|step| !step.passed())
    }

    pub(crate) fn render(&self) -> String {
        if let Some(reason) = &self.skipped {
            return format!("Verification skipped: {reason}");
        }
        let mut out = String::new();
        for (idx, step) in self.steps.iter().enumerate() {
            if idx > 0 {
                out.push_str("\n\n");
            }
            let _ = write!(out, "{}", step.render());
        }
        out
    }

    pub(crate) fn findings(&self, workspace_root: &Path) -> Vec<VerificationFinding> {
        let mut findings = Vec::new();
        for step in self.steps.iter().filter(|step| !step.passed()) {
            let rendered = step.render();
            let files = files_in_output(workspace_root, &rendered);
            let files = if files.is_empty() {
                fallback_files(workspace_root, step.command.kind)
            } else {
                files
            };
            for file in files {
                findings.push(VerificationFinding {
                    file,
                    detail: format!(
                        "sandboxed verifier failed `{}`:\n{}",
                        step.command.display(),
                        rendered
                    ),
                });
            }
        }
        findings
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerificationFinding {
    pub file: String,
    pub detail: String,
}

pub(crate) fn verify_project(
    workspace_root: &Path,
    cfg: &VerifierConfig,
) -> Result<VerificationReport> {
    if !cfg.enabled {
        return Ok(VerificationReport::skipped("disabled in config"));
    }
    let commands = detect_commands(workspace_root, cfg);
    if commands.is_empty() {
        return Ok(VerificationReport::skipped(
            "no supported build or test manifest detected",
        ));
    }

    let sandbox = Sandbox::copy_from(workspace_root)?;
    let mut steps = Vec::new();
    for command in commands {
        let result = run_command(sandbox.path(), &command)?;
        let failed = !result.passed();
        steps.push(result);
        if failed {
            break;
        }
    }
    Ok(VerificationReport {
        skipped: None,
        steps,
    })
}

pub(crate) fn detect_commands(workspace_root: &Path, cfg: &VerifierConfig) -> Vec<VerifyCommand> {
    let timeout = Duration::from_secs(cfg.timeout_seconds.max(1));
    let has = |path: &str| workspace_root.join(path).exists();
    let mut commands = Vec::new();

    if has("package.json") {
        if cfg.install {
            commands.push(VerifyCommand::new(
                VerifyKind::PackageInstall,
                "npm",
                ["install", "--ignore-scripts", "--no-audit", "--no-fund"],
                timeout,
            ));
        }
        commands.push(VerifyCommand::new(
            VerifyKind::PackageBuild,
            "npm",
            ["run", "build", "--if-present"],
            timeout,
        ));
        if cfg.test {
            commands.push(VerifyCommand::new(
                VerifyKind::PackageTest,
                "npm",
                ["test", "--if-present"],
                timeout,
            ));
        }
    } else if has("Cargo.toml") {
        commands.push(VerifyCommand::new(
            VerifyKind::CargoTest,
            "cargo",
            ["test"],
            timeout,
        ));
    } else if has("go.mod") {
        commands.push(VerifyCommand::new(
            VerifyKind::GoTest,
            "go",
            ["test", "./..."],
            timeout,
        ));
    } else if has("pyproject.toml") || has("requirements.txt") || has("setup.py") {
        commands.push(VerifyCommand::new(
            VerifyKind::PythonCompile,
            "python3",
            ["-m", "compileall", "-q", "."],
            timeout,
        ));
        if cfg.test && python_tests_present(workspace_root) {
            commands.push(VerifyCommand::new(
                VerifyKind::PythonTest,
                "python3",
                ["-m", "pytest", "-q"],
                timeout,
            ));
        }
    }

    commands
}

fn python_tests_present(workspace_root: &Path) -> bool {
    workspace_root.join("pytest.ini").exists()
        || workspace_root.join("tests").is_dir()
        || fs::read_to_string(workspace_root.join("pyproject.toml"))
            .map(|text| text.contains("[tool.pytest"))
            .unwrap_or(false)
}

fn run_command(workspace: &Path, command: &VerifyCommand) -> Result<VerifyStepResult> {
    let out_path = workspace.join(".concierge-verify-stdout.log");
    let err_path = workspace.join(".concierge-verify-stderr.log");
    let stdout = File::create(&out_path).context("create verifier stdout capture")?;
    let stderr = File::create(&err_path).context("create verifier stderr capture")?;
    let home = workspace.join(".concierge-verify-home");
    let tmp = workspace.join(".concierge-verify-tmp");
    let npm_cache = workspace.join(".concierge-verify-npm-cache");
    let cargo_home = workspace.join(".concierge-verify-cargo");
    let go_cache = workspace.join(".concierge-verify-gocache");
    let go_mod_cache = workspace.join(".concierge-verify-gomodcache");
    let pycache = workspace.join(".concierge-verify-pycache");
    fs::create_dir_all(&home).context("create verifier home")?;
    fs::create_dir_all(&tmp).context("create verifier tmp")?;
    fs::create_dir_all(&npm_cache).context("create verifier npm cache")?;
    fs::create_dir_all(&cargo_home).context("create verifier cargo home")?;
    fs::create_dir_all(&go_cache).context("create verifier go cache")?;
    fs::create_dir_all(&go_mod_cache).context("create verifier go mod cache")?;
    fs::create_dir_all(&pycache).context("create verifier python cache")?;

    let (mut process, isolation) = sandboxed_process(workspace, command);
    let mut child = process
        .current_dir(workspace)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("TMPDIR", &tmp)
        .env("TEMP", &tmp)
        .env("TMP", &tmp)
        .env("CI", "true")
        .env("NPM_CONFIG_CACHE", &npm_cache)
        .env("NPM_CONFIG_IGNORE_SCRIPTS", "true")
        .env("NPM_CONFIG_AUDIT", "false")
        .env("NPM_CONFIG_FUND", "false")
        .env("CARGO_HOME", &cargo_home)
        .env("CARGO_TARGET_DIR", workspace.join("target"))
        .env("GOCACHE", &go_cache)
        .env("GOMODCACHE", &go_mod_cache)
        .env("PYTHONPYCACHEPREFIX", &pycache)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn();

    let started = Instant::now();
    let (exit_code, timed_out) = match child.as_mut() {
        Ok(child) => loop {
            if let Some(status) = child.try_wait().context("poll verifier command")? {
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
            fs::write(
                &err_path,
                format!("failed to spawn verifier command: {err}"),
            )
            .context("write spawn failure")?;
            (None, false)
        }
    };

    let stdout_tail = read_tail(&out_path)?;
    let stderr_tail = read_tail(&err_path)?;
    let _ = fs::remove_file(&out_path);
    let _ = fs::remove_file(&err_path);

    Ok(VerifyStepResult {
        command: command.clone(),
        isolation,
        exit_code,
        timed_out,
        stdout_tail,
        stderr_tail,
    })
}

fn sandboxed_process(workspace: &Path, command: &VerifyCommand) -> (Command, Isolation) {
    if command_exists("bwrap") {
        let mut process = Command::new("bwrap");
        process.args(build_bwrap_args(
            workspace,
            command.kind.allows_network(),
            &command.program,
            &command.args,
        ));
        return (process, Isolation::Bubblewrap);
    }

    if command_exists("sandbox-exec") && macos_sandbox_usable() {
        let mut process = Command::new("sandbox-exec");
        process
            .arg("-p")
            .arg(build_macos_sandbox_profile(
                workspace,
                command.kind.allows_network(),
            ))
            .arg(&command.program)
            .args(&command.args);
        return (process, Isolation::MacosSandboxExec);
    }

    let mut process = Command::new(&command.program);
    process.args(&command.args);
    (process, Isolation::WorkspaceCopy)
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
        workspace,
        "--die-with-parent".to_string(),
        "--unshare-pid".to_string(),
    ];
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
    let network_rule = if allow_network {
        ""
    } else {
        "\n(deny network*)"
    };
    format!(
        "(version 1)\n\
         (allow default)\n\
         (deny file-write*)\n\
         (allow file-write*{write_filters}){network_rule}"
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

fn read_tail(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let start = bytes.len().saturating_sub(OUTPUT_TAIL_BYTES);
    Ok(String::from_utf8_lossy(&bytes[start..]).to_string())
}

struct Sandbox {
    path: PathBuf,
}

impl Sandbox {
    fn copy_from(workspace_root: &Path) -> Result<Self> {
        let root = unique_sandbox_path();
        fs::create_dir_all(&root).with_context(|| format!("create {}", root.display()))?;
        copy_workspace(workspace_root, &root)?;
        Ok(Self { path: root })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn unique_sandbox_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("mem-verify-{}-{nanos}", std::process::id()))
}

fn copy_workspace(src: &Path, dst: &Path) -> Result<()> {
    for entry in fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if should_skip(&name) {
            continue;
        }
        let dest = dst.join(&name);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            fs::create_dir_all(&dest)?;
            copy_workspace(&path, &dest)?;
        } else if metadata.is_file() {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&path, &dest)
                .with_context(|| format!("copy {} -> {}", path.display(), dest.display()))?;
        }
    }
    Ok(())
}

fn should_skip(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            ".concierge"
                | ".git"
                | "node_modules"
                | "target"
                | "dist"
                | "build"
                | "coverage"
                | ".next"
                | ".turbo"
                | ".cache"
                | "__pycache__"
        )
    )
}

pub(crate) fn files_in_output(workspace_root: &Path, output: &str) -> Vec<String> {
    let delimiters = [
        ' ', '\t', '\n', '\r', '(', ')', '[', ']', ':', ',', '"', '\'', '`',
    ];
    let mut files = Vec::new();
    for token in output.split(|c| delimiters.contains(&c)) {
        let rel = token
            .trim()
            .strip_prefix("./")
            .unwrap_or(token.trim())
            .trim_start_matches('/');
        if rel.is_empty() || !rel.contains('.') {
            continue;
        }
        if workspace_root.join(rel).is_file() && !files.iter().any(|file| file == rel) {
            files.push(rel.to_string());
        }
    }
    files
}

fn fallback_files(workspace_root: &Path, kind: VerifyKind) -> Vec<String> {
    let candidates: &[&str] = match kind {
        VerifyKind::PackageInstall | VerifyKind::PackageBuild | VerifyKind::PackageTest => {
            &["package.json"]
        }
        VerifyKind::CargoTest => &["Cargo.toml"],
        VerifyKind::GoTest => &["go.mod"],
        VerifyKind::PythonCompile | VerifyKind::PythonTest => {
            &["pyproject.toml", "requirements.txt", "setup.py"]
        }
    };
    candidates
        .iter()
        .find(|path| workspace_root.join(path).exists())
        .map(|path| vec![(*path).to_string()])
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    fn cfg() -> VerifierConfig {
        VerifierConfig {
            enabled: true,
            install: false,
            test: true,
            timeout_seconds: 1,
        }
    }

    #[test]
    fn detect_package_commands_are_allowlisted_vectors() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        let commands = detect_commands(dir.path(), &cfg());
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].program, "npm");
        assert_eq!(commands[0].args, vec!["run", "build", "--if-present"]);
        assert_eq!(commands[1].args, vec!["test", "--if-present"]);
    }

    #[test]
    fn disabled_verifier_skips() {
        let dir = TempDir::new().unwrap();
        let report = verify_project(
            dir.path(),
            &VerifierConfig {
                enabled: false,
                ..cfg()
            },
        )
        .unwrap();
        assert_eq!(report.skipped.as_deref(), Some("disabled in config"));
    }

    #[test]
    fn sandbox_copy_skips_mutable_and_build_dirs() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join(".concierge")).unwrap();
        fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        fs::write(dir.path().join(".concierge/names.json"), "{}").unwrap();
        fs::write(dir.path().join("main.py"), "print('ok')\n").unwrap();

        let sandbox = Sandbox::copy_from(dir.path()).unwrap();
        assert!(sandbox.path().join("main.py").exists());
        assert!(!sandbox.path().join(".concierge").exists());
        assert!(!sandbox.path().join("node_modules").exists());
    }

    #[test]
    fn bwrap_args_follow_readonly_workspace_write_pattern() {
        let args = build_bwrap_args(
            Path::new("/tmp/work"),
            false,
            "npm",
            &["run".to_string(), "build".to_string()],
        );
        assert!(
            args.windows(3)
                .any(|window| window[0] == "--ro-bind" && window[1] == "/" && window[2] == "/")
        );
        assert!(args.windows(3).any(|window| window[0] == "--bind"
            && window[1] == "/tmp/work"
            && window[2] == "/tmp/work"));
        assert!(args.iter().any(|arg| arg == "--unshare-net"));
        let command_tail: Vec<&str> = args[args.len() - 4..].iter().map(String::as_str).collect();
        assert_eq!(command_tail, vec!["--", "npm", "run", "build"]);
    }

    #[test]
    fn macos_profile_restricts_writes_and_can_restrict_network() {
        let profile = build_macos_sandbox_profile(Path::new("/tmp/work"), false);
        assert!(profile.contains("(deny file-write*)"));
        assert!(profile.contains("(allow file-write* (subpath \"/tmp/work\"))"));
        assert!(profile.contains("(deny network*)"));

        let install_profile = build_macos_sandbox_profile(Path::new("/tmp/work"), true);
        assert!(!install_profile.contains("(deny network*)"));
    }

    #[test]
    fn files_in_output_extracts_existing_paths() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.ts"), "").unwrap();
        let files = files_in_output(dir.path(), "src/main.ts(3,1): error\nmissing src/other.ts");
        assert_eq!(files, vec!["src/main.ts"]);
    }

    #[test]
    fn python_compile_failure_becomes_file_finding() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "[project]\nname='x'\n").unwrap();
        fs::write(dir.path().join("main.py"), "def f(:\n").unwrap();
        let report = verify_project(dir.path(), &cfg()).unwrap();
        assert!(report.failed(), "report: {}", report.render());
        let findings = report.findings(dir.path());
        assert!(
            findings.iter().any(|finding| finding.file == "main.py"),
            "findings: {findings:?}\n{}",
            report.render()
        );
    }

    #[test]
    fn timeout_is_reported_without_shell_input() {
        let dir = TempDir::new().unwrap();
        let command = VerifyCommand::new(
            VerifyKind::PythonCompile,
            "python3",
            ["-c", "import time; time.sleep(2)"],
            Duration::from_millis(20),
        );
        let result = run_command(dir.path(), &command).unwrap();
        assert!(result.timed_out);
        assert!(!result.passed());
        assert!(matches!(
            result.isolation,
            Isolation::Bubblewrap | Isolation::MacosSandboxExec | Isolation::WorkspaceCopy
        ));
    }
}

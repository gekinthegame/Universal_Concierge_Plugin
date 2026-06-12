use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

fn write_quiet_config(root: &Path) {
    let config_dir = root.join(".concierge");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.toml"), "[trace]\nverbosity = 0\n").unwrap();
}

fn mem(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mem"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}

fn mem_stdin(root: &Path, args: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mem"))
        .current_dir(root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[cfg(feature = "pinata")]
fn mem_without_pinata_jwt(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mem"))
        .current_dir(root)
        .args(args)
        .env_remove("PINATA_JWT")
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn read_config(root: &Path) -> toml::Value {
    let config_text = std::fs::read_to_string(root.join(".concierge/config.toml")).unwrap();
    toml::from_str(&config_text).unwrap()
}

fn output_field(output: &Output, key: &str) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .find_map(|line| {
            let (field, value) = line.split_once('\t')?;
            (field == key).then(|| value.to_string())
        })
        .unwrap_or_else(|| panic!("missing {key:?} in stdout:\n{stdout}"))
}

fn plugin_file_stats(output: &Output) -> BTreeMap<String, (u64, u64)> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let field = fields.next()?;
            if field != "plugin_file" {
                return None;
            }
            let path = fields.next()?.to_string();
            let records = fields.next()?.parse().unwrap();
            let failures = fields.next()?.parse().unwrap();
            Some((path, (records, failures)))
        })
        .collect()
}

fn json_cid_to_string(v: &serde_json::Value) -> String {
    let bytes: Vec<u8> = v
        .as_array()
        .expect("CID should be an array of bytes")
        .iter()
        .map(|b| b.as_u64().unwrap() as u8)
        .collect();
    cid::Cid::try_from(bytes).unwrap().to_string()
}

#[test]
fn init_creates_project_directory_and_keeps_store_under_dot_concierge() {
    let dir = TempDir::new().unwrap();
    let output = mem_stdin(dir.path(), &["init"], "project-a\nn\nn\n");
    assert_success(&output);

    let project = dir.path().join("project-a");
    assert!(project.is_dir());
    assert!(project.join(".concierge/config.toml").exists());
    assert!(!dir.path().join(".concierge/config.toml").exists());

    let config = read_config(&project);
    assert_eq!(config["store"]["root"].as_str(), Some(".concierge"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Project directory"));
    assert!(stdout.contains("memory store:"));
}

#[test]
fn init_configures_concierge_model_and_can_skip_ollama_pull() {
    let dir = TempDir::new().unwrap();
    let output = mem_stdin(
        dir.path(),
        &["init"],
        "project-model\ny\nollama\nhttp://localhost:11434\nmistral\nn\nn\n",
    );
    assert_success(&output);

    let config = read_config(&dir.path().join("project-model"));
    assert_eq!(config["store"]["root"].as_str(), Some(".concierge"));
    assert_eq!(
        config["models"]["concierge"]["provider"].as_str(),
        Some("ollama")
    );
    assert_eq!(
        config["models"]["concierge"]["host"].as_str(),
        Some("http://localhost:11434")
    );
    assert_eq!(
        config["models"]["concierge"]["name"].as_str(),
        Some("mistral")
    );
}

#[test]
fn init_configures_cleanup_model_after_the_worker() {
    let dir = TempDir::new().unwrap();
    // dir, skip concierge, add worker (provider/host/name, skip pull),
    // then add cleanup (provider/host/name, skip pull).
    let output = mem_stdin(
        dir.path(),
        &["init"],
        "project-three\nn\ny\nollama\nhttp://localhost:11434\nqwen2.5-coder\nn\ny\nollama\nhttp://localhost:11434\nrnj-1:8b\nn\n",
    );
    assert_success(&output);

    let config = read_config(&dir.path().join("project-three"));
    assert_eq!(
        config["models"]["worker"]["name"].as_str(),
        Some("qwen2.5-coder")
    );
    assert_eq!(
        config["models"]["cleanup"]["name"].as_str(),
        Some("rnj-1:8b")
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Add a cleanup model?"));
    assert!(stdout.contains("cleanup → rnj-1:8b"));
}

#[test]
fn init_does_not_prompt_for_cleanup_without_a_worker() {
    let dir = TempDir::new().unwrap();
    let output = mem_stdin(dir.path(), &["init"], "project-no-worker\nn\nn\n");
    assert_success(&output);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Add a cleanup model?"),
        "cleanup is only offered after a worker is added"
    );
    let config = read_config(&dir.path().join("project-no-worker"));
    assert!(
        config
            .get("models")
            .and_then(|m| m.get("cleanup"))
            .is_none()
    );
}

#[test]
fn chat_without_config_runs_first_run_setup_before_starting_repl() {
    let dir = TempDir::new().unwrap();
    let output = mem_stdin(dir.path(), &["chat"], "project-chat\nn\nn\n");
    assert_success(&output);

    let project = dir.path().join("project-chat");
    assert!(project.join(".concierge/config.toml").exists());
    let config = read_config(&project);
    assert_eq!(config["store"]["root"].as_str(), Some(".concierge"));

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Project directory"));
    assert!(stdout.contains("No .concierge/config.toml found in this project"));
    assert!(stdout.contains("concierge ready"));
}

#[test]
fn chat_with_existing_config_still_prompts_for_project_directory() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("chosen-chat");
    write_quiet_config(&project);

    let output = mem_stdin(dir.path(), &["chat"], "chosen-chat\n/quit\n");
    assert_success(&output);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Project directory"));
    assert!(stdout.contains("working in"));
    assert!(stdout.contains("chosen-chat"));
    assert!(stdout.contains("concierge ready"));
    assert!(!stdout.contains("Starting first-run setup"));
}

#[test]
fn chat_reports_configured_worker_role() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("worker-chat");
    std::fs::create_dir_all(project.join(".concierge")).unwrap();
    std::fs::write(
        project.join(".concierge/config.toml"),
        r#"
[trace]
verbosity = 0

[models.concierge]
provider = "ollama"
host = "http://localhost:11434"
name = "front"

[models.worker]
provider = "ollama"
host = "http://localhost:11434"
name = "coder"
"#,
    )
    .unwrap();

    let output = mem_stdin(dir.path(), &["chat"], "worker-chat\n/quit\n");
    assert_success(&output);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("concierge ready (front, worker coder)"));
}

#[test]
fn put_get_bind_resolve_and_cat_use_the_local_store() {
    let dir = TempDir::new().unwrap();
    write_quiet_config(dir.path());

    let put = mem(
        dir.path(),
        &[
            "put",
            r#"{"type":"memory","text":"hello cli","kind":"project"}"#,
        ],
    );
    assert_success(&put);
    assert_eq!(put.stderr, b"", "trace verbosity 0 must silence put");
    let cid = String::from_utf8(put.stdout).unwrap().trim().to_string();
    assert!(cid.starts_with("bafy"), "expected CID, got {cid}");

    let get = mem(dir.path(), &["get", &cid]);
    assert_success(&get);
    let record: serde_json::Value = serde_json::from_slice(&get.stdout).unwrap();
    assert_eq!(record["body"]["type"], "memory");
    assert_eq!(record["body"]["text"], "hello cli");
    assert_eq!(record["body"]["kind"], "project");

    let bind = mem(dir.path(), &["bind", "current-project", &cid]);
    assert_success(&bind);
    assert_eq!(bind.stdout, b"");
    assert_eq!(bind.stderr, b"");

    let resolve = mem(dir.path(), &["resolve", "current-project"]);
    assert_success(&resolve);
    assert_eq!(String::from_utf8(resolve.stdout).unwrap().trim(), cid);

    let cat = mem(dir.path(), &["cat", "current-project"]);
    assert_success(&cat);
    let record: serde_json::Value = serde_json::from_slice(&cat.stdout).unwrap();
    assert_eq!(record["body"]["text"], "hello cli");
}

#[test]
fn gc_sweeps_an_orphan_and_get_cat_return_its_tombstone_receipt() {
    let dir = TempDir::new().unwrap();
    write_quiet_config(dir.path());

    // An orphan: stored but never bound, so nothing can reach it.
    let put = mem(
        dir.path(),
        &[
            "put",
            r#"{"type":"memory","text":"throwaway","kind":"project"}"#,
        ],
    );
    assert_success(&put);
    let cid = String::from_utf8(put.stdout).unwrap().trim().to_string();

    // GC sweeps it and reports one orphan pruned.
    let gc = mem(dir.path(), &["gc"]);
    assert_success(&gc);
    let summary = String::from_utf8_lossy(&gc.stdout);
    assert!(
        summary.contains("pruned 1 (0 checkpoints, 1 orphans)"),
        "gc summary:\n{summary}"
    );

    // Fetching the pruned CID is NOT an error — it returns a receipt of truth.
    let get = mem(dir.path(), &["get", &cid]);
    assert_success(&get);
    let receipt = String::from_utf8_lossy(&get.stdout);
    assert!(receipt.contains(&format!("{cid} was pruned")), "{receipt}");
    assert!(receipt.contains("reason: orphan"), "{receipt}");
    assert!(
        receipt.contains("died:"),
        "receipt shows a time of death:\n{receipt}"
    );

    let cat = mem(dir.path(), &["cat", &cid]);
    assert_success(&cat);
    let receipt = String::from_utf8_lossy(&cat.stdout);
    assert!(receipt.contains(&format!("{cid} was pruned")), "{receipt}");
    assert!(receipt.contains("reason: orphan"), "{receipt}");
}

#[cfg(not(any(feature = "pinata", feature = "ipfs")))]
#[test]
fn backend_list_reports_no_compiled_backends_by_default() {
    let dir = TempDir::new().unwrap();
    write_quiet_config(dir.path());

    let output = mem(dir.path(), &["backend", "list"]);
    assert_success(&output);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("no backends compiled in"),
        "stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[cfg(feature = "pinata")]
#[test]
fn backend_list_reports_pinata_manifest_when_feature_enabled() {
    let dir = TempDir::new().unwrap();
    write_quiet_config(dir.path());

    let output = mem(dir.path(), &["backend", "list"]);
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("pinata\tPinata (IPFS, CAR upload)"));
    assert!(stdout.contains("env PINATA_JWT (secret)"));
    assert!(stdout.contains("Pinata JWT"));
    assert!(
        !stdout.contains("no backends compiled in"),
        "feature-enabled registry should not report an empty backend set"
    );
}

#[cfg(feature = "pinata")]
#[test]
fn backend_add_configures_pinata_and_prints_manifest_requirements() {
    let dir = TempDir::new().unwrap();
    write_quiet_config(dir.path());

    let output = mem(dir.path(), &["backend", "add", "pinata"]);
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("configured backend \"pinata\""));
    assert!(stdout.contains("required environment:"));
    assert!(stdout.contains("export PINATA_JWT=<value>"));
    assert!(stdout.contains("Pinata JWT"));

    let config_text = std::fs::read_to_string(dir.path().join(".concierge/config.toml")).unwrap();
    let config: toml::Value = toml::from_str(&config_text).unwrap();
    assert_eq!(config["trace"]["verbosity"].as_integer(), Some(0));
    assert_eq!(config["backend"]["name"].as_str(), Some("pinata"));
}

#[test]
fn share_without_a_configured_backend_errors_clearly() {
    let dir = TempDir::new().unwrap();
    write_quiet_config(dir.path());

    // store something so the target parses to a real CID and reaches the
    // backend-selection step
    let put = mem(
        dir.path(),
        &["put", r#"{"type":"memory","text":"x","kind":"project"}"#],
    );
    assert_success(&put);
    let cid = String::from_utf8(put.stdout).unwrap().trim().to_string();

    let output = mem(dir.path(), &["share", &cid]);
    assert!(
        !output.status.success(),
        "share must fail when no backend is configured"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no backend configured"),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn resume_without_target_defaults_to_latest() {
    let dir = TempDir::new().unwrap();
    write_quiet_config(dir.path());

    let output = mem(dir.path(), &["resume"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no name bound: \"latest\""),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(feature = "pinata")]
#[test]
fn share_with_pinata_configured_requires_manifest_env() {
    let dir = TempDir::new().unwrap();
    write_quiet_config(dir.path());

    let add = mem(dir.path(), &["backend", "add", "pinata"]);
    assert_success(&add);

    let put = mem(
        dir.path(),
        &["put", r#"{"type":"memory","text":"x","kind":"project"}"#],
    );
    assert_success(&put);
    let cid = String::from_utf8(put.stdout).unwrap().trim().to_string();

    let output = mem_without_pinata_jwt(dir.path(), &["share", &cid]);
    assert!(
        !output.status.success(),
        "share must fail clearly until the manifest-required env is set"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("missing required environment variable: PINATA_JWT"),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn model_set_then_list_swaps_the_role() {
    let dir = TempDir::new().unwrap();

    let set = mem(
        dir.path(),
        &["model", "set", "concierge", "--name", "qwen2.5"],
    );
    assert_success(&set);

    let list = mem(dir.path(), &["model", "list"]);
    assert_success(&list);
    let out = String::from_utf8_lossy(&list.stdout);
    assert!(out.contains("concierge"), "list:\n{out}");
    assert!(out.contains("qwen2.5"), "list:\n{out}");

    // the swap is persisted, with provider/host kept at their defaults
    let cfg = std::fs::read_to_string(dir.path().join(".concierge/config.toml")).unwrap();
    let cfg: toml::Value = toml::from_str(&cfg).unwrap();
    assert_eq!(cfg["models"]["concierge"]["name"].as_str(), Some("qwen2.5"));
    assert_eq!(
        cfg["models"]["concierge"]["provider"].as_str(),
        Some("ollama")
    );
}

#[test]
fn model_rm_rejects_the_concierge_role() {
    let dir = TempDir::new().unwrap();
    let out = mem(dir.path(), &["model", "rm", "concierge"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("can't be removed"),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn init_creates_the_working_dir_and_writes_config() {
    let dir = TempDir::new().unwrap();
    // answers: project dir, configure concierge? y, provider, host, name,
    // pull model? n, add worker? n
    let input = "myproject\ny\nollama\nhttp://localhost:11434\nqwen2.5\nn\nn\n";
    let out = mem_stdin(dir.path(), &["init"], input);
    assert_success(&out);

    assert!(
        dir.path().join("myproject").is_dir(),
        "init must create the chosen working directory"
    );
    let cfg = std::fs::read_to_string(dir.path().join("myproject/.concierge/config.toml")).unwrap();
    let cfg: toml::Value = toml::from_str(&cfg).unwrap();
    assert_eq!(cfg["store"]["root"].as_str(), Some(".concierge"));
    assert_eq!(cfg["models"]["concierge"]["name"].as_str(), Some("qwen2.5"));
}

#[test]
fn ingest_stores_a_directory_as_a_verifiable_dag() {
    let dir = TempDir::new().unwrap();
    write_quiet_config(dir.path());

    let data_root = dir.path().join("data");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("f1.txt"), "v1").unwrap();

    // Ingest the data directory
    let ingest = mem(dir.path(), &["ingest", "data", "--name", "my-data"]);
    assert_success(&ingest);
    let root_cid = output_field(&ingest, "root_manifest");
    let ingest_run_cid = output_field(&ingest, "ingest_run");
    assert_eq!(output_field(&ingest, "file_count"), "1");
    assert_eq!(output_field(&ingest, "byte_count"), "2");
    assert_eq!(output_field(&ingest, "ignored_count"), "0");
    assert_eq!(output_field(&ingest, "plugin_records"), "0");
    assert_eq!(output_field(&ingest, "plugin_failures"), "0");

    // Verify we can resolve the name
    let resolve = mem(dir.path(), &["resolve", "my-data"]);
    assert_success(&resolve);
    assert_eq!(String::from_utf8(resolve.stdout).unwrap().trim(), root_cid);

    // Verify we can 'cat' the manifest
    let cat = mem(dir.path(), &["cat", "my-data"]);
    assert_success(&cat);
    let manifest: serde_json::Value = serde_json::from_slice(&cat.stdout).unwrap();
    assert_eq!(manifest["body"]["type"], "directory_manifest");
    assert_eq!(manifest["body"]["entries"][0]["path"], "f1.txt");

    let cat_run = mem(dir.path(), &["cat", &ingest_run_cid]);
    assert_success(&cat_run);
    let run: serde_json::Value = serde_json::from_slice(&cat_run.stdout).unwrap();
    assert_eq!(run["body"]["type"], "ingest_run");
    assert_eq!(json_cid_to_string(&run["body"]["manifest"]), root_cid);
    assert_eq!(run["body"]["file_count"].as_u64(), Some(1));
    assert_eq!(run["body"]["byte_count"].as_u64(), Some(2));

    // Verify we can 'cat' the file ref
    let file_ref_cid = json_cid_to_string(&manifest["body"]["entries"][0]["file_ref"]);
    let cat_ref = mem(dir.path(), &["cat", &file_ref_cid]);
    assert_success(&cat_ref);
    let file_ref: serde_json::Value = serde_json::from_slice(&cat_ref.stdout).unwrap();
    assert_eq!(file_ref["body"]["type"], "file_ref");
    assert_eq!(file_ref["body"]["size"].as_u64(), Some(2));
    assert!(file_ref["body"]["media_type"].is_null());
    assert!(file_ref["body"]["mtime"].is_u64());

    // Verify we can 'cat' the blob content
    let blob_cid = json_cid_to_string(&file_ref["body"]["content"]);
    let cat_blob = mem(dir.path(), &["cat", &blob_cid]);
    assert_success(&cat_blob);
    let blob: serde_json::Value = serde_json::from_slice(&cat_blob.stdout).unwrap();
    assert_eq!(blob["body"]["type"], "blob");
    // decode base64 bytes if needed, but for simple string it might be literal or array
    // serde_bytes serializes as a byte array or base64 depending on configuration
    // In our case it's likely a list of integers in JSON.
    assert_eq!(blob["body"]["bytes"], serde_json::json!([118, 49])); // "v1"
}

#[test]
fn ingest_single_file_keeps_the_file_name() {
    let dir = TempDir::new().unwrap();
    write_quiet_config(dir.path());
    std::fs::write(dir.path().join("one.txt"), "hi").unwrap();

    let ingest = mem(dir.path(), &["ingest", "one.txt"]);
    assert_success(&ingest);
    let root_cid = output_field(&ingest, "root_manifest");
    assert_eq!(output_field(&ingest, "file_count"), "1");
    assert_eq!(output_field(&ingest, "plugin_records"), "0");
    assert_eq!(output_field(&ingest, "plugin_failures"), "0");

    let cat = mem(dir.path(), &["cat", &root_cid]);
    assert_success(&cat);
    let manifest: serde_json::Value = serde_json::from_slice(&cat.stdout).unwrap();
    assert_eq!(manifest["body"]["type"], "directory_manifest");
    assert_eq!(manifest["body"]["entries"][0]["path"], "one.txt");
}

#[test]
fn ingest_source_index_plugin_writes_symbol_records() {
    let dir = TempDir::new().unwrap();
    write_quiet_config(dir.path());

    let data_root = dir.path().join("data");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(
        data_root.join("auth.py"),
        "def authenticate(token):\n    return verify(token)\n\nclass Authenticator:\n    def login(self, user):\n        return user.token\n",
    )
    .unwrap();
    std::fs::write(
        data_root.join("view.jsx"),
        "function Shell() { return <div>ok</div>; }\nclass Panel { render() { return <span />; } }\n",
    )
    .unwrap();
    std::fs::write(
        data_root.join("view.tsx"),
        "interface Widget { render(): string; }\nfunction mount(): void {}\nclass Screen { open(): JSX.Element { return <section />; } }\n",
    )
    .unwrap();
    std::fs::write(data_root.join("no_symbols.py"), "x = 1\n").unwrap();
    std::fs::write(data_root.join("broken.py"), "def broken(:\n    return 2\n").unwrap();

    let ingest = mem(dir.path(), &["ingest", "data", "--plugin", "source-index"]);
    assert_success(&ingest);
    assert_eq!(output_field(&ingest, "file_count"), "5");
    assert_eq!(output_field(&ingest, "plugin_records"), "10");
    assert_eq!(output_field(&ingest, "plugin_failures"), "1");
    let file_stats = plugin_file_stats(&ingest);
    assert_eq!(file_stats["auth.py"], (3, 0));
    assert_eq!(file_stats["view.jsx"], (3, 0));
    assert_eq!(file_stats["view.tsx"], (4, 0));
    assert_eq!(file_stats["no_symbols.py"], (0, 0));
    assert_eq!(file_stats["broken.py"], (0, 1));

    let root_cid = output_field(&ingest, "root_manifest");
    let ingest_run_cid = output_field(&ingest, "ingest_run");
    let cat = mem(dir.path(), &["cat", &root_cid]);
    assert_success(&cat);
    let manifest: serde_json::Value = serde_json::from_slice(&cat.stdout).unwrap();
    let symbol_cids: Vec<String> = manifest["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|edge| edge["rel"] == "contains")
        .map(|edge| json_cid_to_string(&edge["to"]))
        .collect();
    assert_eq!(symbol_cids.len(), 10);

    let mut languages = BTreeSet::new();
    for symbol_cid in symbol_cids {
        let cat_symbol = mem(dir.path(), &["cat", &symbol_cid]);
        assert_success(&cat_symbol);
        let symbol: serde_json::Value = serde_json::from_slice(&cat_symbol.stdout).unwrap();
        assert_eq!(symbol["body"]["type"], "symbol");
        assert_eq!(symbol["source"]["kind"], "derived");
        languages.insert(symbol["body"]["language"].as_str().unwrap().to_string());
        assert!(
            symbol["edges"]
                .as_array()
                .unwrap()
                .iter()
                .any(|edge| edge["rel"] == "derived_from")
        );
    }
    assert_eq!(
        languages,
        BTreeSet::from([
            "javascript".to_string(),
            "python".to_string(),
            "typescript".to_string()
        ])
    );

    let cat_run = mem(dir.path(), &["cat", &ingest_run_cid]);
    assert_success(&cat_run);
    let run: serde_json::Value = serde_json::from_slice(&cat_run.stdout).unwrap();
    assert_eq!(run["body"]["per_file_plugin_records"]["auth.py"], 3);
    assert_eq!(run["body"]["per_file_plugin_records"]["no_symbols.py"], 0);
    assert_eq!(run["body"]["per_file_plugin_records"]["broken.py"], 0);
    assert_eq!(run["body"]["per_file_plugin_failures"]["auth.py"], 0);
    assert_eq!(run["body"]["per_file_plugin_failures"]["no_symbols.py"], 0);
    assert_eq!(run["body"]["per_file_plugin_failures"]["broken.py"], 1);
}

#[test]
fn ingest_unknown_plugin_errors_clearly() {
    let dir = TempDir::new().unwrap();
    write_quiet_config(dir.path());

    let data_root = dir.path().join("data");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("f1.txt"), "v1").unwrap();

    let ingest = mem(dir.path(), &["ingest", "data", "--plugin", "missing"]);
    assert!(
        !ingest.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&ingest.stdout),
        String::from_utf8_lossy(&ingest.stderr)
    );
    assert!(
        String::from_utf8_lossy(&ingest.stderr)
            .contains("ingest plugin \"missing\" is not available"),
        "stderr:\n{}",
        String::from_utf8_lossy(&ingest.stderr)
    );
}

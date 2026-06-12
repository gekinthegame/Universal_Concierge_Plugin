//! Layer 3 - Hermes harness adapter.
//!
//! This crate is the Hermes-specific bridge for Phase 6. It translates Hermes
//! ACP-style session/tool/file callbacks into the host-neutral Concierge
//! [`Envelope`] stream, then reuses the Phase 2 JSONL ingest path to write the
//! actual IPLD graph.

use std::io::BufRead;
use std::path::Path;

use concierge_adapter_jsonl::{ingest_envelopes, IngestReport};
use concierge_core::{
    cid_from_link, Cid, CidOrName, CoreBinding, Envelope, Event, ImportedFrom, Reasoning, Record,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Hermes-side event kinds that we know how to translate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HermesEvent {
    SessionStarted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    SessionEnded,
    UserPrompt {
        text: String,
    },
    ModelResponse {
        text: String,
    },
    ToolCallStarted {
        tool: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args_json: Option<String>,
    },
    ToolCallFinished {
        tool: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_json: Option<String>,
    },
    FileRead {
        path: String,
    },
    FileWritten {
        path: String,
    },
    DecisionRecorded {
        text: String,
    },
    MemoryRecorded {
        text: String,
    },
    CheckpointRequested {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

/// A Hermes event with the routing envelope that lets us preserve host / session scope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HermesEnvelope {
    pub host_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    pub ts: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_from: Option<ImportedFrom>,
    /// Forwarded as-is to the core envelope: the model's reasoning behind this
    /// step, when Hermes exposes it (absent otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    #[serde(flatten)]
    pub event: HermesEvent,
}

/// Explicit include / exclude policy for Hermes file touches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTouchPolicy {
    includes: Vec<String>,
    excludes: Vec<String>,
}

impl Default for FileTouchPolicy {
    fn default() -> Self {
        Self {
            includes: Vec::new(),
            excludes: vec![
                ".git/".to_string(),
                ".concierge/".to_string(),
                "target/".to_string(),
                "node_modules/".to_string(),
                "dist/".to_string(),
                "build/".to_string(),
            ],
        }
    }
}

impl FileTouchPolicy {
    pub fn new(includes: Vec<String>, excludes: Vec<String>) -> Self {
        Self { includes, excludes }
    }

    pub fn allows(&self, path: &str) -> bool {
        let normalized = normalize_path(path);
        if normalized.is_empty() {
            return false;
        }
        if self
            .includes
            .iter()
            .any(|prefix| normalized.starts_with(&normalize_path(prefix)))
        {
            return true;
        }
        if normalized
            .split('/')
            .any(|segment| segment.starts_with('.') && segment != "." && segment != "..")
        {
            return false;
        }
        !self
            .excludes
            .iter()
            .any(|prefix| normalized.starts_with(&normalize_path(prefix)))
    }
}

/// Result of ingesting a Hermes session.
#[derive(Debug, Default)]
pub struct HermesIngestReport {
    pub translated_events: usize,
    pub skipped_touches: Vec<String>,
    pub pre_session_checkpoint: Option<Cid>,
    pub post_session_checkpoint: Option<Cid>,
    pub inner: IngestReport,
}

/// Phase 6 adapter.
#[derive(Debug, Clone, Default)]
pub struct HermesAdapter {
    file_policy: FileTouchPolicy,
}

impl HermesAdapter {
    pub fn new(file_policy: FileTouchPolicy) -> Self {
        Self { file_policy }
    }

    /// Translate Hermes envelopes to Concierge envelopes. Session boundaries are preserved
    /// in the output stream; the ingest path decides how to checkpoint them.
    pub fn translate(&self, events: &[HermesEnvelope]) -> Vec<Envelope> {
        events
            .iter()
            .filter_map(|env| self.translate_one(env))
            .collect()
    }

    /// Ingest a Hermes session into Concierge memory.
    ///
    /// This uses the Phase 2 JSONL ingest path for the actual writes, but frames the
    /// session with explicit checkpoints before and after the translated stream so the
    /// phase 6 checkpointing requirement is handled by this adapter.
    pub fn ingest_session<B: CoreBinding>(
        &self,
        events: &[HermesEnvelope],
        binding: &B,
        base_dir: &Path,
    ) -> Result<HermesIngestReport, String> {
        let mut report = HermesIngestReport::default();
        let Some(first) = events.first() else {
            return Ok(report);
        };

        let pre_session_checkpoint =
            self.checkpoint_existing_latest(binding, first, "session-start")?;
        report.pre_session_checkpoint = pre_session_checkpoint.clone();

        let translated = self.translate(events);
        report.translated_events = translated.len();
        let translated = translated
            .into_iter()
            .filter(|env| !matches!(env.event, Event::SessionEnded))
            .collect::<Vec<_>>();
        let items = translated
            .into_iter()
            .enumerate()
            .map(|(idx, env)| (idx + 1, env));
        report.inner = ingest_envelopes(items, binding, base_dir, IngestReport::default());

        let post_session_checkpoint =
            self.checkpoint_session_end(binding, first, pre_session_checkpoint.as_ref())?;
        report.post_session_checkpoint = post_session_checkpoint;
        Ok(report)
    }

    /// Read Hermes events from a JSONL stream, then ingest them.
    pub fn ingest_jsonl<R: BufRead, B: CoreBinding>(
        &self,
        reader: R,
        binding: &B,
        base_dir: &Path,
    ) -> Result<HermesIngestReport, String> {
        let mut events = Vec::new();
        for (line_no, line) in reader.lines().enumerate() {
            let line_no = line_no + 1;
            let line = line.map_err(|e| format!("read error at line {line_no}: {e}"))?;
            if line.trim().is_empty() {
                continue;
            }
            let env: HermesEnvelope = serde_json::from_str(&line)
                .map_err(|e| format!("invalid Hermes JSON at line {line_no}: {e}"))?;
            events.push(env);
        }
        self.ingest_session(&events, binding, base_dir)
    }

    fn translate_one(&self, env: &HermesEnvelope) -> Option<Envelope> {
        let event = match &env.event {
            HermesEvent::SessionStarted { cwd } => Event::SessionStarted { cwd: cwd.clone() },
            HermesEvent::SessionEnded => Event::SessionEnded,
            HermesEvent::UserPrompt { text } => Event::UserPrompt { text: text.clone() },
            HermesEvent::ModelResponse { text } => Event::ModelResponse { text: text.clone() },
            HermesEvent::ToolCallStarted { tool, args_json } => Event::ToolCallStarted {
                tool: tool.clone(),
                args_json: args_json.clone(),
            },
            HermesEvent::ToolCallFinished {
                tool,
                ok,
                result_json,
            } => Event::ToolCallFinished {
                tool: tool.clone(),
                ok: *ok,
                result_json: result_json.clone(),
            },
            HermesEvent::FileRead { path } => {
                if !self.file_policy.allows(path) {
                    return None;
                }
                Event::FileRead { path: path.clone() }
            }
            HermesEvent::FileWritten { path } => {
                if !self.file_policy.allows(path) {
                    return None;
                }
                Event::FileWritten { path: path.clone() }
            }
            HermesEvent::DecisionRecorded { text } => {
                Event::DecisionRecorded { text: text.clone() }
            }
            HermesEvent::MemoryRecorded { text } => Event::MemoryRecorded { text: text.clone() },
            HermesEvent::CheckpointRequested { label } => Event::CheckpointRequested {
                label: label.clone(),
            },
        };

        Some(Envelope {
            host_id: env.host_id.clone(),
            session_id: env.session_id.clone(),
            project_id: env.project_id.clone(),
            event_id: env.event_id.clone(),
            ts: env.ts.clone(),
            imported_from: env.imported_from.clone(),
            reasoning: env.reasoning.clone(),
            event,
        })
    }

    fn checkpoint_existing_latest<B: CoreBinding>(
        &self,
        binding: &B,
        env: &HermesEnvelope,
        label: &str,
    ) -> Result<Option<Cid>, String> {
        let current = match self.resolve_latest(binding, env) {
            Some(cid) => cid,
            None => return Ok(None),
        };
        let record = binding
            .get(&CidOrName::Cid(current.clone()))
            .map_err(|e| e.to_string())?;
        let Record::Live {
            kind, body_json, ..
        } = record
        else {
            return Ok(None);
        };
        if kind != "checkpoint" {
            return Ok(None);
        }
        let root = checkpoint_root_from_body(&body_json)?;
        let checkpoint = binding
            .checkpoint(label, &root, Some(&current))
            .map_err(|e| e.to_string())?;
        self.bind_scoped_names(binding, env, label, &checkpoint)?;
        Ok(Some(checkpoint))
    }

    fn checkpoint_session_end<B: CoreBinding>(
        &self,
        binding: &B,
        env: &HermesEnvelope,
        parent: Option<&Cid>,
    ) -> Result<Option<Cid>, String> {
        let latest_name = self.session_latest_name(env);
        let head = match binding.resolve(&latest_name) {
            Ok(cid) => cid,
            Err(_) => return Ok(None),
        };
        let checkpoint = binding
            .checkpoint("session-ended", &head, parent)
            .map_err(|e| e.to_string())?;
        self.bind_scoped_names(binding, env, "session-ended", &checkpoint)?;
        Ok(Some(checkpoint))
    }

    fn resolve_latest<B: CoreBinding>(&self, binding: &B, env: &HermesEnvelope) -> Option<Cid> {
        for name in [
            self.session_latest_name(env),
            self.host_latest_name(env),
            self.global_latest_name(),
        ] {
            if let Ok(cid) = binding.resolve(&name) {
                return Some(cid);
            }
        }
        None
    }

    fn bind_scoped_names<B: CoreBinding>(
        &self,
        binding: &B,
        env: &HermesEnvelope,
        label: &str,
        cid: &Cid,
    ) -> Result<(), String> {
        for name in self.scoped_names(env, label) {
            binding.bind(&name, cid).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn scoped_names(&self, env: &HermesEnvelope, label: &str) -> Vec<String> {
        let mut names = vec![
            self.session_checkpoint_name(env, label),
            self.session_latest_name(env),
            self.host_latest_name(env),
        ];
        if let Some(project) = &env.project_id {
            names.push(format!("project:{project}:latest"));
        }
        names.push(self.global_latest_name());
        names
    }

    fn session_checkpoint_name(&self, env: &HermesEnvelope, label: &str) -> String {
        format!(
            "host:{}:session:{}:checkpoint:{}",
            env.host_id, env.session_id, label
        )
    }

    fn session_latest_name(&self, env: &HermesEnvelope) -> String {
        format!("host:{}:session:{}:latest", env.host_id, env.session_id)
    }

    fn host_latest_name(&self, env: &HermesEnvelope) -> String {
        format!("host:{}:latest", env.host_id)
    }

    fn global_latest_name(&self) -> String {
        "latest".to_string()
    }
}

fn checkpoint_root_from_body(body_json: &str) -> Result<Cid, String> {
    let value: serde_json::Value = serde_json::from_str(body_json)
        .map_err(|e| format!("checkpoint record JSON parse failed: {e}"))?;
    let body = value
        .get("body")
        .ok_or_else(|| "checkpoint record missing body".to_string())?;
    let root = body
        .get("root")
        .ok_or_else(|| "checkpoint record missing root".to_string())?;
    cid_from_link(root).map_err(|e| e.to_string())
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
        .trim()
        .trim_start_matches("./")
        .to_string()
}

/// Deserialize a Hermes JSONL stream and return the raw envelope records.
pub fn read_jsonl<R: BufRead>(reader: R) -> Result<Vec<HermesEnvelope>, String> {
    let mut out = Vec::new();
    for (line_no, line) in reader.lines().enumerate() {
        let line_no = line_no + 1;
        let line = line.map_err(|e| format!("read error at line {line_no}: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let env: HermesEnvelope = serde_json::from_str(&line)
            .map_err(|e| format!("invalid Hermes JSON at line {line_no}: {e}"))?;
        out.push(env);
    }
    Ok(out)
}

/// Convert a Hermes session into JSONL lines of Concierge envelopes.
pub fn to_jsonl(events: &[HermesEnvelope]) -> Result<String, String> {
    let adapter = HermesAdapter::default();
    let envelopes = adapter.translate(events);
    let mut out = String::new();
    for env in envelopes {
        let line = serde_json::to_string(&env).map_err(|e| e.to_string())?;
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

/// Helper for tests and CLI: ingest a Hermes JSONL stream into Concierge memory.
pub fn ingest<R: BufRead, B: CoreBinding>(
    reader: R,
    binding: &B,
    base_dir: &Path,
) -> Result<HermesIngestReport, String> {
    HermesAdapter::default().ingest_jsonl(reader, binding, base_dir)
}

/// A stable content hash that helps tests and potential future replay fixtures.
pub fn session_fingerprint(events: &[HermesEnvelope]) -> String {
    let mut hasher = Sha256::new();
    for env in events {
        hasher.update(
            serde_json::to_string(env)
                .unwrap_or_else(|_| format!("{}:{}:{}", env.host_id, env.session_id, env.ts)),
        );
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use concierge_core::{CidOrName, MemCli, Node, Record};
    use std::io::BufReader;

    fn store() -> (tempfile::TempDir, MemCli) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mem = MemCli::new(dir.path());
        (dir, mem)
    }

    fn seed_latest(mem: &MemCli, label: &str) -> Cid {
        let node = Node {
            kind: "prompt".to_string(),
            fields_json: serde_json::json!({ "text": label }).to_string(),
        };
        let cid = mem.put_node(&node).expect("seed node");
        let checkpoint = mem.checkpoint("seed", &cid, None).expect("seed checkpoint");
        mem.bind("latest", &checkpoint).expect("bind latest");
        checkpoint
    }

    #[test]
    fn translates_session_tool_file_and_boundary_events() {
        let events = vec![
            HermesEnvelope {
                host_id: "hermes-01".to_string(),
                session_id: "session-1".to_string(),
                project_id: Some("alpha".to_string()),
                event_id: Some("evt-1".to_string()),
                ts: "2026-06-02T00:00:00Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: HermesEvent::SessionStarted {
                    cwd: Some("/work/app".to_string()),
                },
            },
            HermesEnvelope {
                host_id: "hermes-01".to_string(),
                session_id: "session-1".to_string(),
                project_id: Some("alpha".to_string()),
                event_id: Some("evt-2".to_string()),
                ts: "2026-06-02T00:00:01Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: HermesEvent::UserPrompt {
                    text: "please update docs".to_string(),
                },
            },
            HermesEnvelope {
                host_id: "hermes-01".to_string(),
                session_id: "session-1".to_string(),
                project_id: Some("alpha".to_string()),
                event_id: Some("evt-3".to_string()),
                ts: "2026-06-02T00:00:02Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: HermesEvent::ToolCallStarted {
                    tool: "write_file".to_string(),
                    args_json: Some(r#"{"path":"notes.md"}"#.to_string()),
                },
            },
            HermesEnvelope {
                host_id: "hermes-01".to_string(),
                session_id: "session-1".to_string(),
                project_id: Some("alpha".to_string()),
                event_id: Some("evt-4".to_string()),
                ts: "2026-06-02T00:00:03Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: HermesEvent::ToolCallFinished {
                    tool: "write_file".to_string(),
                    ok: true,
                    result_json: Some(r#"{"bytes":42}"#.to_string()),
                },
            },
            HermesEnvelope {
                host_id: "hermes-01".to_string(),
                session_id: "session-1".to_string(),
                project_id: Some("alpha".to_string()),
                event_id: Some("evt-5".to_string()),
                ts: "2026-06-02T00:00:04Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: HermesEvent::FileRead {
                    path: "docs/readme.md".to_string(),
                },
            },
            HermesEnvelope {
                host_id: "hermes-01".to_string(),
                session_id: "session-1".to_string(),
                project_id: Some("alpha".to_string()),
                event_id: Some("evt-6".to_string()),
                ts: "2026-06-02T00:00:05Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: HermesEvent::FileWritten {
                    path: ".cache/generated.txt".to_string(),
                },
            },
            HermesEnvelope {
                host_id: "hermes-01".to_string(),
                session_id: "session-1".to_string(),
                project_id: Some("alpha".to_string()),
                event_id: Some("evt-7".to_string()),
                ts: "2026-06-02T00:00:06Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: HermesEvent::DecisionRecorded {
                    text: "stay with the current plan".to_string(),
                },
            },
            HermesEnvelope {
                host_id: "hermes-01".to_string(),
                session_id: "session-1".to_string(),
                project_id: Some("alpha".to_string()),
                event_id: Some("evt-8".to_string()),
                ts: "2026-06-02T00:00:07Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: HermesEvent::SessionEnded,
            },
        ];

        let adapter = HermesAdapter::default();
        let envelopes = adapter.translate(&events);
        let kinds: Vec<_> = envelopes.iter().map(|env| env.event.clone()).collect();
        assert!(matches!(kinds[0], Event::SessionStarted { .. }));
        assert!(matches!(kinds[1], Event::UserPrompt { .. }));
        assert!(matches!(kinds[2], Event::ToolCallStarted { .. }));
        assert!(matches!(kinds[3], Event::ToolCallFinished { .. }));
        assert!(matches!(kinds[4], Event::FileRead { .. }));
        assert!(matches!(kinds[5], Event::DecisionRecorded { .. }));
        assert!(matches!(kinds[6], Event::SessionEnded));
        assert_eq!(envelopes.len(), 7, "hidden file touches are excluded");
    }

    #[test]
    fn ingest_session_writes_pre_and_post_checkpoints_and_skips_hidden_files() {
        let (dir, mem) = store();
        let base = dir.path().join("notes");
        std::fs::create_dir_all(&base).expect("notes dir");
        std::fs::write(base.join("readme.md"), b"hello from hermes").expect("write visible");
        std::fs::create_dir_all(dir.path().join(".cache")).expect("cache dir");
        std::fs::write(dir.path().join(".cache/generated.txt"), b"ignore me")
            .expect("write hidden");

        let previous = seed_latest(&mem, "prior checkpoint");

        let events = vec![
            HermesEnvelope {
                host_id: "hermes-01".to_string(),
                session_id: "session-2".to_string(),
                project_id: Some("alpha".to_string()),
                event_id: Some("evt-1".to_string()),
                ts: "2026-06-03T00:00:00Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: HermesEvent::SessionStarted {
                    cwd: Some(dir.path().display().to_string()),
                },
            },
            HermesEnvelope {
                host_id: "hermes-01".to_string(),
                session_id: "session-2".to_string(),
                project_id: Some("alpha".to_string()),
                event_id: Some("evt-2".to_string()),
                ts: "2026-06-03T00:00:01Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: HermesEvent::UserPrompt {
                    text: "update the docs".to_string(),
                },
            },
            HermesEnvelope {
                host_id: "hermes-01".to_string(),
                session_id: "session-2".to_string(),
                project_id: Some("alpha".to_string()),
                event_id: Some("evt-3".to_string()),
                ts: "2026-06-03T00:00:02Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: HermesEvent::ToolCallStarted {
                    tool: "read_file".to_string(),
                    args_json: Some(r#"{"path":"notes/readme.md"}"#.to_string()),
                },
            },
            HermesEnvelope {
                host_id: "hermes-01".to_string(),
                session_id: "session-2".to_string(),
                project_id: Some("alpha".to_string()),
                event_id: Some("evt-4".to_string()),
                ts: "2026-06-03T00:00:03Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: HermesEvent::ToolCallFinished {
                    tool: "read_file".to_string(),
                    ok: true,
                    result_json: Some(r#"{"status":"ok"}"#.to_string()),
                },
            },
            HermesEnvelope {
                host_id: "hermes-01".to_string(),
                session_id: "session-2".to_string(),
                project_id: Some("alpha".to_string()),
                event_id: Some("evt-5".to_string()),
                ts: "2026-06-03T00:00:04Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: HermesEvent::FileRead {
                    path: "notes/readme.md".to_string(),
                },
            },
            HermesEnvelope {
                host_id: "hermes-01".to_string(),
                session_id: "session-2".to_string(),
                project_id: Some("alpha".to_string()),
                event_id: Some("evt-6".to_string()),
                ts: "2026-06-03T00:00:05Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: HermesEvent::FileWritten {
                    path: ".cache/generated.txt".to_string(),
                },
            },
            HermesEnvelope {
                host_id: "hermes-01".to_string(),
                session_id: "session-2".to_string(),
                project_id: Some("alpha".to_string()),
                event_id: Some("evt-7".to_string()),
                ts: "2026-06-03T00:00:06Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: HermesEvent::MemoryRecorded {
                    text: "persistent note".to_string(),
                },
            },
            HermesEnvelope {
                host_id: "hermes-01".to_string(),
                session_id: "session-2".to_string(),
                project_id: Some("alpha".to_string()),
                event_id: Some("evt-8".to_string()),
                ts: "2026-06-03T00:00:07Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: HermesEvent::CheckpointRequested {
                    label: Some("mid-session".to_string()),
                },
            },
            HermesEnvelope {
                host_id: "hermes-01".to_string(),
                session_id: "session-2".to_string(),
                project_id: Some("alpha".to_string()),
                event_id: Some("evt-9".to_string()),
                ts: "2026-06-03T00:00:08Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: HermesEvent::SessionEnded,
            },
        ];

        let adapter = HermesAdapter::default();
        let report = adapter
            .ingest_session(&events, &mem, dir.path())
            .expect("ingest session");

        assert_eq!(report.translated_events, 8, "hidden file touch is filtered");
        assert_eq!(
            report.inner.nodes_written, 4,
            "prompt + tool result + file ref + memory"
        );
        assert_eq!(
            report.inner.checkpoints, 1,
            "the explicit mid-session checkpoint lands"
        );
        assert!(
            report.pre_session_checkpoint.is_some(),
            "existing latest state should get a before-session checkpoint"
        );
        assert!(
            report.post_session_checkpoint.is_some(),
            "the end of the session should checkpoint the session head"
        );

        let book = mem.social_book().expect("book");
        assert!(
            book.following.is_empty(),
            "Hermes adapter should not mutate social state"
        );
        let latest = mem
            .resolve("host:hermes-01:session:session-2:latest")
            .expect("latest");
        match mem.get(&CidOrName::Cid(latest)).expect("latest record") {
            Record::Live { kind, .. } => assert_eq!(kind, "checkpoint"),
            other => panic!("expected latest checkpoint, got {other:?}"),
        }

        let pre = report.pre_session_checkpoint.expect("pre");
        let pre_record = mem.get(&CidOrName::Cid(pre.clone())).expect("pre record");
        let Record::Live { body_json, .. } = pre_record else {
            panic!("expected pre-session checkpoint to be live");
        };
        let body: serde_json::Value = serde_json::from_str(&body_json).expect("pre body json");
        let parent = body
            .get("body")
            .and_then(|b| b.get("parent"))
            .expect("checkpoint parent cid");
        let parent_cid = cid_from_link(parent).expect("parent cid");
        assert_eq!(
            parent_cid, previous,
            "pre-session checkpoint should chain from prior latest"
        );
    }

    #[test]
    fn file_policy_respects_excludes_and_explicit_includes() {
        let policy = FileTouchPolicy::new(vec![".cache/generated.txt".to_string()], vec![]);
        assert!(policy.allows(".cache/generated.txt"));

        let policy = FileTouchPolicy::default();
        assert!(!policy.allows(".cache/generated.txt"));
        assert!(policy.allows("docs/readme.md"));
    }

    #[test]
    fn jsonl_roundtrip_parses_fixture() {
        let fixture = include_str!("../tests/fixtures/canned_hermes.jsonl");
        let events = read_jsonl(BufReader::new(fixture.as_bytes())).expect("read jsonl");
        assert_eq!(events.len(), 3);
        assert_eq!(session_fingerprint(&events).len(), 64);
    }

    #[test]
    fn translated_jsonl_is_valid_concierge_json() {
        let events = vec![HermesEnvelope {
            host_id: "hermes-01".to_string(),
            session_id: "s".to_string(),
            project_id: None,
            event_id: None,
            ts: "2026-06-03T00:00:00Z".to_string(),
            imported_from: None,
            reasoning: None,
            event: HermesEvent::UserPrompt {
                text: "hello".to_string(),
            },
        }];
        let jsonl = to_jsonl(&events).expect("jsonl");
        let line: Envelope = serde_json::from_str(jsonl.trim()).expect("envelope");
        assert!(matches!(line.event, Event::UserPrompt { .. }));
    }
}

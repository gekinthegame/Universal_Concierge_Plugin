//! Programmatic session state for chat, recall, checkpoints, and whole-plan
//! worker handoff.

use crate::audit;
use crate::blockstore::LocalBlocks;
use crate::cid::Cid;
use crate::config::{CheckpointConfig, VerifierConfig};
use crate::model::Model;
use crate::node::{
    Checkpoint, Conversation, Edge, EdgeRel, Node, Plan, Prompt, Record, Response, Source,
    ToolResult,
};
use crate::store::{Store, SystemClock};
use crate::verifier;
use crate::work::{
    AlignmentPrompt, FileWrite, MAX_REPAIR_LAPS, WorkReport, WorkTrace, alignment_prompt_text,
    execute_project_tool, extract_file_body, file_prompt_text, manifest_prompt_text,
    parse_manifest, parse_tool_request, persist_file_node, project_file_map,
    project_file_map_summary, upsert_write, work_files_summary, write_workspace_file,
};
use std::fmt::Write as _;
use std::path::PathBuf;

#[derive(Clone)]
struct LoadedRecord {
    cid: Cid,
    record: Record,
}

/// What was restored when a checkpoint was resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumeInfo {
    pub checkpoint: Cid,
    pub root: Cid,
    pub turns: usize,
    pub loaded_context: usize,
}

/// A live chat session: a store, a model, and the growing conversation it
/// produces. Chatting *is* writing to memory.
pub struct Session<M: Model> {
    store: Store<LocalBlocks, SystemClock>,
    model: M,
    worker: Option<Box<dyn Model>>,
    _cleanup: Option<Box<dyn Model>>,
    workspace_root: PathBuf,
    convo_name: String,
    turns: Vec<Cid>,
    head: Option<Cid>,
    loaded: Vec<LoadedRecord>,
    checkpoint: CheckpointConfig,
    verifier: VerifierConfig,
    turns_since_checkpoint: u32,
    last_checkpoint: Option<Cid>,
}

impl<M: Model> Session<M> {
    pub fn new(
        store: Store<LocalBlocks, SystemClock>,
        model: M,
        convo_name: impl Into<String>,
    ) -> Self {
        Self {
            store,
            model,
            worker: None,
            _cleanup: None,
            workspace_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            convo_name: convo_name.into(),
            turns: Vec::new(),
            head: None,
            loaded: Vec::new(),
            // auto-checkpoint stays off until explicitly enabled (so tests and
            // library embeds aren't perturbed); the CLI turns it on from config.
            checkpoint: CheckpointConfig {
                auto: false,
                ..CheckpointConfig::default()
            },
            verifier: VerifierConfig {
                enabled: false,
                ..VerifierConfig::default()
            },
            turns_since_checkpoint: 0,
            last_checkpoint: None,
        }
    }

    /// Attach an optional worker role. `/work` passes the Concierge plan to
    /// this model whole; normal chat remains on the Concierge.
    pub fn with_worker(mut self, worker: Box<dyn Model>) -> Self {
        self.worker = Some(worker);
        self
    }

    /// Attach an optional cleanup role for existing configs. `/work` repair
    /// re-entry now aligns through the configured worker.
    pub fn with_cleanup(mut self, cleanup: Box<dyn Model>) -> Self {
        self._cleanup = Some(cleanup);
        self
    }

    /// Override where worker file writes land. The CLI uses the selected
    /// project directory; tests set this explicitly to avoid process cwd.
    pub fn with_workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace_root = root.into();
        self
    }

    /// Enable auto-checkpointing with the given policy. Write-side only — it
    /// snapshots state as the river flows; recall stays manual.
    pub fn with_checkpoints(mut self, policy: CheckpointConfig) -> Self {
        self.checkpoint = policy;
        self
    }

    /// Configure sandboxed real-tool verification for `/work`.
    pub fn with_verifier(mut self, verifier: VerifierConfig) -> Self {
        self.verifier = verifier;
        self
    }

    pub(crate) fn model_name(&self) -> &str {
        self.model.name()
    }

    pub(crate) fn worker_name(&self) -> Option<&str> {
        self.worker.as_ref().map(|worker| worker.name())
    }

    /// One exchange: record the prompt, record the model call as a ToolResult,
    /// record the response, extend the conversation, and rebind the head.
    pub fn turn(&mut self, user_input: &str) -> anyhow::Result<String> {
        self.turn_streaming(user_input, &mut |_| {})
    }

    /// Like `turn`, but streams the model's answer to `on_token` fragment by
    /// fragment for live display. The recorded `Response` text is unaffected —
    /// the callback is display-only. On success it emits a trailing newline to
    /// `on_token`, closing the streamed line before the post-model record trace.
    pub fn turn_streaming(
        &mut self,
        user_input: &str,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<String> {
        let model_name = self.model.name().to_string();
        let prompt_text = self.prompt_text(user_input)?;
        let context_edges = self
            .loaded
            .iter()
            .map(|loaded| Edge {
                rel: EdgeRel::UsedAsContext,
                to: loaded.cid,
            })
            .collect();

        let prompt_cid = self.store.put_node_with_edges(
            Node::Prompt(Prompt {
                text: prompt_text.clone(),
                model: Some(model_name.clone()),
            }),
            Source::User,
            context_edges,
        )?;

        let answer = match self.model.complete_streaming(&prompt_text, on_token) {
            Ok(answer) => answer,
            Err(err) => {
                let tool_cid = self.write_tool_result(
                    &model_name,
                    &prompt_text,
                    &err.to_string(),
                    false,
                    prompt_cid,
                )?;
                self.turns.push(prompt_cid);
                self.turns.push(tool_cid);
                self.write_conversation_head()?;
                self.maybe_checkpoint()?;
                return Err(err);
            }
        };
        // Close the streamed answer line before the post-model record trace.
        on_token("\n");

        let tool_cid =
            self.write_tool_result(&model_name, &prompt_text, &answer, true, prompt_cid)?;

        let response_cid = self.store.put_node_with_edges(
            Node::Response(Response {
                text: answer.clone(),
                model: model_name.clone(),
            }),
            Source::Model { name: model_name },
            vec![
                Edge {
                    rel: EdgeRel::DerivedFrom,
                    to: prompt_cid,
                },
                Edge {
                    rel: EdgeRel::DerivedFrom,
                    to: tool_cid,
                },
            ],
        )?;

        self.turns.push(prompt_cid);
        self.turns.push(tool_cid);
        self.turns.push(response_cid);
        self.write_conversation_head()?;
        self.maybe_checkpoint()?;

        Ok(answer)
    }

    /// Whole-plan role handoff: Concierge writes one complete plan, then the
    /// configured worker receives that plan verbatim and builds the project by
    /// calling workspace tools (write/read/list) in a loop until it is done.
    pub fn work(&mut self, job: &str) -> anyhow::Result<WorkReport> {
        self.work_with_trace(job, &mut |_| {})
    }

    pub(crate) fn work_with_trace(
        &mut self,
        job: &str,
        on_trace: &mut dyn FnMut(WorkTrace),
    ) -> anyhow::Result<WorkReport> {
        if self.worker.is_none() {
            anyhow::bail!(
                "no worker model configured; add [models.worker] in .concierge/config.toml"
            );
        }

        let concierge_name = self.model.name().to_string();
        let project_files = project_file_map(&self.workspace_root)?;
        on_trace(WorkTrace::stage(
            "Concierge",
            concierge_name.clone(),
            format!(
                "Planning whole-task handoff for `/work`.\nProject file map: {}",
                project_file_map_summary(&project_files)
            ),
        ));
        let plan_prompt_text = self.planning_prompt_text(job, &project_files)?;
        let context_edges = self.context_edges();
        let plan_prompt_cid = self.store.put_node_with_edges(
            Node::Prompt(Prompt {
                text: plan_prompt_text.clone(),
                model: Some(concierge_name.clone()),
            }),
            Source::User,
            context_edges,
        )?;

        let plan_text = match self.model.complete(&plan_prompt_text) {
            Ok(plan) => plan,
            Err(err) => {
                on_trace(WorkTrace::note(
                    "Concierge error",
                    format!("Planning failed: {err}"),
                ));
                let tool_cid = self.write_tool_result(
                    &concierge_name,
                    &plan_prompt_text,
                    &err.to_string(),
                    false,
                    plan_prompt_cid,
                )?;
                self.turns.push(plan_prompt_cid);
                self.turns.push(tool_cid);
                self.write_conversation_head()?;
                self.maybe_checkpoint()?;
                return Err(err);
            }
        };
        on_trace(WorkTrace::stage(
            "Concierge plan",
            concierge_name.clone(),
            plan_text.clone(),
        ));

        let plan_tool_cid = self.write_tool_result(
            &concierge_name,
            &plan_prompt_text,
            &plan_text,
            true,
            plan_prompt_cid,
        )?;
        let plan_cid = self.store.put_node_with_edges(
            Node::Plan(Plan {
                title: plan_title(job),
                prose: plan_text.clone(),
                spec: Some(plan_prompt_cid),
            }),
            Source::Model {
                name: concierge_name.clone(),
            },
            vec![
                Edge {
                    rel: EdgeRel::DerivedFrom,
                    to: plan_prompt_cid,
                },
                Edge {
                    rel: EdgeRel::DerivedFrom,
                    to: plan_tool_cid,
                },
            ],
        )?;

        let worker_name = self.worker.as_ref().unwrap().name().to_string();

        // --- Concierge: file manifest -------------------------------------
        // The Concierge turns its own plan into an explicit list of files. The
        // worker builds from this list and the plan, never from the raw user
        // prompt.
        let manifest_prompt = manifest_prompt_text(job, &plan_text);
        let manifest_prompt_cid = self.store.put_node_with_edges(
            Node::Prompt(Prompt {
                text: manifest_prompt.clone(),
                model: Some(concierge_name.clone()),
            }),
            Source::System,
            vec![Edge {
                rel: EdgeRel::DerivedFrom,
                to: plan_cid,
            }],
        )?;
        let manifest_text = match self.model.complete(&manifest_prompt) {
            Ok(text) => text,
            Err(err) => {
                on_trace(WorkTrace::note(
                    "Concierge error",
                    format!("Manifest generation failed: {err}"),
                ));
                let tool_cid = self.write_tool_result(
                    &concierge_name,
                    &manifest_prompt,
                    &err.to_string(),
                    false,
                    manifest_prompt_cid,
                )?;
                self.turns.extend([
                    plan_prompt_cid,
                    plan_tool_cid,
                    plan_cid,
                    manifest_prompt_cid,
                    tool_cid,
                ]);
                self.write_conversation_head()?;
                self.maybe_checkpoint()?;
                return Err(err);
            }
        };
        let manifest = parse_manifest(&manifest_text);
        let manifest_tool_cid = self.write_tool_result(
            &concierge_name,
            &manifest_prompt,
            &manifest_text,
            true,
            manifest_prompt_cid,
        )?;
        on_trace(WorkTrace::stage(
            "Concierge manifest",
            concierge_name.clone(),
            format!("{} file(s):\n{}", manifest.len(), manifest.join("\n")),
        ));
        if manifest.is_empty() {
            let failure_cid = self.store.put_node_with_edges(
                Node::ToolResult(ToolResult {
                    tool: "work.manifest".to_string(),
                    input: format!("manifest_tool_result:{manifest_tool_cid}"),
                    output: "concierge produced no file manifest".to_string(),
                    ok: false,
                }),
                Source::System,
                vec![Edge {
                    rel: EdgeRel::DerivedFrom,
                    to: manifest_tool_cid,
                }],
            )?;
            self.turns.extend([
                plan_prompt_cid,
                plan_tool_cid,
                plan_cid,
                manifest_prompt_cid,
                manifest_tool_cid,
                failure_cid,
            ]);
            self.write_conversation_head()?;
            self.maybe_checkpoint()?;
            anyhow::bail!("concierge produced no file manifest");
        }

        // --- Worker: one file per call ------------------------------------
        // First draft stays on the stable path: the worker gets the full
        // Concierge plan and manifest, then writes one file per call. Tools are
        // reserved for the repair side, where audit evidence gives them a
        // smaller blast radius.
        on_trace(WorkTrace::stage(
            "Worker",
            worker_name.clone(),
            format!(
                "Building {} file(s) one at a time from the manifest.",
                manifest.len()
            ),
        ));
        let worker_prompt_cid = self.store.put_node_with_edges(
            Node::Prompt(Prompt {
                text: format!(
                    "Per-file build over manifest ({} files):\n{}",
                    manifest.len(),
                    manifest.join("\n")
                ),
                model: Some(worker_name.clone()),
            }),
            Source::System,
            vec![Edge {
                rel: EdgeRel::PlannedAs,
                to: plan_cid,
            }],
        )?;

        let mut writes: Vec<FileWrite> = Vec::new();
        let mut transcript = String::new();
        for target in &manifest {
            let file_prompt = file_prompt_text(&plan_text, &manifest, &writes, target);
            let reply = match self.worker.as_ref().unwrap().complete(&file_prompt) {
                Ok(reply) => reply,
                Err(err) => {
                    on_trace(WorkTrace::note(
                        format!("Worker error: {target}"),
                        err.to_string(),
                    ));
                    let _ = writeln!(&mut transcript, "[{target}] ERROR: {err}");
                    continue;
                }
            };
            let body = extract_file_body(&reply);
            match write_workspace_file(&self.workspace_root, target, &body) {
                Ok(write) => {
                    on_trace(WorkTrace::note(
                        format!("Wrote {target}"),
                        format!("{} bytes", body.len()),
                    ));
                    let _ = writeln!(&mut transcript, "[{target}] {} bytes", body.len());
                    writes.push(write);
                }
                Err(err) => {
                    on_trace(WorkTrace::note(
                        format!("Skipped {target}"),
                        err.to_string(),
                    ));
                    let _ = writeln!(&mut transcript, "[{target}] SKIPPED: {err}");
                }
            }
        }

        let worker_result_cid = self.write_tool_result(
            &worker_name,
            &manifest.join("\n"),
            &transcript,
            true,
            worker_prompt_cid,
        )?;

        if writes.is_empty() {
            on_trace(WorkTrace::note(
                "Worker",
                "Worker wrote no files from the manifest.".to_string(),
            ));
            let failure_cid = self.store.put_node_with_edges(
                Node::ToolResult(ToolResult {
                    tool: "work.worker_loop".to_string(),
                    input: format!("worker_result:{worker_result_cid}"),
                    output: "worker wrote no files".to_string(),
                    ok: false,
                }),
                Source::System,
                vec![Edge {
                    rel: EdgeRel::DerivedFrom,
                    to: worker_result_cid,
                }],
            )?;
            self.turns.extend([
                plan_prompt_cid,
                plan_tool_cid,
                plan_cid,
                manifest_prompt_cid,
                manifest_tool_cid,
                worker_prompt_cid,
                worker_result_cid,
                failure_cid,
            ]);
            self.write_conversation_head()?;
            self.maybe_checkpoint()?;
            anyhow::bail!("worker wrote no files");
        }

        // --- Deterministic checks + alignment re-entry --------------------
        // Each lap: deterministic import-fix → audit → workhorse alignment
        // against the original plan. Whatever survives recirculates through
        // another scoped lap. Bounded by MAX_REPAIR_LAPS and a progress guard.
        let mut repair_turns: Vec<Cid> = Vec::new();
        if let Some(cid) =
            self.deterministic_import_fix(&mut writes, worker_result_cid, on_trace)?
        {
            repair_turns.push(cid);
        }
        let mut report = self.audit_and_verify(&writes, &project_files, on_trace)?;
        on_trace(WorkTrace::note(
            "Audit",
            if report.is_clean() {
                "No defects found.".to_string()
            } else {
                report.render()
            },
        ));

        let mut lap = 0;
        while !report.is_clean() && lap < MAX_REPAIR_LAPS {
            let before = report.finding_count();
            lap += 1;

            let (prompt_cid, tool_cid) = self.alignment_lap(
                &mut writes,
                &report,
                job,
                &plan_text,
                &manifest,
                worker_result_cid,
                lap,
                on_trace,
            )?;
            repair_turns.push(prompt_cid);
            repair_turns.push(tool_cid);

            if let Some(cid) =
                self.deterministic_import_fix(&mut writes, worker_result_cid, on_trace)?
            {
                repair_turns.push(cid);
            }

            report = self.audit_and_verify(&writes, &project_files, on_trace)?;
            on_trace(WorkTrace::note(
                format!("Audit (repair lap {lap})"),
                if report.is_clean() {
                    "Clean.".to_string()
                } else {
                    report.render()
                },
            ));

            if !report.is_clean() && report.finding_count() >= before {
                on_trace(WorkTrace::note(
                    "Repair",
                    format!("Lap {lap} made no progress; stopping re-entry."),
                ));
                break;
            }
        }

        if !report.is_clean() {
            let rendered = report.render();
            on_trace(WorkTrace::note(
                "Repair",
                format!("Re-entry ended with unresolved audit findings:\n{rendered}"),
            ));
            let failure_cid = self.store.put_node_with_edges(
                Node::ToolResult(ToolResult {
                    tool: "work.audit".to_string(),
                    input: format!("worker_result:{worker_result_cid}"),
                    output: rendered.clone(),
                    ok: false,
                }),
                Source::System,
                vec![Edge {
                    rel: EdgeRel::DerivedFrom,
                    to: worker_result_cid,
                }],
            )?;
            let mut turn_ids = vec![
                plan_prompt_cid,
                plan_tool_cid,
                plan_cid,
                manifest_prompt_cid,
                manifest_tool_cid,
                worker_prompt_cid,
                worker_result_cid,
            ];
            turn_ids.extend(repair_turns);
            turn_ids.push(failure_cid);
            self.turns.extend(turn_ids);
            self.write_conversation_head()?;
            self.maybe_checkpoint()?;
            anyhow::bail!("work audit has unresolved findings:\n{rendered}");
        }

        let files = writes
            .iter()
            .map(|write| persist_file_node(&self.store, write, worker_result_cid))
            .collect::<anyhow::Result<Vec<_>>>()?;
        on_trace(WorkTrace::note("File writes", work_files_summary(&files)));
        let mut response_edges = vec![
            Edge {
                rel: EdgeRel::DerivedFrom,
                to: worker_prompt_cid,
            },
            Edge {
                rel: EdgeRel::DerivedFrom,
                to: worker_result_cid,
            },
            Edge {
                rel: EdgeRel::PlannedAs,
                to: plan_cid,
            },
        ];
        response_edges.extend(files.iter().map(|file| Edge {
            rel: EdgeRel::Produced,
            to: file.cid,
        }));
        let worker_response_cid = self.store.put_node_with_edges(
            Node::Response(Response {
                text: transcript,
                model: worker_name,
            }),
            Source::Model {
                name: self
                    .worker
                    .as_ref()
                    .map(|worker| worker.name().to_string())
                    .unwrap_or_else(|| "worker".to_string()),
            },
            response_edges,
        )?;

        let mut turn_ids = vec![
            plan_prompt_cid,
            plan_tool_cid,
            plan_cid,
            manifest_prompt_cid,
            manifest_tool_cid,
            worker_prompt_cid,
            worker_result_cid,
        ];
        turn_ids.extend(repair_turns);
        turn_ids.push(worker_response_cid);
        self.turns.extend(turn_ids);
        self.write_conversation_head()?;
        self.maybe_checkpoint()?;

        Ok(WorkReport {
            plan: plan_cid,
            worker_response: worker_response_cid,
            files,
        })
    }

    fn audit_and_verify(
        &self,
        writes: &[FileWrite],
        project_files: &str,
        on_trace: &mut dyn FnMut(WorkTrace),
    ) -> anyhow::Result<audit::AuditReport> {
        let mut report = audit::review_with_project_files(writes, project_files);
        if !report.is_clean() {
            return Ok(report);
        }

        let verification = verifier::verify_project(&self.workspace_root, &self.verifier)?;
        if verification.passed() {
            on_trace(WorkTrace::note("Verification", verification.render()));
            return Ok(report);
        }
        if verification.failed() {
            on_trace(WorkTrace::note(
                "Verification failed",
                verification.render(),
            ));
            report
                .scaffold
                .extend(
                    verification
                        .findings(&self.workspace_root)
                        .into_iter()
                        .map(|finding| audit::ScaffoldFinding {
                            file: finding.file,
                            detail: finding.detail,
                        }),
                );
            return Ok(report);
        }

        on_trace(WorkTrace::note("Verification", verification.render()));
        Ok(report)
    }

    /// Deterministic import correction (no model): rewrite/split imports that
    /// don't resolve or pull symbols from the wrong file. Mutates `writes` and
    /// the on-disk files; records a `work.import_fix` node when anything changed.
    fn deterministic_import_fix(
        &self,
        writes: &mut Vec<FileWrite>,
        worker_result_cid: Cid,
        on_trace: &mut dyn FnMut(WorkTrace),
    ) -> anyhow::Result<Option<Cid>> {
        let (fixed, path_fixes) = audit::fix_import_paths(writes);
        if path_fixes.is_empty() {
            return Ok(None);
        }
        *writes = fixed;
        let mut log = String::new();
        let mut changed: Vec<String> = Vec::new();
        for fix in &path_fixes {
            on_trace(WorkTrace::note(
                format!("Fixed import in {}", fix.file),
                format!("'{}' → '{}'", fix.from, fix.to),
            ));
            let _ = writeln!(&mut log, "{}: '{}' → '{}'", fix.file, fix.from, fix.to);
            if !changed.contains(&fix.file) {
                changed.push(fix.file.clone());
            }
        }
        for path in &changed {
            if let Some(write) = writes.iter().find(|write| &write.path == path) {
                write_workspace_file(&self.workspace_root, &write.path, &write.content)?;
            }
        }
        let cid = self.store.put_node_with_edges(
            Node::ToolResult(ToolResult {
                tool: "work.import_fix".to_string(),
                input: format!("worker_result:{worker_result_cid}"),
                output: log,
                ok: true,
            }),
            Source::System,
            vec![Edge {
                rel: EdgeRel::DerivedFrom,
                to: worker_result_cid,
            }],
        )?;
        Ok(Some(cid))
    }

    /// One scoped alignment lap: the workhorse rewrites files directly against
    /// the original goal, original plan, original manifest, and current audit
    /// findings. Mutates `writes` and disk; records the lap's prompt and
    /// tool-result nodes.
    #[allow(clippy::too_many_arguments)]
    fn alignment_lap(
        &self,
        writes: &mut Vec<FileWrite>,
        report: &audit::AuditReport,
        job: &str,
        plan_text: &str,
        manifest: &[String],
        worker_result_cid: Cid,
        lap: usize,
        on_trace: &mut dyn FnMut(WorkTrace),
    ) -> anyhow::Result<(Cid, Cid)> {
        let worker = self.worker.as_ref().expect("worker model present");
        let worker_name = worker.name().to_string();
        let scope = report.alignment_files(writes);
        on_trace(WorkTrace::stage(
            "Workhorse alignment",
            worker_name.clone(),
            format!(
                "Repair lap {lap}: aligning {} scoped file(s) to the original plan.",
                scope.len()
            ),
        ));
        let full_audit = report.render();
        let mut alignment_log = String::new();
        for path in &scope {
            let idx = writes.iter().position(|write| &write.path == path);
            let current = idx
                .map(|idx| writes[idx].content.clone())
                .unwrap_or_else(|| {
                    "(file is missing; create it with complete contents)".to_string()
                });
            let findings = report.for_file(path);
            let prompt = alignment_prompt_text(AlignmentPrompt {
                goal: job,
                project_plan: plan_text,
                manifest,
                written: writes.as_slice(),
                target: path,
                current: &current,
                findings: &findings,
                audit: &full_audit,
            });
            let reply = match worker.complete(&prompt) {
                Ok(reply) => reply,
                Err(err) => {
                    on_trace(WorkTrace::note(
                        format!("Alignment error: {path}"),
                        err.to_string(),
                    ));
                    let _ = writeln!(&mut alignment_log, "[{path}] ERROR: {err}");
                    continue;
                }
            };
            let reply = if let Some(request) = parse_tool_request(&reply) {
                let result =
                    execute_project_tool(&self.workspace_root, &request.name, &request.arguments);
                on_trace(WorkTrace::note(
                    result
                        .label
                        .clone()
                        .map(|label| format!("Alignment {label}"))
                        .unwrap_or_else(|| format!("Alignment tool {}", request.name)),
                    result.output.clone(),
                ));
                let _ = writeln!(
                    &mut alignment_log,
                    "[{path}] TOOL {} ok={} done={}: {}",
                    request.name, result.ok, result.done, result.output
                );
                if let Some(write) = result.write {
                    upsert_write(writes, write);
                    continue;
                }
                let followup = format!(
                    "{prompt}\n\nTool result from {}:\n{}\n\nNow return the complete corrected contents for {path} as one fenced code block.",
                    request.name, result.output
                );
                match worker.complete(&followup) {
                    Ok(reply) => reply,
                    Err(err) => {
                        on_trace(WorkTrace::note(
                            format!("Alignment error: {path}"),
                            err.to_string(),
                        ));
                        let _ = writeln!(&mut alignment_log, "[{path}] ERROR: {err}");
                        continue;
                    }
                }
            } else {
                reply
            };
            let body = extract_file_body(&reply);
            if body.trim().is_empty() {
                on_trace(WorkTrace::note(
                    format!("Alignment skipped {path}"),
                    "model returned an empty rewrite".to_string(),
                ));
                let _ = writeln!(&mut alignment_log, "[{path}] SKIPPED: empty rewrite");
                continue;
            }
            match write_workspace_file(&self.workspace_root, path, &body) {
                Ok(write) => {
                    on_trace(WorkTrace::note(
                        format!("Aligned {path}"),
                        format!("{} bytes", body.len()),
                    ));
                    let _ = writeln!(&mut alignment_log, "[{path}] {} bytes", body.len());
                    if let Some(idx) = idx {
                        writes[idx] = write;
                    } else {
                        writes.push(write);
                    }
                }
                Err(err) => {
                    on_trace(WorkTrace::note(
                        format!("Alignment skipped {path}"),
                        err.to_string(),
                    ));
                    let _ = writeln!(&mut alignment_log, "[{path}] SKIPPED: {err}");
                }
            }
        }
        let prompt_cid = self.store.put_node_with_edges(
            Node::Prompt(Prompt {
                text: report.render(),
                model: Some(worker_name.clone()),
            }),
            Source::System,
            vec![Edge {
                rel: EdgeRel::DerivedFrom,
                to: worker_result_cid,
            }],
        )?;
        let tool_cid = self.write_tool_result(
            &worker_name,
            &report.render(),
            &alignment_log,
            true,
            prompt_cid,
        )?;
        Ok((prompt_cid, tool_cid))
    }

    fn write_tool_result(
        &self,
        model_name: &str,
        input: &str,
        output: &str,
        ok: bool,
        prompt_cid: Cid,
    ) -> anyhow::Result<Cid> {
        self.store.put_node_with_edges(
            Node::ToolResult(ToolResult {
                tool: format!("model.complete:{model_name}"),
                input: input.to_string(),
                output: output.to_string(),
                ok,
            }),
            Source::System,
            vec![Edge {
                rel: EdgeRel::DerivedFrom,
                to: prompt_cid,
            }],
        )
    }

    fn write_conversation_head(&mut self) -> anyhow::Result<()> {
        let convo_cid = self.store.put_node(
            Node::Conversation(Conversation {
                turns: self.turns.clone(),
                parent: self.head,
            }),
            Source::System,
        )?;
        self.store.bind(&self.convo_name, convo_cid)?;
        self.head = Some(convo_cid);
        Ok(())
    }

    fn prompt_text(&self, user_input: &str) -> anyhow::Result<String> {
        if self.loaded.is_empty() {
            return Ok(user_input.to_string());
        }

        let mut prompt = String::from("Loaded session context:\n");
        for (idx, loaded) in self.loaded.iter().enumerate() {
            let json = serde_json::to_string_pretty(&loaded.record)?;
            writeln!(&mut prompt, "[{idx}] cid: {}", loaded.cid)?;
            writeln!(&mut prompt, "{json}")?;
        }
        writeln!(&mut prompt, "\nUser prompt:\n{user_input}")?;
        Ok(prompt)
    }

    fn planning_prompt_text(&self, job: &str, project_files: &str) -> anyhow::Result<String> {
        let prompt = format!(
            "You are the Concierge. Write one complete execution plan for the \
             worker as a single whole-task handoff. The worker will receive \
             this entire plan verbatim.\n\n\
             Current project file map:\n{project_files}\n\n\
             Job:\n{job}"
        );
        self.prompt_text(&prompt)
    }

    fn context_edges(&self) -> Vec<Edge> {
        self.loaded
            .iter()
            .map(|loaded| Edge {
                rel: EdgeRel::UsedAsContext,
                to: loaded.cid,
            })
            .collect()
    }

    fn resolve_target(&self, target: &str) -> anyhow::Result<Cid> {
        match target.parse::<Cid>() {
            Ok(cid) => Ok(cid),
            Err(_) => self.store.resolve(target),
        }
    }

    /// Fetch a record by name or CID without changing the session.
    pub fn recall(&self, target: &str) -> anyhow::Result<Record> {
        let cid = self.resolve_target(target)?;
        self.store.get_node(&cid)
    }

    /// Manual recall: fetch a record by name or CID and load it into session
    /// context for later turns.
    pub fn load(&mut self, target: &str) -> anyhow::Result<Record> {
        let cid = self.resolve_target(target)?;
        let record = self.store.get_node(&cid)?;
        self.loaded.push(LoadedRecord {
            cid,
            record: record.clone(),
        });
        Ok(record)
    }

    /// Number of records loaded as context.
    pub fn loaded_context_len(&self) -> usize {
        self.loaded.len()
    }

    /// The CID and record currently loaded as context.
    pub fn loaded_context(&self) -> impl Iterator<Item = (Cid, &Record)> {
        self.loaded
            .iter()
            .map(|loaded| (loaded.cid, &loaded.record))
    }

    /// The current conversation head CID, once at least one turn has happened.
    pub fn head(&self) -> Option<Cid> {
        self.head
    }

    /// Number of records in the current conversation turn list.
    pub fn turns_len(&self) -> usize {
        self.turns.len()
    }

    /// The configured auto-checkpoint name (default `latest`) — the target
    /// `/resume` and `mem resume` use when none is given.
    pub fn checkpoint_name(&self) -> &str {
        &self.checkpoint.name
    }

    /// Snapshot the current conversation as a `Checkpoint` and bind `name` to
    /// it, so the session can be resumed from that CID after a restart.
    pub fn checkpoint(&mut self, label: &str, name: &str) -> anyhow::Result<Cid> {
        let root = self
            .head
            .ok_or_else(|| anyhow::anyhow!("nothing to checkpoint: no turns yet"))?;
        let cid = self.store.put_node(
            Node::Checkpoint(Checkpoint {
                label: label.to_string(),
                root,
                parent: None,
            }),
            Source::System,
        )?;
        self.store.bind(name, cid)?;
        Ok(cid)
    }

    /// After a recorded turn, snapshot on the configured cadence.
    fn maybe_checkpoint(&mut self) -> anyhow::Result<()> {
        if !self.checkpoint.auto {
            return Ok(());
        }
        self.turns_since_checkpoint += 1;
        if self.turns_since_checkpoint >= self.checkpoint.every_turns.max(1) {
            self.write_auto_checkpoint()?;
            self.turns_since_checkpoint = 0;
        }
        Ok(())
    }

    /// Write a labeled auto `Checkpoint` chained to the previous one and rebind
    /// the well-known name (default `latest`). An Observer: it snapshots, never
    /// decides what to recall.
    fn write_auto_checkpoint(&mut self) -> anyhow::Result<()> {
        let Some(root) = self.head else {
            return Ok(());
        };
        let cid = self.store.put_node(
            Node::Checkpoint(Checkpoint {
                label: "auto".to_string(),
                root,
                parent: self.last_checkpoint,
            }),
            Source::System,
        )?;
        self.store.bind(&self.checkpoint.name, cid)?;
        self.last_checkpoint = Some(cid);
        Ok(())
    }

    /// Flush a trailing snapshot on clean exit (catches the tail when
    /// `every_turns > 1`). No-op when nothing has happened since the last one.
    pub fn checkpoint_on_exit(&mut self) -> anyhow::Result<()> {
        if self.checkpoint.auto && self.checkpoint.on_exit && self.turns_since_checkpoint > 0 {
            self.write_auto_checkpoint()?;
            self.turns_since_checkpoint = 0;
        }
        Ok(())
    }

    /// Restore this session from a checkpoint name or CID.
    ///
    /// The checkpoint root must be a `Conversation`. The durable conversation
    /// turn list and head are restored, and any records previously linked as
    /// prompt context are loaded back into the live session.
    pub fn resume_from_checkpoint(&mut self, target: &str) -> anyhow::Result<ResumeInfo> {
        let checkpoint_cid = self.resolve_target(target)?;
        let checkpoint = match self.store.get_node(&checkpoint_cid)?.body {
            Node::Checkpoint(checkpoint) => checkpoint,
            other => anyhow::bail!("resume target must be a Checkpoint, got {other:?}"),
        };
        let conversation = match self.store.get_node(&checkpoint.root)?.body {
            Node::Conversation(conversation) => conversation,
            other => anyhow::bail!(
                "checkpoint {} root {} must be a Conversation, got {other:?}",
                checkpoint.label,
                checkpoint.root
            ),
        };

        self.turns = conversation.turns;
        self.head = Some(checkpoint.root);
        self.loaded = self.load_context_from_turns()?;
        self.last_checkpoint = Some(checkpoint_cid);
        self.turns_since_checkpoint = 0;

        Ok(ResumeInfo {
            checkpoint: checkpoint_cid,
            root: checkpoint.root,
            turns: self.turns.len(),
            loaded_context: self.loaded.len(),
        })
    }

    fn load_context_from_turns(&self) -> anyhow::Result<Vec<LoadedRecord>> {
        let mut loaded = Vec::new();
        for turn_cid in &self.turns {
            let record = self.store.get_node(turn_cid)?;
            if !matches!(record.body, Node::Prompt(_)) {
                continue;
            }
            for edge in record.edges {
                if edge.rel != EdgeRel::UsedAsContext
                    || loaded
                        .iter()
                        .any(|known: &LoadedRecord| known.cid == edge.to)
                {
                    continue;
                }
                loaded.push(LoadedRecord {
                    cid: edge.to,
                    record: self.store.get_node(&edge.to)?,
                });
            }
        }
        Ok(loaded)
    }
}

fn plan_title(job: &str) -> String {
    let trimmed = job.trim();
    let title: String = trimmed.chars().take(80).collect();
    if title.is_empty() {
        "work plan".to_string()
    } else {
        title
    }
}

#[cfg(test)]
mod tests;

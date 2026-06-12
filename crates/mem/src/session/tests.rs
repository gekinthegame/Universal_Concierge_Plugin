use super::*;
use crate::config::VerifierConfig;
use crate::names::NameIndex;
use crate::node::Skill;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::Path;
use std::rc::Rc;
use tempfile::TempDir;

struct MockModel {
    name: &'static str,
    result: MockResult,
    prompts: Rc<RefCell<Vec<String>>>,
    /// Scripted `complete` replies, consumed front-to-back. When non-empty each
    /// call returns the next reply; when empty it falls back to `result`.
    script: RefCell<VecDeque<String>>,
}

enum MockResult {
    Reply(String),
    Error(String),
}

impl Model for MockModel {
    fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        self.prompts.borrow_mut().push(prompt.to_string());
        if let Some(next) = self.script.borrow_mut().pop_front() {
            return Ok(next);
        }
        match &self.result {
            MockResult::Reply(reply) => Ok(reply.clone()),
            MockResult::Error(error) => anyhow::bail!("{error}"),
        }
    }
    fn name(&self) -> &str {
        self.name
    }
}

/// A mock that returns `reply` for every `complete` call (chat/turn roles, and
/// any single-shot use).
fn plain_mock(name: &'static str, reply: &str, prompts: Rc<RefCell<Vec<String>>>) -> MockModel {
    MockModel {
        name,
        result: MockResult::Reply(reply.to_string()),
        prompts,
        script: RefCell::new(VecDeque::new()),
    }
}

/// A mock that returns each scripted reply in order across successive `complete`
/// calls.
fn scripted_mock(
    name: &'static str,
    replies: Vec<&str>,
    prompts: Rc<RefCell<Vec<String>>>,
) -> MockModel {
    MockModel {
        name,
        result: MockResult::Reply(String::new()),
        prompts,
        script: RefCell::new(replies.into_iter().map(String::from).collect()),
    }
}

/// Wrap a file body in a fenced code block, the way the worker replies.
fn fenced(body: &str) -> String {
    format!("Here is the file:\n```ts\n{body}\n```")
}

fn write_react_scaffold(workspace: &Path) {
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::write(workspace.join("package.json"), "{}").unwrap();
    std::fs::write(workspace.join("index.html"), "<div id=\"root\"></div>").unwrap();
    std::fs::write(workspace.join("tsconfig.json"), "{}").unwrap();
    std::fs::write(workspace.join("src/main.tsx"), "import './App';").unwrap();
}

fn session(reply: &str) -> (TempDir, Session<MockModel>, Rc<RefCell<Vec<String>>>) {
    let dir = TempDir::new().unwrap();
    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);
    let prompts = Rc::new(RefCell::new(Vec::new()));
    let session = Session::new(
        store,
        plain_mock("mock", reply, prompts.clone()),
        "conversation",
    );
    (dir, session, prompts)
}

fn failing_session(error: &str) -> (TempDir, Session<MockModel>, Rc<RefCell<Vec<String>>>) {
    let dir = TempDir::new().unwrap();
    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);
    let prompts = Rc::new(RefCell::new(Vec::new()));
    let session = Session::new(
        store,
        MockModel {
            name: "mock",
            result: MockResult::Error(error.to_string()),
            prompts: prompts.clone(),
            script: RefCell::new(VecDeque::new()),
        },
        "conversation",
    );
    (dir, session, prompts)
}

#[test]
fn checkpoint_errors_with_no_turns_then_snapshots_the_head() {
    let (_d, mut s, _p) = session("ok");
    assert!(
        s.checkpoint("empty", "current-project").is_err(),
        "nothing to checkpoint before a turn"
    );

    s.turn("hello").unwrap();
    let head = s.head().unwrap();
    s.checkpoint("first", "current-project").unwrap();

    match s.recall("current-project").unwrap().body {
        Node::Checkpoint(c) => {
            assert_eq!(c.label, "first");
            assert_eq!(c.root, head, "checkpoint root is the conversation head");
        }
        other => panic!("expected Checkpoint, got {other:?}"),
    }
}

#[test]
fn resume_from_checkpoint_restores_head_turns_loaded_context_and_chains_next_turn() {
    let (dir, mut s, _prompts) = session("before");
    let skill_cid = s
        .store
        .put_node(
            Node::Skill(Skill {
                name: "code-review".into(),
                body: "inspect error handling".into(),
                supersedes: None,
            }),
            Source::System,
        )
        .unwrap();
    s.store.bind("skill:code-review", skill_cid).unwrap();
    s.load("skill:code-review").unwrap();
    s.turn("first").unwrap();
    let checkpoint_root = s.head().unwrap();
    let checkpoint_cid = s.checkpoint("resume-test", "current-project").unwrap();
    drop(s);

    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);
    let prompts = Rc::new(RefCell::new(Vec::new()));
    let mut resumed = Session::new(
        store,
        plain_mock("mock", "after", prompts.clone()),
        "conversation",
    );

    let info = resumed.resume_from_checkpoint("current-project").unwrap();
    assert_eq!(info.checkpoint, checkpoint_cid);
    assert_eq!(info.root, checkpoint_root);
    assert_eq!(info.turns, 3);
    assert_eq!(info.loaded_context, 1);
    assert_eq!(resumed.head(), Some(checkpoint_root));
    assert_eq!(resumed.turns_len(), 3);
    assert_eq!(resumed.loaded_context_len(), 1);
    assert_eq!(resumed.loaded_context().next().unwrap().0, skill_cid);

    assert_eq!(resumed.turn("continue").unwrap(), "after");
    let model_prompt = prompts.borrow()[0].clone();
    assert!(model_prompt.contains("Loaded session context"));
    assert!(model_prompt.contains("inspect error handling"));
    assert!(model_prompt.contains("User prompt:\ncontinue"));

    let next_head = resumed.head().unwrap();
    assert_ne!(next_head, checkpoint_root);
    match resumed.recall("conversation").unwrap().body {
        Node::Conversation(c) => {
            assert_eq!(c.parent, Some(checkpoint_root));
            assert_eq!(c.turns.len(), 6);
        }
        other => panic!("expected Conversation, got {other:?}"),
    }
}

#[test]
fn turn_returns_the_model_reply() {
    let (_d, mut s, prompts) = session("hi there");
    assert_eq!(s.turn("hello").unwrap(), "hi there");
    assert_eq!(prompts.borrow().as_slice(), ["hello"]);
}

#[test]
fn turn_streaming_emits_the_answer_then_a_closing_newline() {
    let (_d, mut s, _p) = session("streamed reply");
    let mut seen = String::new();
    let answer = s
        .turn_streaming("hi", &mut |fragment| seen.push_str(fragment))
        .unwrap();
    // The recorded answer stays clean; the display gets a trailing newline.
    assert_eq!(answer, "streamed reply");
    assert_eq!(seen, "streamed reply\n");

    // And the clean answer is what landed in the Response node.
    let convo = s.recall("conversation").unwrap();
    let turns = match convo.body {
        Node::Conversation(c) => c.turns,
        other => panic!("expected Conversation, got {other:?}"),
    };
    match s.recall(&turns[2].to_string()).unwrap().body {
        Node::Response(r) => assert_eq!(r.text, "streamed reply"),
        other => panic!("expected Response, got {other:?}"),
    }
}

#[test]
fn a_turn_writes_prompt_tool_result_response_conversation_and_rebinds_head() {
    let (_d, mut s, _prompts) = session("the answer");
    s.turn("the question").unwrap();
    assert!(s.head().is_some(), "head set after a turn");

    // the conversation head is bound to the conversation name
    let convo = s.recall("conversation").unwrap();
    let turns = match convo.body {
        Node::Conversation(c) => c.turns,
        other => panic!("expected Conversation, got {other:?}"),
    };
    assert_eq!(
        turns.len(),
        3,
        "one prompt + one tool result + one response"
    );

    let prompt = s.recall(&turns[0].to_string()).unwrap();
    match prompt.body {
        Node::Prompt(p) => assert_eq!(p.text, "the question"),
        other => panic!("expected Prompt, got {other:?}"),
    }

    let tool = s.recall(&turns[1].to_string()).unwrap();
    match &tool.body {
        Node::ToolResult(t) => {
            assert_eq!(t.tool, "model.complete:mock");
            assert_eq!(t.input, "the question");
            assert_eq!(t.output, "the answer");
            assert!(t.ok);
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
    assert!(
        tool.edges
            .iter()
            .any(|e| e.rel == EdgeRel::DerivedFrom && e.to == turns[0]),
        "tool result must link to its prompt"
    );

    let response = s.recall(&turns[2].to_string()).unwrap();
    match &response.body {
        Node::Response(r) => assert_eq!(r.text, "the answer"),
        other => panic!("expected Response, got {other:?}"),
    }
    // response is linked back to its prompt and tool result, and provenance is the model
    assert!(
        response
            .edges
            .iter()
            .any(|e| e.rel == EdgeRel::DerivedFrom && e.to == turns[0]),
        "response must link to its prompt"
    );
    assert!(
        response
            .edges
            .iter()
            .any(|e| e.rel == EdgeRel::DerivedFrom && e.to == turns[1]),
        "response must link to its tool result"
    );
    assert!(matches!(response.source, Source::Model { .. }));
}

#[test]
fn second_turn_chains_the_conversation_via_parent() {
    let (_d, mut s, _prompts) = session("ok");
    s.turn("first").unwrap();
    let first_head = s.head().unwrap();
    s.turn("second").unwrap();
    let second = s.recall("conversation").unwrap();
    match second.body {
        Node::Conversation(c) => {
            assert_eq!(
                c.turns.len(),
                6,
                "two prompts + two tool results + two responses"
            );
            assert_eq!(
                c.parent,
                Some(first_head),
                "head chains to the prior conversation"
            );
        }
        other => panic!("expected Conversation, got {other:?}"),
    }
}

#[test]
fn recall_errors_on_unknown_name() {
    let (_d, s, _prompts) = session("x");
    assert!(s.recall("nothing-here").is_err());
}

#[test]
fn load_adds_context_to_later_model_prompts_and_prompt_edges() {
    let (_d, mut s, prompts) = session("ok");
    let skill_cid = s
        .store
        .put_node(
            Node::Skill(Skill {
                name: "code-review".into(),
                body: "inspect the diff first".into(),
                supersedes: None,
            }),
            Source::System,
        )
        .unwrap();
    s.store.bind("skill:code-review", skill_cid).unwrap();

    let loaded = s.load("skill:code-review").unwrap();
    assert_eq!(s.loaded_context_len(), 1);
    assert_eq!(s.loaded_context().next().unwrap().0, skill_cid);
    match loaded.body {
        Node::Skill(skill) => assert_eq!(skill.name, "code-review"),
        other => panic!("expected Skill, got {other:?}"),
    }

    s.turn("use the loaded skill").unwrap();

    let model_prompt = prompts.borrow()[0].clone();
    assert!(model_prompt.contains("Loaded session context"));
    assert!(model_prompt.contains(&skill_cid.to_string()));
    assert!(model_prompt.contains("inspect the diff first"));
    assert!(model_prompt.contains("User prompt:\nuse the loaded skill"));

    let convo = s.recall("conversation").unwrap();
    let turns = match convo.body {
        Node::Conversation(c) => c.turns,
        other => panic!("expected Conversation, got {other:?}"),
    };
    let prompt = s.recall(&turns[0].to_string()).unwrap();
    match prompt.body {
        Node::Prompt(p) => assert_eq!(p.text, model_prompt),
        other => panic!("expected Prompt, got {other:?}"),
    }
    assert!(
        prompt
            .edges
            .iter()
            .any(|e| e.rel == EdgeRel::UsedAsContext && e.to == skill_cid),
        "prompt must link to loaded context"
    );
}

#[test]
fn work_requires_a_worker_without_calling_concierge() {
    let (_d, mut s, prompts) = session("plan");
    let err = s.work("write files").unwrap_err().to_string();
    assert!(err.contains("no worker model configured"), "got: {err}");
    assert!(
        prompts.borrow().is_empty(),
        "must not ask Concierge for a plan when no worker can execute it"
    );
}

/// Live end-to-end check against a running Ollama. Ignored by default (needs
/// the configured models). Run with:
///   cargo test -p mem live_work_builds_files_via_manifest_and_per_file -- --ignored --nocapture
#[test]
#[ignore]
fn live_work_builds_files_via_manifest_and_per_file() {
    use crate::config::ModelConfig;
    use crate::model::build;

    let cfg = |name: &str| ModelConfig {
        host: "http://127.0.0.1:11434".to_string(),
        name: name.to_string(),
        provider: "ollama".to_string(),
    };
    let dir = TempDir::new().unwrap();
    let workspace = dir.path().join("workspace");
    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);

    let concierge = build(&cfg("ssfdre38/gemma4-turbo:e4b")).unwrap();
    let worker = build(&cfg("rnj-1:8b")).unwrap();
    let mut s = Session::new(store, concierge, "conversation")
        .with_worker(worker)
        .with_workspace_root(&workspace);

    let report = s
        .work("Build a tiny TypeScript greeter: a src/greet.ts exporting greet(name) and a src/index.ts that calls it.")
        .expect("live worker run");
    assert!(
        !report.files.is_empty(),
        "worker should write at least one file"
    );
}

#[test]
fn work_fails_when_concierge_produces_no_manifest() {
    let dir = TempDir::new().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);
    // Concierge writes a plan, then a manifest reply that contains no file paths.
    let mut s = Session::new(
        store,
        scripted_mock(
            "concierge",
            vec![
                "Complete plan for the worker.",
                "Sorry, I have no files to list.",
            ],
            Rc::new(RefCell::new(Vec::new())),
        ),
        "conversation",
    )
    .with_worker(Box::new(plain_mock(
        "worker",
        "unused",
        Rc::new(RefCell::new(Vec::new())),
    )))
    .with_workspace_root(&workspace);

    let mut trace = Vec::new();
    let err = s
        .work_with_trace("build app", &mut |event| trace.push(event))
        .unwrap_err()
        .to_string();
    assert!(err.contains("no file manifest"), "got: {err}");

    assert!(
        trace.iter().any(|event| event.label == "Concierge manifest"
            && event.body.starts_with("0 file(s)")),
        "trace should show the empty manifest"
    );

    let conversation = match s.recall("conversation").unwrap().body {
        Node::Conversation(conversation) => conversation,
        other => panic!("expected Conversation, got {other:?}"),
    };
    assert_eq!(
        conversation.turns.len(),
        6,
        "plan(3) + manifest prompt/tool(2) + failed manifest tool(1)"
    );
    match s.recall(&conversation.turns[5].to_string()).unwrap().body {
        Node::ToolResult(result) => {
            assert_eq!(result.tool, "work.manifest");
            assert!(!result.ok);
            assert!(result.output.contains("no file manifest"));
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[test]
fn work_builds_each_manifest_file_from_plan_without_the_user_prompt() {
    let dir = TempDir::new().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::write(workspace.join("src/other.ts"), "keep\n").unwrap();

    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);
    let worker_prompts = Rc::new(RefCell::new(Vec::new()));
    let complete_plan = "PLAN-LINE-ONE\nPLAN-LINE-TWO\nNo chunking.";
    let mut s = Session::new(
        store,
        scripted_mock(
            "concierge",
            vec![complete_plan, "src/app.ts"],
            Rc::new(RefCell::new(Vec::new())),
        ),
        "conversation",
    )
    .with_worker(Box::new(scripted_mock(
        "worker",
        vec![&fenced("new")],
        worker_prompts.clone(),
    )))
    .with_workspace_root(&workspace);

    let report = s.work("update the existing app file").unwrap();
    assert_eq!(
        std::fs::read_to_string(workspace.join("src/app.ts")).unwrap(),
        "new\n",
        "the guided file is written by the per-file worker pass"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("src/other.ts")).unwrap(),
        "keep\n",
        "files not in the manifest are untouched"
    );

    let worker_prompt = worker_prompts.borrow()[0].clone();
    assert!(
        worker_prompt.contains(&format!("Project plan:\n{complete_plan}")),
        "worker must receive the plan"
    );
    assert!(
        worker_prompt.contains("Write the complete contents of this one file:\nsrc/app.ts"),
        "worker receives one file target at a time; prompt:\n{worker_prompt}"
    );
    assert!(
        !worker_prompt.contains("update the existing app file"),
        "worker should receive the Concierge plan, not the raw user prompt; prompt:\n{worker_prompt}"
    );
    assert_eq!(report.files[0].path, "src/app.ts");

    match s.recall(&report.plan.to_string()).unwrap().body {
        Node::Plan(plan) => assert_eq!(plan.prose, complete_plan),
        other => panic!("expected Plan, got {other:?}"),
    }
    let response = s.recall(&report.worker_response.to_string()).unwrap();
    assert!(
        response
            .edges
            .iter()
            .any(|edge| edge.rel == EdgeRel::Produced && edge.to == report.files[0].cid),
        "worker response must produce the written FileRef"
    );
}

#[test]
fn work_iterates_the_manifest_passing_already_written_paths() {
    let dir = TempDir::new().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);
    let worker_prompts = Rc::new(RefCell::new(Vec::new()));
    let mut s = Session::new(
        store,
        scripted_mock(
            "concierge",
            vec!["plan", "src/a.ts\nsrc/b.ts"],
            Rc::new(RefCell::new(Vec::new())),
        ),
        "conversation",
    )
    .with_worker(Box::new(scripted_mock(
        "worker",
        vec![
            &fenced("export class Alpha {}"),
            &fenced("export class Beta {}"),
        ],
        worker_prompts.clone(),
    )))
    .with_workspace_root(&workspace);

    let report = s.work("two files").unwrap();
    assert_eq!(report.files.len(), 2);
    assert_eq!(
        std::fs::read_to_string(workspace.join("src/a.ts")).unwrap(),
        "export class Alpha {}\n"
    );

    let second_prompt = worker_prompts.borrow()[1].clone();
    assert!(
        second_prompt.contains("src/a.ts — defines: Alpha"),
        "later per-file prompts show already-written public symbols; prompt:\n{second_prompt}"
    );
}

#[test]
fn work_can_create_new_project_files_in_one_flow() {
    let dir = TempDir::new().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);
    let mut s = Session::new(
        store,
        scripted_mock(
            "concierge",
            vec!["create a new project file", "src/new_app.ts"],
            Rc::new(RefCell::new(Vec::new())),
        ),
        "conversation",
    )
    .with_worker(Box::new(scripted_mock(
        "worker",
        vec![&fenced("created")],
        Rc::new(RefCell::new(Vec::new())),
    )))
    .with_workspace_root(&workspace);

    let report = s.work("create new file").unwrap();
    assert_eq!(report.files[0].path, "src/new_app.ts");
    assert_eq!(
        std::fs::read_to_string(workspace.join("src/new_app.ts")).unwrap(),
        "created\n"
    );
}

#[test]
fn work_alignment_can_use_project_tools_on_flagged_files() {
    let dir = TempDir::new().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    write_react_scaffold(&workspace);

    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);
    let broken = "export const App = () => <div className=\"bg-blue-600 text-gray-400\">hi</div>;";
    let mut s = Session::new(
        store,
        scripted_mock(
            "concierge",
            vec!["plan", "src/App.tsx", "Replace the flagged low-contrast text class."],
            Rc::new(RefCell::new(Vec::new())),
        ),
        "conversation",
    )
    .with_worker(Box::new(scripted_mock(
        "worker",
        vec![
            &fenced(broken),
            r#"{"name":"edit_file","arguments":{"path":"src/App.tsx","old":"text-gray-400","new":"text-white"}}"#,
        ],
        Rc::new(RefCell::new(Vec::new())),
    )))
    .with_workspace_root(&workspace);

    let mut trace = Vec::new();
    let report = s
        .work_with_trace("build", &mut |event| trace.push(event))
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(workspace.join("src/App.tsx")).unwrap(),
        "export const App = () => <div className=\"bg-blue-600 text-white\">hi</div>;\n",
        "alignment tool edits are applied to disk and the in-memory file set"
    );
    assert_eq!(report.files[0].path, "src/App.tsx");
    assert!(
        trace
            .iter()
            .any(|event| event.label == "Alignment Edited src/App.tsx"),
        "alignment tool use should be visible in the river trace"
    );
    assert!(
        trace
            .iter()
            .any(|event| event.label == "Audit (repair lap 1)" && event.body.contains("Clean")),
        "re-audit after the alignment tool edit should be clean"
    );
}

#[test]
fn work_skips_unsafe_manifest_paths_without_escaping_the_project() {
    let dir = TempDir::new().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);
    // The only manifest entry is an escape path. The worker returns a body, but
    // the write is rejected and skipped, so the run fails with no files — and
    // nothing lands outside the project.
    let mut s = Session::new(
        store,
        scripted_mock(
            "concierge",
            vec!["try to escape", "../escape.txt"],
            Rc::new(RefCell::new(Vec::new())),
        ),
        "conversation",
    )
    .with_worker(Box::new(scripted_mock(
        "worker",
        vec![&fenced("nope")],
        Rc::new(RefCell::new(Vec::new())),
    )))
    .with_workspace_root(&workspace);

    let mut trace = Vec::new();
    let err = s
        .work_with_trace("escape", &mut |event| trace.push(event))
        .unwrap_err()
        .to_string();
    assert!(err.contains("worker wrote no files"), "got: {err}");
    assert!(!dir.path().join("escape.txt").exists());
    assert!(
        trace
            .iter()
            .any(|event| event.label == "Skipped ../escape.txt"
                && event.body.contains("cannot contain '..'")),
        "the unsafe path should be skipped with a recorded reason"
    );
}

#[test]
fn work_deterministically_fixes_overclimbing_import_paths() {
    let dir = TempDir::new().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    write_react_scaffold(&workspace);

    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);
    // Worker emits a `../../` import that climbs one level too far. Alignment
    // model — the deterministic fixer alone should resolve it.
    let mut s = Session::new(
        store,
        scripted_mock(
            "concierge",
            vec![
                "plan",
                "src/types/models.ts\nsrc/components/ContractsView.tsx",
            ],
            Rc::new(RefCell::new(Vec::new())),
        ),
        "conversation",
    )
    .with_worker(Box::new(scripted_mock(
        "worker",
        vec![
            &fenced("export interface Contract {}"),
            &fenced("import { Contract } from '../../types/models';\nexport const View = 1;"),
        ],
        Rc::new(RefCell::new(Vec::new())),
    )))
    .with_workspace_root(&workspace);

    let mut trace = Vec::new();
    s.work_with_trace("build", &mut |event| trace.push(event))
        .unwrap();

    assert!(
        std::fs::read_to_string(workspace.join("src/components/ContractsView.tsx"))
            .unwrap()
            .contains("from '../types/models'"),
        "the over-climbing import path is corrected deterministically"
    );
    assert!(
        trace.iter().any(
            |event| event.label == "Fixed import in src/components/ContractsView.tsx"
                && event
                    .body
                    .contains("'../../types/models' → '../types/models'")
        ),
        "the fix is traced"
    );
    assert!(
        trace
            .iter()
            .any(|event| event.label == "Audit" && event.body.contains("No defects found")),
        "after the deterministic fix the audit is clean"
    );
}

#[test]
fn work_flags_design_anti_patterns_and_cleans_them() {
    let dir = TempDir::new().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    write_react_scaffold(&workspace);

    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);
    // Worker emits gray-on-colored-background (a design tell, not a code bug).
    let mut s = Session::new(
        store,
        scripted_mock(
            "concierge",
            vec!["plan", "src/App.tsx"],
            Rc::new(RefCell::new(Vec::new())),
        ),
        "conversation",
    )
    .with_worker(Box::new(scripted_mock(
        "worker",
        vec![
            &fenced(
                "export const App = () => <div className=\"bg-blue-600 text-gray-400\">hi</div>;",
            ),
            &fenced("export const App = () => <div className=\"bg-blue-600 text-white\">hi</div>;"),
        ],
        Rc::new(RefCell::new(Vec::new())),
    )))
    .with_workspace_root(&workspace);

    let mut trace = Vec::new();
    s.work_with_trace("build", &mut |event| trace.push(event))
        .unwrap();

    assert!(
        trace
            .iter()
            .any(|e| e.label == "Audit" && e.body.contains("Design anti-patterns")),
        "the design tell is surfaced by the audit"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("src/App.tsx")).unwrap(),
        "export const App = () => <div className=\"bg-blue-600 text-white\">hi</div>;\n",
        "the workhorse alignment pass fixed the gray-on-color tell"
    );
    assert!(
        trace
            .iter()
            .any(|e| e.label == "Audit (repair lap 1)" && e.body.contains("Clean")),
        "re-audit clean after the design fix"
    );
}

#[test]
fn work_repair_loop_recirculates_until_clean() {
    let dir = TempDir::new().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    write_react_scaffold(&workspace);

    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);
    // Two files, each with a gray-on-color tell. The workhorse fixes A on lap 1
    // but returns B unchanged-broken; lap 2 fixes B.
    let a_broken = "export const A = () => <div className=\"bg-blue-600 text-gray-400\">a</div>;";
    let a_fixed = "export const A = () => <div className=\"bg-blue-600 text-white\">a</div>;";
    let b_broken = "export const B = () => <div className=\"bg-red-600 text-gray-400\">b</div>;";
    let b_fixed = "export const B = () => <div className=\"bg-red-600 text-white\">b</div>;";
    let mut s = Session::new(
        store,
        scripted_mock(
            "concierge",
            vec!["plan", "src/A.tsx\nsrc/B.tsx"],
            Rc::new(RefCell::new(Vec::new())),
        ),
        "conversation",
    )
    .with_worker(Box::new(scripted_mock(
        "worker",
        vec![
            &fenced(a_broken),
            &fenced(b_broken),
            &fenced(a_fixed),
            &fenced(b_broken),
            &fenced(b_fixed),
        ],
        Rc::new(RefCell::new(Vec::new())),
    )))
    .with_workspace_root(&workspace);

    let mut trace = Vec::new();
    s.work_with_trace("build", &mut |event| trace.push(event))
        .unwrap();

    assert!(
        std::fs::read_to_string(workspace.join("src/A.tsx"))
            .unwrap()
            .contains("text-white"),
        "A fixed on lap 1"
    );
    assert!(
        std::fs::read_to_string(workspace.join("src/B.tsx"))
            .unwrap()
            .contains("text-white"),
        "B fixed on lap 2"
    );
    assert!(
        trace
            .iter()
            .any(|e| e.label == "Audit (repair lap 2)" && e.body.contains("Clean")),
        "two laps recirculate to a clean audit"
    );
}

#[test]
fn work_repair_loop_stops_on_no_progress() {
    let dir = TempDir::new().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    write_react_scaffold(&workspace);

    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);
    let broken = "export const A = () => <div className=\"bg-blue-600 text-gray-400\">a</div>;";
    let mut s = Session::new(
        store,
        scripted_mock(
            "concierge",
            vec!["plan", "src/A.tsx"],
            Rc::new(RefCell::new(Vec::new())),
        ),
        "conversation",
    )
    .with_worker(Box::new(scripted_mock(
        "worker",
        vec![&fenced(broken), &fenced(broken)],
        Rc::new(RefCell::new(Vec::new())),
    )))
    .with_workspace_root(&workspace);

    let mut trace = Vec::new();
    let err = s
        .work_with_trace("build", &mut |event| trace.push(event))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("work audit has unresolved findings"),
        "got: {err}"
    );

    assert!(
        trace
            .iter()
            .any(|e| e.label == "Repair" && e.body.contains("no progress")),
        "the loop stops when a lap does not shrink the audit"
    );
    assert!(
        std::fs::read_to_string(workspace.join("src/A.tsx"))
            .unwrap()
            .contains("text-gray-400"),
        "unfixed file remains on disk for inspection"
    );
    assert!(!trace.iter().any(|e| e.label == "Audit (repair lap 3)"));
}

#[test]
fn work_dirty_audit_fails_when_alignment_makes_no_progress() {
    let dir = TempDir::new().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);
    // Worker writes a file with a broken import, then returns an empty alignment.
    let mut s = Session::new(
        store,
        scripted_mock(
            "concierge",
            vec!["plan", "src/a.ts"],
            Rc::new(RefCell::new(Vec::new())),
        ),
        "conversation",
    )
    .with_worker(Box::new(scripted_mock(
        "worker",
        vec![&fenced("import { Gone } from './missing';")],
        Rc::new(RefCell::new(Vec::new())),
    )))
    .with_workspace_root(&workspace);

    let mut trace = Vec::new();
    let err = s
        .work_with_trace("build", &mut |event| trace.push(event))
        .unwrap_err()
        .to_string();

    assert!(
        err.contains("work audit has unresolved findings"),
        "got: {err}"
    );
    assert!(
        std::fs::read_to_string(workspace.join("src/a.ts"))
            .unwrap()
            .contains("Gone"),
        "the dirty file remains on disk for inspection"
    );
    assert!(
        trace
            .iter()
            .any(|event| event.label == "Audit" && event.body.contains("Broken imports")),
        "the audit must surface the defect instead of silently passing"
    );
}

#[test]
fn work_alignment_repairs_flagged_files_against_original_plan() {
    let dir = TempDir::new().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);
    let worker_prompts = Rc::new(RefCell::new(Vec::new()));
    let mut s = Session::new(
        store,
        scripted_mock(
            "concierge",
            vec!["plan", "src/a.ts"],
            Rc::new(RefCell::new(Vec::new())),
        ),
        "conversation",
    )
    .with_worker(Box::new(scripted_mock(
        "worker",
        vec![
            &fenced("import { Gone } from './missing';"),
            &fenced("export const ok = 1;"),
        ],
        worker_prompts.clone(),
    )))
    .with_workspace_root(&workspace);

    let mut trace = Vec::new();
    let report = s
        .work_with_trace("build", &mut |event| trace.push(event))
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(workspace.join("src/a.ts")).unwrap(),
        "export const ok = 1;\n",
        "the workhorse alignment pass rewrote the flagged file"
    );
    assert_eq!(report.files[0].path, "src/a.ts");

    let alignment_prompt = worker_prompts.borrow()[1].clone();
    assert!(alignment_prompt.contains("Original project plan:\nplan"));
    assert!(alignment_prompt.contains("File for this alignment step: src/a.ts"));
    assert!(alignment_prompt.contains("Findings for this file:"));
    assert!(
        alignment_prompt.contains("Current contents:\nimport { Gone } from './missing';"),
        "workhorse is shown the broken file; prompt:\n{alignment_prompt}"
    );

    assert!(trace.iter().any(|event| event.label == "Aligned src/a.ts"));
    assert!(
        trace
            .iter()
            .any(|event| event.label == "Audit (repair lap 1)" && event.body.contains("Clean")),
        "re-audit after alignment should be clean"
    );

    let conversation = match s.recall("conversation").unwrap().body {
        Node::Conversation(conversation) => conversation,
        other => panic!("expected Conversation, got {other:?}"),
    };
    assert_eq!(
        conversation.turns.len(),
        10,
        "plan(3) + manifest(2) + worker prompt/aggregate(2) + alignment(2) + response(1)"
    );
}

#[test]
fn work_scaffold_findings_reenter_through_workhorse_alignment_and_create_missing_files() {
    let dir = TempDir::new().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);
    let concierge_prompts = Rc::new(RefCell::new(Vec::new()));
    let worker_prompts = Rc::new(RefCell::new(Vec::new()));
    let mut s = Session::new(
        store,
        scripted_mock(
            "concierge",
            vec!["Build a React app.", "src/App.tsx"],
            concierge_prompts.clone(),
        ),
        "conversation",
    )
    .with_worker(Box::new(scripted_mock(
        "worker",
        vec![
            &fenced("import React from 'react';\nexport const App = () => <main>BX</main>;"),
            &fenced(
                "<div id=\"root\"></div>\n<script type=\"module\" src=\"/src/main.tsx\"></script>",
            ),
            &fenced("{\"scripts\":{\"build\":\"vite --host 0.0.0.0\"},\"dependencies\":{\"@vitejs/plugin-react\":\"latest\",\"vite\":\"latest\",\"typescript\":\"latest\",\"react\":\"latest\",\"react-dom\":\"latest\"},\"devDependencies\":{}}"),
            &fenced("import React from 'react';\nimport { createRoot } from 'react-dom/client';\nimport { App } from './App';\n\ncreateRoot(document.getElementById('root')!).render(<App />);"),
            &fenced("{\"compilerOptions\":{\"jsx\":\"react-jsx\",\"strict\":true},\"include\":[\"src\"]}"),
        ],
        worker_prompts.clone(),
    )))
    .with_workspace_root(&workspace);

    let mut trace = Vec::new();
    let report = s
        .work_with_trace("build a React app", &mut |event| trace.push(event))
        .unwrap();

    let paths: Vec<&str> = report.files.iter().map(|file| file.path.as_str()).collect();
    assert!(paths.contains(&"src/App.tsx"));
    assert!(paths.contains(&"package.json"));
    assert!(paths.contains(&"index.html"));
    assert!(paths.contains(&"tsconfig.json"));
    assert!(paths.contains(&"src/main.tsx"));
    assert!(workspace.join("package.json").exists());
    assert!(workspace.join("index.html").exists());
    assert!(workspace.join("tsconfig.json").exists());
    assert!(workspace.join("src/main.tsx").exists());

    assert!(
        concierge_prompts.borrow().len() == 2,
        "Concierge should produce the original plan and manifest only"
    );
    let alignment_prompt = worker_prompts.borrow()[1].clone();
    assert!(alignment_prompt.contains("Original project plan:\nBuild a React app."));
    assert!(alignment_prompt.contains("Scaffold findings"));
    assert!(alignment_prompt.contains("package.json"));
    assert!(
        trace
            .iter()
            .any(|event| event.label == "Workhorse alignment"),
        "repair re-enters through workhorse alignment"
    );
    assert!(
        trace
            .iter()
            .any(|event| event.label == "Audit (repair lap 1)" && event.body.contains("Clean")),
        "scaffold creation should re-audit clean"
    );
}

#[test]
fn work_verifier_failure_reenters_through_workhorse_alignment() {
    let dir = TempDir::new().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);
    let concierge_prompts = Rc::new(RefCell::new(Vec::new()));
    let worker_prompts = Rc::new(RefCell::new(Vec::new()));
    let mut s = Session::new(
        store,
        scripted_mock(
            "concierge",
            vec!["Build a Python app.", "pyproject.toml\nmain.py"],
            concierge_prompts.clone(),
        ),
        "conversation",
    )
    .with_worker(Box::new(scripted_mock(
        "worker",
        vec![
            &fenced("[project]\nname = \"verify-demo\"\nversion = \"0.1.0\""),
            &fenced("def main(:\n    return 1"),
            &fenced("def main():\n    return 1"),
        ],
        worker_prompts.clone(),
    )))
    .with_workspace_root(&workspace)
    .with_verifier(VerifierConfig {
        enabled: true,
        install: false,
        test: false,
        timeout_seconds: 5,
    });

    let mut trace = Vec::new();
    let report = s
        .work_with_trace("build a Python app", &mut |event| trace.push(event))
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(workspace.join("main.py")).unwrap(),
        "def main():\n    return 1\n"
    );
    assert!(report.files.iter().any(|file| file.path == "main.py"));
    assert!(
        trace
            .iter()
            .any(|event| event.label == "Verification failed" && event.body.contains("compileall")),
        "compileall failure should be surfaced in trace: {trace:#?}"
    );
    assert!(
        concierge_prompts.borrow().len() == 2,
        "Concierge should produce the original plan and manifest only"
    );
    assert!(
        worker_prompts.borrow()[2].contains("sandboxed verifier failed"),
        "alignment prompt should include verifier evidence"
    );
    assert!(
        trace
            .iter()
            .any(|event| event.label == "Verification" && event.body.contains("exit_code: 0")),
        "repair should re-run verifier cleanly: {trace:#?}"
    );
}

fn auto_policy(name: &str, every_turns: u32, on_exit: bool) -> CheckpointConfig {
    CheckpointConfig {
        auto: true,
        every_turns,
        on_exit,
        name: name.to_string(),
        keep_checkpoints: 10,
    }
}

#[test]
fn no_auto_checkpoint_unless_enabled() {
    let (_d, mut s, _p) = session("ok");
    s.turn("one").unwrap();
    assert!(
        s.recall("latest").is_err(),
        "no `latest` checkpoint without auto enabled"
    );
}

#[test]
fn auto_checkpoint_binds_latest_and_chains_each_turn() {
    let (_d, mut s, _p) = session("ok");
    s = s.with_checkpoints(auto_policy("latest", 1, true));

    s.turn("one").unwrap();
    let head1 = s.head().unwrap();
    let cp1 = s.store.resolve("latest").unwrap();
    match s.recall("latest").unwrap().body {
        Node::Checkpoint(c) => {
            assert_eq!(c.label, "auto");
            assert_eq!(c.root, head1);
            assert_eq!(c.parent, None, "first auto-checkpoint has no parent");
        }
        other => panic!("expected Checkpoint, got {other:?}"),
    }

    s.turn("two").unwrap();
    let head2 = s.head().unwrap();
    match s.recall("latest").unwrap().body {
        Node::Checkpoint(c) => {
            assert_eq!(c.root, head2);
            assert_eq!(c.parent, Some(cp1), "auto-checkpoints chain via parent");
        }
        other => panic!("expected Checkpoint, got {other:?}"),
    }
}

#[test]
fn auto_checkpoint_respects_cadence_and_on_exit_flush() {
    let (_d, mut s, _p) = session("ok");
    s = s.with_checkpoints(auto_policy("latest", 3, true));

    s.turn("one").unwrap();
    s.turn("two").unwrap();
    assert!(
        s.recall("latest").is_err(),
        "no snapshot before the cadence is reached"
    );

    s.turn("three").unwrap();
    assert!(s.recall("latest").is_ok(), "snapshot at every_turns = 3");

    // a trailing turn (below the cadence) is captured on exit
    s.turn("four").unwrap();
    s.checkpoint_on_exit().unwrap();
    let head = s.head().unwrap();
    match s.recall("latest").unwrap().body {
        Node::Checkpoint(c) => assert_eq!(c.root, head),
        other => panic!("expected Checkpoint, got {other:?}"),
    }
}

#[test]
fn auto_checkpoint_chains_to_resumed_checkpoint_after_restart() {
    let (dir, mut s, _p) = session("before");
    s = s.with_checkpoints(auto_policy("latest", 1, true));

    s.turn("one").unwrap();
    let resumed_parent = s.store.resolve("latest").unwrap();
    let resumed_root = s.head().unwrap();
    drop(s);

    let blocks = LocalBlocks::new(dir.path().join("blocks"));
    let names = NameIndex::load(dir.path().join("names.json")).unwrap();
    let store = Store::new(blocks, names);
    let prompts = Rc::new(RefCell::new(Vec::new()));
    let mut resumed = Session::new(store, plain_mock("mock", "after", prompts), "conversation")
        .with_checkpoints(auto_policy("latest", 1, true));

    let info = resumed.resume_from_checkpoint("latest").unwrap();
    assert_eq!(info.checkpoint, resumed_parent);
    assert_eq!(info.root, resumed_root);

    resumed.turn("two").unwrap();
    let next_head = resumed.head().unwrap();
    let next_checkpoint = resumed.store.resolve("latest").unwrap();
    assert_ne!(next_checkpoint, resumed_parent);
    match resumed.recall("latest").unwrap().body {
        Node::Checkpoint(c) => {
            assert_eq!(c.root, next_head);
            assert_eq!(
                c.parent,
                Some(resumed_parent),
                "auto-checkpoint parent survives resume/restart"
            );
        }
        other => panic!("expected Checkpoint, got {other:?}"),
    }
}

#[test]
fn resume_defaults_to_the_latest_auto_checkpoint() {
    let (_d, mut s, _p) = session("ok");
    s = s.with_checkpoints(auto_policy("latest", 1, true));
    assert_eq!(s.checkpoint_name(), "latest");

    s.turn("one").unwrap();
    let head = s.head().unwrap();

    // what `/resume` with no argument does: resume the session's default name
    let name = s.checkpoint_name().to_string();
    let info = s.resume_from_checkpoint(&name).unwrap();
    assert_eq!(info.root, head);
}

#[test]
fn failed_model_call_records_failed_tool_result_and_rebinds_head() {
    let (_d, mut s, prompts) = failing_session("offline");

    let err = s.turn("hello").unwrap_err().to_string();
    assert!(err.contains("offline"));
    assert_eq!(prompts.borrow().as_slice(), ["hello"]);
    assert!(
        s.head().is_some(),
        "failed model call still records the turn"
    );

    let convo = s.recall("conversation").unwrap();
    let turns = match convo.body {
        Node::Conversation(c) => c.turns,
        other => panic!("expected Conversation, got {other:?}"),
    };
    assert_eq!(turns.len(), 2, "prompt + failed tool result");
    let tool = s.recall(&turns[1].to_string()).unwrap();
    match tool.body {
        Node::ToolResult(t) => {
            assert_eq!(t.tool, "model.complete:mock");
            assert_eq!(t.input, "hello");
            assert!(t.output.contains("offline"));
            assert!(!t.ok);
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[test]
fn failed_model_call_is_auto_checkpointed_when_recorded() {
    let (_d, mut s, prompts) = failing_session("offline");
    s = s.with_checkpoints(auto_policy("latest", 1, true));

    let err = s.turn("hello").unwrap_err().to_string();
    assert!(err.contains("offline"));
    assert_eq!(prompts.borrow().as_slice(), ["hello"]);
    let head = s.head().unwrap();

    match s.recall("latest").unwrap().body {
        Node::Checkpoint(c) => {
            assert_eq!(c.label, "auto");
            assert_eq!(c.root, head);
            assert_eq!(c.parent, None);
        }
        other => panic!("expected Checkpoint, got {other:?}"),
    }

    match s.recall("conversation").unwrap().body {
        Node::Conversation(c) => assert_eq!(c.turns.len(), 2),
        other => panic!("expected Conversation, got {other:?}"),
    }
}

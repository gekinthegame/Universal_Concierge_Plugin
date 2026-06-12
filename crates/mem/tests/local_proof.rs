//! Phase 3.3 — the full local proof milestone, end to end, on `LocalBlocks`
//! only. No external service.
//!
//! Drive a conversation through the REPL session (with a recalled skill in
//! context), snapshot a `Checkpoint`, **kill the process** (drop everything),
//! **restart** from the same directory, resolve the checkpoint, resume the
//! conversation, and recall the named skill. If this passes, the substrate is
//! real — before any network exists.

use mem::blockstore::LocalBlocks;
use mem::cid::Cid;
use mem::dag::reachable_from;
use mem::model::Model;
use mem::names::NameIndex;
use mem::node::{EdgeRel, Node, Skill, Source};
use mem::repl::Session;
use mem::store::{Store, SystemClock};
use std::collections::BTreeSet;
use std::path::Path;
use tempfile::TempDir;

/// A deterministic stand-in for the configured model: echoes a canned reply.
struct ScriptedModel {
    reply: String,
}
impl Model for ScriptedModel {
    fn complete(&self, _prompt: &str) -> anyhow::Result<String> {
        Ok(self.reply.clone())
    }
    fn name(&self) -> &str {
        "scripted"
    }
}

fn open_store(root: &Path) -> Store<LocalBlocks, SystemClock> {
    let blocks = LocalBlocks::new(root.join("blocks"));
    let names = NameIndex::load(root.join("names.json")).unwrap();
    Store::new(blocks, names)
}

#[test]
fn chat_checkpoint_kill_restart_resume_and_recall_skill() {
    mem::trace::set_verbosity(0);
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    // ---- session 1: seed a skill, chat with it loaded, checkpoint ----
    let checkpoint_cid: Cid = {
        let mut store = open_store(root);
        let skill_cid = store
            .put_node(
                Node::Skill(Skill {
                    name: "code-review".into(),
                    body: "Always check error handling and tests.".into(),
                    supersedes: None,
                }),
                Source::System,
            )
            .unwrap();
        store.bind("skill:code-review", skill_cid).unwrap();

        let mut session = Session::new(
            store,
            ScriptedModel {
                reply: "understood".into(),
            },
            "conversation",
        );

        session.load("skill:code-review").unwrap();
        session.turn("review my code").unwrap();
        session.turn("anything else?").unwrap();

        session
            .checkpoint("local-proof", "current-project")
            .unwrap()
    }; // everything dropped here — "process killed"

    // ---- session 2: a fresh store over the SAME dir — "restart" ----
    let store = open_store(root);

    // resume from the checkpoint name
    let resolved = store.resolve("current-project").unwrap();
    assert_eq!(resolved, checkpoint_cid, "checkpoint name survives restart");

    let convo_root = match store.get_node(&resolved).unwrap().body {
        Node::Checkpoint(cp) => {
            assert_eq!(cp.label, "local-proof");
            cp.root
        }
        other => panic!("expected Checkpoint, got {other:?}"),
    };

    // the conversation resumes with full context: 2 turns × (prompt+tool+response)
    let turns = match store.get_node(&convo_root).unwrap().body {
        Node::Conversation(c) => c.turns,
        other => panic!("expected Conversation, got {other:?}"),
    };
    assert_eq!(turns.len(), 6, "two turns, three records each");

    // the recalled skill reached the first prompt and is linked as context
    let skill_cid = store.resolve("skill:code-review").unwrap();
    let first_prompt = store.get_node(&turns[0]).unwrap();
    match &first_prompt.body {
        Node::Prompt(p) => assert!(
            p.text.contains("error handling"),
            "loaded skill context must reach the prompt"
        ),
        other => panic!("expected Prompt, got {other:?}"),
    }
    assert!(
        first_prompt
            .edges
            .iter()
            .any(|e| e.rel == EdgeRel::UsedAsContext && e.to == skill_cid),
        "prompt links to the loaded skill as context"
    );

    // walk the persisted graph from checkpoint root after restart
    let local_blocks = LocalBlocks::new(root.join("blocks"));
    let reachable: BTreeSet<Cid> = reachable_from(&local_blocks, &resolved)
        .unwrap()
        .into_iter()
        .collect();
    assert!(
        reachable.contains(&resolved),
        "walk includes checkpoint root"
    );
    assert!(
        reachable.contains(&convo_root),
        "walk follows checkpoint.root"
    );
    assert!(
        reachable.contains(&skill_cid),
        "walk follows prompt UsedAsContext edge to skill"
    );
    for turn in &turns {
        assert!(reachable.contains(turn), "walk includes turn {turn}");
    }

    // and the named skill is itself recallable after restart
    match store.get_node(&skill_cid).unwrap().body {
        Node::Skill(s) => {
            assert_eq!(s.name, "code-review");
            assert!(s.body.contains("error handling"));
        }
        other => panic!("expected Skill, got {other:?}"),
    }

    // finally, a fresh REPL session resumes from the checkpoint and keeps
    // writing through the restored conversation.
    let mut resumed = Session::new(
        store,
        ScriptedModel {
            reply: "resumed".into(),
        },
        "conversation",
    );
    let info = resumed.resume_from_checkpoint("current-project").unwrap();
    assert_eq!(info.checkpoint, checkpoint_cid);
    assert_eq!(info.root, convo_root);
    assert_eq!(info.turns, 6);
    assert_eq!(info.loaded_context, 1);
    assert_eq!(resumed.head(), Some(convo_root));
    assert_eq!(resumed.turns_len(), 6);
    assert_eq!(resumed.loaded_context_len(), 1);

    match resumed.recall("skill:code-review").unwrap().body {
        Node::Skill(s) => {
            assert_eq!(s.name, "code-review");
            assert!(s.body.contains("error handling"));
        }
        other => panic!("expected Skill, got {other:?}"),
    }

    assert_eq!(resumed.turn("continue after restart").unwrap(), "resumed");
    let resumed_root = resumed.head().unwrap();
    assert_ne!(resumed_root, convo_root);
    let resumed_turns = match resumed.recall("conversation").unwrap().body {
        Node::Conversation(c) => {
            assert_eq!(c.parent, Some(convo_root));
            c.turns
        }
        other => panic!("expected Conversation, got {other:?}"),
    };
    assert_eq!(
        resumed_turns.len(),
        9,
        "resumed turn extends prior 6 records by prompt+tool+response"
    );

    match resumed.recall(&resumed_turns[6].to_string()).unwrap().body {
        Node::Prompt(p) => {
            assert!(p.text.contains("Loaded session context"));
            assert!(p.text.contains("error handling"));
            assert!(p.text.contains("User prompt:\ncontinue after restart"));
        }
        other => panic!("expected Prompt, got {other:?}"),
    }
}

//! Phase 8 §4 — Memory Synthesis (Summarization Checkpoints).
//!
//! **No generation on-node** (Decision 0022, rule 3). The node:
//! 1. **detects** when a room thread is large enough to be worth synthesizing (a
//!    *candidate*),
//! 2. **assembles** the thread so the *host's* model can summarize it, and
//! 3. **records** the host-returned summary as a `decision` node that links back to
//!    the synthesized messages as provenance.
//!
//! The actual generation is always the host's. If no host model is available, a
//! candidate simply stays unsynthesized — the node never spins up an LLM to do it.

use std::collections::BTreeSet;

use crate::binding::{Cid, CoreBinding, MemCli, Node};
use crate::error::Result;

/// A room thread becomes a synthesis candidate at this many messages (§4).
pub const SYNTHESIS_THRESHOLD: usize = 50;

/// A room flagged as worth synthesizing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynthesisCandidate {
    pub room: String,
    pub message_count: usize,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Every room the node knows about (the message book + bound `room-latest-` names).
fn known_rooms(mem: &MemCli) -> Result<BTreeSet<String>> {
    let mut rooms: BTreeSet<String> = mem.room_book()?.rooms.keys().cloned().collect();
    for (name, _) in mem.names()? {
        if let Some(room) = name.strip_prefix("room-latest-") {
            rooms.insert(room.to_string());
        }
    }
    Ok(rooms)
}

/// Rooms whose thread has reached `threshold` messages — flagged for (host-side)
/// synthesis. The Guardian surfaces these; it does not summarize them itself.
pub fn synthesis_candidates(mem: &MemCli, threshold: usize) -> Result<Vec<SynthesisCandidate>> {
    let mut out = Vec::new();
    for room in known_rooms(mem)? {
        let count = mem.room_thread(&room)?.len();
        if count >= threshold {
            out.push(SynthesisCandidate { room, message_count: count });
        }
    }
    Ok(out)
}

/// Assemble a room thread into plain text **for the host model to summarize** —
/// the node performs no summarization. Returns the joined messages and the
/// provenance CIDs (the exact sub-graph that was synthesized).
pub fn assemble_thread(mem: &MemCli, room: &str) -> Result<(String, Vec<Cid>)> {
    let thread = mem.room_thread(room)?;
    let text = thread
        .iter()
        .map(|(_, env)| format!("{}: {}", env.author(), env.payload))
        .collect::<Vec<_>>()
        .join("\n");
    let cids = thread.into_iter().map(|(cid, _)| cid).collect();
    Ok((text, cids))
}

/// Record a **host-produced** `summary` of `room` as a `decision` node *derived
/// from* the synthesized messages — so `walk`/graph-gravity follow the links back
/// to the source sub-graph as provenance — and bind a name for it. Returns the new
/// node's CID. The summary text comes *from the host*: this function does no
/// generation, it only persists what the host returned. The resulting CID is a
/// `--pinned` candidate for the §1 context-packer (surfacing, not auto-injection).
pub fn record_synthesis(
    mem: &MemCli,
    room: &str,
    summary: &str,
    provenance: &[Cid],
) -> Result<Cid> {
    // `Decision` keeps only question/choice/rationale; the provenance travels as a
    // derived `Source` (real, gravity-counted edges), not as body fields.
    let body = serde_json::json!({
        "question": format!("Synthesis of room '{room}' ({} messages)", provenance.len()),
        "choice": summary,
        "rationale": "host-model synthesis; the node performed no generation",
    });
    let cid = mem.put_node_derived(
        &Node { kind: "decision".to_string(), fields_json: body.to_string() },
        provenance,
    )?;
    mem.bind(&format!("synthesis-{room}-{}", now_secs()), &cid)?;
    Ok(cid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::{CidOrName, Record};

    #[test]
    fn a_room_becomes_a_candidate_only_past_the_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        for i in 0..4 {
            mem.post_message("design", &format!("message {i}")).unwrap();
        }
        // 4 messages: a candidate at threshold 3, not at threshold 5.
        let at3 = synthesis_candidates(&mem, 3).unwrap();
        assert!(at3.iter().any(|c| c.room == "design" && c.message_count == 4));
        assert!(synthesis_candidates(&mem, 5).unwrap().is_empty(), "below threshold → not flagged");
    }

    #[test]
    fn assemble_thread_gathers_messages_and_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        mem.post_message("r", "first idea").unwrap();
        mem.post_message("r", "second idea").unwrap();
        let (text, provenance) = assemble_thread(&mem, "r").unwrap();
        assert!(text.contains("first idea") && text.contains("second idea"));
        assert_eq!(provenance.len(), 2, "both messages are provenance");
    }

    #[test]
    fn record_synthesis_writes_a_decision_with_provenance_and_no_generation() {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        mem.post_message("r", "a").unwrap();
        mem.post_message("r", "b").unwrap();
        let (_text, provenance) = assemble_thread(&mem, "r").unwrap();
        // The HOST produced this summary; the node only records it.
        let cid = record_synthesis(&mem, "r", "The team agreed on X.", &provenance).unwrap();

        let Record::Live { kind, body_json, .. } = mem.get(&CidOrName::Cid(cid.clone())).unwrap()
        else {
            panic!("expected a live record");
        };
        assert_eq!(kind, "decision", "synthesis is recorded as a Decision node");
        // The full record nests the node under `body`.
        let value: serde_json::Value = serde_json::from_str(&body_json).unwrap();
        assert_eq!(value["body"]["choice"], "The team agreed on X.", "the host summary is the choice");

        // Provenance is real derived links: walking the synthesis reaches the source messages.
        let reachable = mem.walk(&cid).unwrap();
        for source in &provenance {
            assert!(reachable.contains(source), "synthesis links back to its source sub-graph");
        }
    }
}

use mem::node::{Edge, EdgeRel, Link, Node, Record, Source};

fn link() -> Link {
    mem::cid::compute(b"sample-link")
}

fn main() {
    let l = link();
    let bodies = vec![
        Node::Memory(mem::node::Memory {
            text: "m".into(),
            kind: mem::node::MemoryKind::Project,
        }),
        Node::UserPrefs(mem::node::UserPrefs {
            entries: vec![("k".into(), "v".into())],
        }),
        Node::Plan(mem::node::Plan {
            title: "t".into(),
            prose: "p".into(),
            spec: Some(l),
        }),
        Node::Decision(mem::node::Decision {
            question: "q".into(),
            choice: "c".into(),
            rationale: "r".into(),
        }),
        Node::Prompt(mem::node::Prompt {
            text: "t".into(),
            model: Some("opus".into()),
        }),
        Node::Response(mem::node::Response {
            text: "t".into(),
            model: "opus".into(),
        }),
        Node::ToolResult(mem::node::ToolResult {
            tool: "t".into(),
            input: "i".into(),
            output: "o".into(),
            ok: true,
        }),
        Node::Blob(mem::node::Blob {
            bytes: vec![1, 2, 3],
            media_type: Some("text/plain".into()),
        }),
        Node::FileRef(mem::node::FileRef {
            path: "/p".into(),
            size: Some(3),
            media_type: Some("text/plain".into()),
            mtime: Some(1234),
            content: l,
        }),
        Node::Task(mem::node::Task {
            title: "t".into(),
            prose: "p".into(),
            parent: Some(l),
        }),
        Node::Conversation(mem::node::Conversation {
            turns: vec![l],
            parent: None,
        }),
        Node::Skill(mem::node::Skill {
            name: "n".into(),
            body: "b".into(),
            supersedes: Some(l),
        }),
        Node::Checkpoint(mem::node::Checkpoint {
            label: "l".into(),
            root: l,
            parent: Some(l),
        }),
        Node::DirectoryManifest(mem::node::DirectoryManifest {
            root_path: "/r".into(),
            entries: vec![mem::node::DirectoryEntry {
                path: "f".into(),
                file_ref: l,
            }],
        }),
        Node::IngestRun(mem::node::IngestRun {
            source_path: "/s".into(),
            manifest: l,
            file_count: 1,
            byte_count: 2,
            ignored_count: 3,
            plugin_records: 4,
            plugin_failures: 5,
            per_file_plugin_records: [("f".into(), 6)].into(),
            per_file_plugin_failures: [("f".into(), 7)].into(),
        }),
        Node::Symbol(mem::node::Symbol {
            path: "/p".into(),
            name: "n".into(),
            kind: "k".into(),
            language: "l".into(),
            signature: "s".into(),
            body: "b".into(),
            start_line: 1,
            end_line: 2,
        }),
        Node::ExtractedText(mem::node::ExtractedText {
            path: "/p".into(),
            text: "t".into(),
            media_type: Some("m".into()),
        }),
    ];

    let sources = [
        Source::User,
        Source::System,
        Source::Model {
            name: "opus".into(),
        },
        Source::Derived { from: vec![l] },
    ];

    let edge_rels = [
        EdgeRel::Produced,
        EdgeRel::PlannedAs,
        EdgeRel::DerivedFrom,
        EdgeRel::UsedAsContext,
        EdgeRel::References,
        EdgeRel::Contains,
        EdgeRel::Summarizes,
        EdgeRel::Supersedes,
    ];

    for (i, body) in bodies.into_iter().enumerate() {
        let record = Record {
            schema_version: mem::node::CURRENT_SCHEMA_VERSION,
            created_at: 1000 + i as u64,
            source: sources[i % sources.len()].clone(),
            edges: vec![Edge {
                rel: edge_rels[i % edge_rels.len()],
                to: l,
            }],
            body: body.clone(),
        };

        let bytes = mem::node::encode(&record).unwrap();
        let cid = mem::cid::compute(&bytes);
        println!(
            "(\"{}\", \"{}\", \"{}\"),",
            body.kind(),
            hex::encode(bytes),
            cid
        );
    }
}

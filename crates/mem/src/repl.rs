//! Terminal-facing REPL facade for a memory `Session`.

use crate::commands::{
    checkpoint_command_arg, parse_checkpoint_args, resume_command_arg, work_command_arg,
};
use crate::model::Model;
use crate::work::WorkTrace;
use std::io::Write as _;

pub use crate::session::{ResumeInfo, Session};
pub use crate::work::{WorkFile, WorkReport};

/// Run the interactive command loop until the user exits.
///
/// `/work <job>` runs a whole-plan worker handoff and writes the worker's file
/// blocks; `/load <name|cid>` recalls a record; `/checkpoint [name] [label...]`
/// snapshots the conversation; `/resume [name|cid]` restores one (defaults to
/// the auto-checkpoint, `latest`); `/quit` (or EOF/Ctrl-C) exits. Everything
/// else is passed to the session as a chat turn.
pub fn run(mut session: Session<Box<dyn Model>>) -> anyhow::Result<()> {
    let mut rl = rustyline::DefaultEditor::new()?;
    let worker = session
        .worker_name()
        .map(|name| format!(", worker {name}"))
        .unwrap_or_default();
    println!(
        "concierge ready ({}{}). type to chat; `/work`, `/load`, `/checkpoint`, `/resume`, `/quit`.",
        session.model_name(),
        worker
    );
    loop {
        match rl.readline("you ▷ ") {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if line == "/quit" || line == "/exit" {
                    break;
                }
                if let Some(arg) = line.strip_prefix("/load ") {
                    match session.load(arg.trim()) {
                        Ok(record) => println!("{}", serde_json::to_string_pretty(&record)?),
                        Err(e) => eprintln!("recall: {e}"),
                    }
                    continue;
                }
                if let Some(arg) = checkpoint_command_arg(line) {
                    let arg = arg.trim();
                    let (name, label) = parse_checkpoint_args(arg);
                    match session.checkpoint(label, name) {
                        Ok(cid) => println!("checkpoint {name} -> {cid}"),
                        Err(e) => eprintln!("checkpoint: {e}"),
                    }
                    continue;
                }
                if let Some(arg) = resume_command_arg(line) {
                    let arg = arg.trim();
                    let target = if arg.is_empty() {
                        session.checkpoint_name().to_string()
                    } else {
                        arg.to_string()
                    };
                    match session.resume_from_checkpoint(&target) {
                        Ok(info) => println!(
                            "resumed {} -> {} ({} records, {} loaded)",
                            info.checkpoint, info.root, info.turns, info.loaded_context
                        ),
                        Err(e) => eprintln!("resume: {e}"),
                    }
                    continue;
                }
                if let Some(arg) = work_command_arg(line) {
                    let job = arg.trim();
                    if job.is_empty() {
                        eprintln!("work: usage /work <job>");
                        continue;
                    }
                    let _ = rl.add_history_entry(line);
                    let mut open_stream = false;
                    match session.work_with_trace(job, &mut |trace| {
                        print_work_trace(&mut open_stream, trace)
                    }) {
                        Ok(report) => {
                            close_work_trace_stream(&mut open_stream);
                            println!("plan -> {}", report.plan);
                            println!("worker response -> {}", report.worker_response);
                            for file in report.files {
                                println!("wrote {} -> {}", file.path, file.cid);
                            }
                        }
                        Err(e) => {
                            close_work_trace_stream(&mut open_stream);
                            eprintln!("work: {e}");
                        }
                    }
                    continue;
                }
                let _ = rl.add_history_entry(line);
                // Stream the answer live: the prefix prints with the first
                // fragment (after the prompt-node trace), tokens flush as they
                // arrive, and `turn_streaming` emits the closing newline.
                let mut started = false;
                let result = session.turn_streaming(line, &mut |fragment| {
                    if !started {
                        print!("concierge ◁ ");
                        started = true;
                    }
                    print!("{fragment}");
                    let _ = std::io::stdout().flush();
                });
                if let Err(e) = result {
                    if started {
                        println!();
                    }
                    eprintln!("error: {e}");
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted)
            | Err(rustyline::error::ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("input error: {e}");
                break;
            }
        }
    }
    session.checkpoint_on_exit()?;
    Ok(())
}

fn print_work_trace(open_stream: &mut bool, trace: WorkTrace) {
    if trace.stream {
        if !*open_stream {
            match &trace.model {
                Some(model) => print!("\n[{} - {}]\n", trace.label, model),
                None => print!("\n[{}]\n", trace.label),
            }
            *open_stream = true;
        }
        print!("{}", trace.body);
        let _ = std::io::stdout().flush();
        return;
    }

    close_work_trace_stream(open_stream);
    let body = trace.body.trim();
    if body.is_empty() {
        return;
    }
    match trace.model {
        Some(model) => println!("\n[{} - {}]\n{}", trace.label, model, body),
        None => println!("\n[{}]\n{}", trace.label, body),
    }
}

fn close_work_trace_stream(open_stream: &mut bool) {
    if *open_stream {
        println!();
        *open_stream = false;
    }
}

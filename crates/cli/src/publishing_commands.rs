use super::*;
use concierge_core::{BackendInfo, EgressPlan};

fn resolve_or_cid(mem: &MemCli, arg: &str) -> Cid {
    mem.resolve(arg).unwrap_or_else(|_| Cid(arg.to_string()))
}

/// Make a name safe to use as a default `.car` filename.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `export-car <name|cid> [out.car] [--dry-run]` — export the subgraph reachable
/// from a root as a CARv1 (or preview its manifest with `--dry-run`).
pub(super) fn cmd_export_car(args: &[String]) -> ExitCode {
    let Some(target) = args.get(1).map(String::as_str) else {
        eprintln!("usage: concierge-plugin export-car <name|cid> [out.car] [--dry-run]");
        return ExitCode::from(2);
    };
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let outfile = args
        .get(2)
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| format!("{}.car", sanitize(target)));

    let mem = MemCli::new(workdir());
    let root = resolve_or_cid(&mem, target);

    if dry_run {
        match mem.build_egress_plan_for_target(target, EgressOperation::PlaintextCarExport) {
            Ok(plan) => {
                print_egress_preview(&plan);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("export-car failed: {e}");
                ExitCode::FAILURE
            }
        }
    } else {
        let plan = match mem.build_egress_plan_for_target_and_backend(
            target,
            EgressOperation::PlaintextCarExport,
            "local-file",
            &outfile,
            "plaintext-portable",
        ) {
            Ok(plan) => plan,
            Err(e) => {
                eprintln!("export-car failed: {e}");
                return ExitCode::FAILURE;
            }
        };
        print_egress_preview(&plan);
        println!("Warning: this writes a portable plaintext CAR outside the store.");
        if !args.iter().any(|arg| arg == "--confirm-plaintext-export") {
            eprintln!("export-car refused: review the manifest and add --confirm-plaintext-export");
            return ExitCode::from(2);
        }
        match mem.write_reviewed_plaintext_car(&plan, std::path::Path::new(&outfile)) {
            Ok(bytes) => {
                println!("exported root {} → {outfile} ({bytes} bytes)", root.0);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("export-car failed: {e}");
                ExitCode::FAILURE
            }
        }
    }
}

/// `lock <root|name> [--label L]` — mark a root and its entire reachable
/// subgraph Locked / Local-only. Any later public publish or plaintext export
/// whose manifest reaches a locked node is refused by the core egress guard.
pub(super) fn cmd_lock(args: &[String]) -> ExitCode {
    let Some(target) = args.get(1).map(String::as_str) else {
        eprintln!("usage: concierge-plugin lock <root|name> [--label \"...\"]");
        return ExitCode::from(2);
    };
    let label = flag_value(args, "--label").unwrap_or_default();
    let mem = MemCli::new(workdir());
    let root = resolve_or_cid(&mem, target);
    match mem.lock_subgraph(&root, &label) {
        Ok(()) => {
            let n = mem
                .export_car_manifest(&root)
                .map(|(c, _)| c.len())
                .unwrap_or(0);
            println!("locked {} (+{n} reachable nodes) — local-only", root.0);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("lock failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `locks` — list the current Locked / Local-only roots.
pub(super) fn cmd_locks() -> ExitCode {
    let mem = MemCli::new(workdir());
    match mem.locks() {
        Ok(locks) if locks.is_empty() => {
            println!("no locked roots");
            ExitCode::SUCCESS
        }
        Ok(locks) => {
            for l in locks {
                let label = if l.label.is_empty() { "-" } else { &l.label };
                println!("{}\t{label}", l.root);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("locks failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `import-car <file.car> <name>` — import a CARv1, verifying every block's CID,
/// then bind the root under `name`.
pub(super) fn cmd_import_car(args: &[String]) -> ExitCode {
    let (Some(file), Some(name)) = (
        args.get(1).map(String::as_str),
        args.get(2).map(String::as_str),
    ) else {
        eprintln!("usage: concierge-plugin import-car <file.car> <name>");
        return ExitCode::from(2);
    };
    let car = match std::fs::read(file) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("import-car failed: cannot read {file}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mem = MemCli::new(workdir());
    let agent_id = args
        .iter()
        .position(|a| a == "--agent-id")
        .and_then(|i| args.get(i + 1))
        .map(String::as_str);
    let signature = args
        .iter()
        .position(|a| a == "--signature")
        .and_then(|i| args.get(i + 1))
        .map(String::as_str);

    let result = match (agent_id, signature) {
        (Some(agent_id), Some(signature)) => mem.import_signed_car(&car, name, agent_id, signature),
        (None, None) => mem.import_car(&car, name),
        _ => {
            eprintln!("import-car failed: use both --agent-id and --signature together");
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(root) => {
            println!("imported root {} bound to `{name}`", root.0);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("import-car failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_egress_preview(plan: &EgressPlan) {
    println!(
        "egress preview: {} root {} -> {} blocks, {} bytes via {} to {} ({})",
        plan.operation.label(),
        plan.root.0,
        plan.block_count,
        plan.byte_size,
        plan.backend,
        plan.backend_target,
        plan.network_posture,
    );
    if !plan.file_paths.is_empty() {
        println!("files: {}", plan.file_paths.join(", "));
    }
    for warning in &plan.sensitivity_warnings {
        println!("warning: {warning}");
    }
    if plan.known_public_receipts > 0 {
        println!(
            "warning: {} prior public publication receipt(s) exist",
            plan.known_public_receipts
        );
    }
    if plan.is_blocked() {
        println!("blocked: {}", plan.blocker_summary());
    }
}

/// The ambiguous legacy word `share` is preview-only and always refuses public
/// egress. Phase A requires the explicit `publish-public` operation.
pub(super) fn cmd_share(args: &[String]) -> ExitCode {
    let Some(target) = args.get(1).map(String::as_str) else {
        eprintln!("usage: concierge-plugin share <root|name> [--dry-run]");
        return ExitCode::from(2);
    };
    let mem = MemCli::new(workdir());

    match mem.build_egress_plan_for_target(target, EgressOperation::PublicPublish) {
        Ok(plan) => {
            print_egress_preview(&plan);
            eprintln!(
                "share refused: public publication must use `publish-public <root> --confirm-public`"
            );
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("share failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `share-private` never accepts a password or emits a capability in a terminal.
/// It previews the exact source and directs authorization to the Data Platter.
pub(super) fn cmd_share_private(args: &[String]) -> ExitCode {
    let Some(target) = args.get(1).map(String::as_str) else {
        eprintln!(
            "usage: concierge-plugin share-private <root|name> --namespace N --recipients A,B"
        );
        return ExitCode::from(2);
    };
    let Some(namespace) = flag_value(args, "--namespace") else {
        eprintln!("share-private failed: --namespace is required");
        return ExitCode::from(2);
    };
    let Some(recipients) = flag_value(args, "--recipients") else {
        eprintln!("share-private failed: --recipients is required");
        return ExitCode::from(2);
    };
    let recipients = recipients
        .split(',')
        .map(str::trim)
        .filter(|recipient| !recipient.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mem = MemCli::new(workdir());
    match mem.build_encrypt_and_share_plan(target, &namespace, &recipients) {
        Ok(plan) => {
            print_egress_preview(&plan.source);
            println!("private namespace: {}", plan.destination_namespace);
            println!("recipients: {}", plan.recipients.join(", "));
            if plan.source.is_blocked() {
                eprintln!(
                    "Private sharing blocked: this root is locked local plaintext.\nOpen the Data Platter and choose Convert to encrypted private and share."
                );
            } else {
                eprintln!(
                    "Private sharing requires password authorization from the Data Platter. Open it and choose Convert to encrypted private and share."
                );
            }
            ExitCode::from(2)
        }
        Err(error) => {
            eprintln!("share-private failed: {error}");
            ExitCode::FAILURE
        }
    }
}

pub(super) fn cmd_publish_public(args: &[String]) -> ExitCode {
    let Some(target) = args.get(1).map(String::as_str) else {
        eprintln!("usage: concierge-plugin publish-public <root|name> --confirm-public");
        return ExitCode::from(2);
    };
    let mem = MemCli::new(workdir());
    let plan = match mem.build_egress_plan_for_target(target, EgressOperation::PublicPublish) {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("publish-public failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    print_egress_preview(&plan);
    println!(
        "IRREVERSIBLE PUBLICATION WARNING: standard Kubo is public-networked unless explicitly isolated."
    );
    if !args.iter().any(|arg| arg == "--confirm-public") {
        eprintln!("publish-public refused: review the manifest and add --confirm-public");
        return ExitCode::from(2);
    }
    match mem.publish_public(&plan) {
        Ok(receipt) => {
            println!(
                "published {} via {} - fetch: {}",
                receipt.root, receipt.backend, receipt.gateway_url
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("publish-public failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_backend_info(info: &BackendInfo) {
    println!(
        "{}\t{}\t{}",
        info.name,
        info.blurb,
        info.requirements_summary()
    );
}

/// `backend list` / `backend show <name>` / `backend add <name>` — inspect and
/// configure the publishing backends compiled into the plugin.
pub(super) fn cmd_backend(args: &[String]) -> ExitCode {
    let mem = MemCli::new(workdir());
    match args.get(1).map(String::as_str) {
        Some("list") => match mem.list_backends() {
            Ok(backends) if backends.is_empty() => {
                println!("no backends compiled in");
                ExitCode::SUCCESS
            }
            Ok(backends) => {
                for backend in backends {
                    print_backend_info(&backend);
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("backend list failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some("show") => {
            let Some(name) = args.get(2).map(String::as_str) else {
                eprintln!("usage: concierge-plugin backend show <name>");
                return ExitCode::from(2);
            };
            match mem.list_backends() {
                Ok(backends) => match backends.into_iter().find(|b| b.name == name) {
                    Some(backend) => {
                        print_backend_info(&backend);
                        ExitCode::SUCCESS
                    }
                    None => {
                        eprintln!("backend show failed: unknown backend `{name}`");
                        ExitCode::from(2)
                    }
                },
                Err(e) => {
                    eprintln!("backend show failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("add") => {
            let Some(name) = args.get(2).map(String::as_str) else {
                eprintln!("usage: concierge-plugin backend add <name>");
                return ExitCode::from(2);
            };
            match mem.add_backend(name) {
                Ok(()) => {
                    println!("configured backend `{name}`");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("backend add failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("usage: concierge-plugin backend <list | show <name> | add <name>>");
            ExitCode::from(2)
        }
    }
}

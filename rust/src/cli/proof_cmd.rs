pub(crate) fn cmd_proof(args: &[String]) {
    let project_root = super::common::detect_project_root(args);

    let mut format: Option<String> = None;
    let mut write = true;
    let mut filename: Option<String> = None;
    let mut max_evidence: Option<usize> = None;
    let mut max_ledger_files: Option<usize> = None;

    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--format=") {
            format = Some(v.to_string());
            continue;
        }
        if a == "--format" {
            if let Some(v) = it.peek()
                && !v.starts_with("--")
            {
                format = Some((*v).clone());
                it.next();
            }
            continue;
        }
        if a == "--json" {
            format = Some("json".to_string());
            continue;
        }
        if a == "--summary" {
            format = Some("summary".to_string());
            continue;
        }
        if a == "--no-write" {
            write = false;
            continue;
        }
        if let Some(v) = a.strip_prefix("--filename=") {
            filename = Some(v.to_string());
            continue;
        }
        if a == "--filename" {
            if let Some(v) = it.peek()
                && !v.starts_with("--")
            {
                filename = Some((*v).clone());
                it.next();
            }
            continue;
        }
        if let Some(v) = a.strip_prefix("--max-evidence=") {
            max_evidence = v.parse::<usize>().ok();
            continue;
        }
        if a == "--max-evidence" {
            if let Some(v) = it.peek()
                && !v.starts_with("--")
            {
                max_evidence = (*v).parse::<usize>().ok();
                it.next();
            }
            continue;
        }
        if let Some(v) = a.strip_prefix("--max-ledger-files=") {
            max_ledger_files = v.parse::<usize>().ok();
            continue;
        }
        if a == "--max-ledger-files"
            && let Some(v) = it.peek()
            && !v.starts_with("--")
        {
            max_ledger_files = (*v).parse::<usize>().ok();
            it.next();
        }
    }

    let sources = crate::core::context_proof::ProofSources {
        project_root: Some(project_root.clone()),
        ..Default::default()
    };

    match crate::tools::ctx_proof::handle_export(
        &project_root,
        format.as_deref(),
        write,
        filename.as_deref(),
        max_evidence,
        max_ledger_files,
        sources,
    ) {
        Ok(out) => println!("{out}"),
        Err(e) => {
            eprintln!("ERROR: {e}");
            eprintln!(
                "Usage: lean-ctx proof [--format json|summary|both] [--no-write] [--filename <name>] [--max-evidence <n>] [--max-ledger-files <n>] [--root <path>]\n\
                 Examples:\n\
                   lean-ctx proof\n\
                   lean-ctx proof --summary\n\
                   lean-ctx proof --no-write --format json\n"
            );
            std::process::exit(2);
        }
    }
}

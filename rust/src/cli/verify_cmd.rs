pub(crate) fn cmd_verify(args: &[String]) {
    let mut format: Option<String> = None;

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
        }
    }

    match crate::tools::ctx_verify::handle_stats(format.as_deref()) {
        Ok(out) => println!("{out}"),
        Err(e) => {
            eprintln!("ERROR: {e}");
            eprintln!(
                "Usage: lean-ctx verify [--format summary|json|both] [--json]\n\
                 Examples:\n\
                   lean-ctx verify\n\
                   lean-ctx verify --json\n"
            );
            std::process::exit(2);
        }
    }
}

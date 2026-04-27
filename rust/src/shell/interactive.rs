use std::io::{self, BufRead, Write};

use crate::core::stats;

pub fn interactive() {
    let real_shell = super::platform::detect_shell();

    eprintln!(
        "lean-ctx shell v{} (wrapping {real_shell})",
        env!("CARGO_PKG_VERSION")
    );
    eprintln!("All command output is automatically compressed.");
    eprintln!("Type 'exit' to quit.\n");

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        let _ = write!(stdout, "lean-ctx> ");
        let _ = stdout.flush();

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }

        let cmd = line.trim();
        if cmd.is_empty() {
            continue;
        }
        if cmd == "exit" || cmd == "quit" {
            break;
        }
        if cmd == "gain" {
            println!("{}", stats::format_gain());
            continue;
        }

        let exit_code = super::exec::exec(cmd);

        if exit_code != 0 {
            let _ = writeln!(stdout, "[exit: {exit_code}]");
        }
    }
}

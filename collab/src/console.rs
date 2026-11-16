//! Small helpers for the command-line interface: showing and reading codes.

use std::io::{BufRead, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::code::{self, Code};

/// Print a code and try to copy it to the clipboard (Windows' built-in `clip` tool).
pub fn show_code(label: &str, text: &str) {
    println!("\n===== {label} CODE =====");
    // The `CODE:` prefix makes it easy for scripts (and our tests) to find the line.
    println!("CODE: {text}");
    println!("========================");
    let copied = std::process::Command::new("clip")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child.stdin.take().expect("stdin is piped").write_all(text.as_bytes())?;
            child.wait()
        })
        .map(|status| status.success())
        .unwrap_or(false);
    if copied {
        println!("(copied to your clipboard)");
    }
}

/// Ask for a code until a valid one is pasted.
pub fn read_code(prompt: &str) -> Result<Code, Box<dyn std::error::Error>> {
    let stdin = std::io::stdin();
    loop {
        print!("{prompt}");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            return Err("no code entered".into());
        }
        // Accept the line with or without the "CODE: " label.
        let line = line.trim().trim_start_matches("CODE:").trim();
        match code::decode(line) {
            Ok(code) => return Ok(code),
            Err(e) => println!("Couldn't use that code: {e}. Try again."),
        }
    }
}

/// Returns a flag that becomes `true` once Enter is pressed (or input closes).
pub fn stop_on_enter() -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        flag.store(true, Ordering::Relaxed);
    });
    stop
}

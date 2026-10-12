use std::io::IsTerminal;
use std::io::Write;

/// Lowercase machine-name-safe identifier: `[^a-z0-9-]` → `-`, no repeated
/// or edge dashes.
pub fn ident(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        let c = if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' };
        if !(out.ends_with('-') && c == '-') {
            out.push(c);
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() { "default".into() } else { trimmed }
}

fn tty() -> bool {
    std::io::stdout().is_terminal()
}

fn wrap(code: &str, s: &str) -> String {
    if tty() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String {
    wrap("1", s)
}
pub fn green(s: &str) -> String {
    wrap("32", s)
}
pub fn yellow(s: &str) -> String {
    wrap("33", s)
}
pub fn red(s: &str) -> String {
    wrap("31", s)
}
pub fn dim(s: &str) -> String {
    wrap("2", s)
}

pub fn ok(s: &str) {
    println!("{}", green(&format!("✓ {s}")));
}

pub fn warn(s: &str) {
    eprintln!("{}", yellow(&format!("! {s}")));
}

/// Colored machine state for tables.
pub fn state_cell(state: &str) -> String {
    match state {
        "running" => green(state),
        "paused" => yellow(state),
        s => dim(s),
    }
}

/// Simple y/N confirmation; `yes` short-circuits. Refuses to proceed on EOF.
pub fn confirm(prompt: &str, yes: bool) -> std::io::Result<bool> {
    if yes {
        return Ok(true);
    }
    print!("{prompt} [yN] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        return Ok(false);
    }
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

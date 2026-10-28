//! Terminal output. Every step announces itself, so a failure is always read against the last
//! thing the tool said it was doing.

use std::io::IsTerminal;

fn paint(code: &str, text: &str) -> String {
    if std::io::stderr().is_terminal() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

/// A step starting.
pub fn step(msg: impl AsRef<str>) {
    eprintln!("{} {}", paint("1;36", "==>"), msg.as_ref());
}

/// A detail under the current step.
pub fn info(msg: impl AsRef<str>) {
    eprintln!("    {}", msg.as_ref());
}

/// A recorded value: `name  value`.
pub fn kv(name: &str, value: impl std::fmt::Display) {
    eprintln!("    {name:<28} {value}");
}

/// Something worth knowing that does not stop the run.
pub fn warn(msg: impl AsRef<str>) {
    eprintln!("{} {}", paint("1;33", "warn"), msg.as_ref());
}

pub fn ok(msg: impl AsRef<str>) {
    eprintln!("  {}   {}", paint("32", "ok"), msg.as_ref());
}

pub fn bad(msg: impl AsRef<str>) {
    eprintln!("  {}  {}", paint("31", "BAD"), msg.as_ref());
}

pub fn error(msg: impl AsRef<str>) {
    eprintln!("{} {}", paint("1;31", "error"), msg.as_ref());
}

// ---------------------------------------------------------------- a partly done run

/// Work a command had to leave undone, each item said as it happened. Exits 3 rather than 1,
/// so a partial run is never mistaken for a finished one or for a hard failure.
#[derive(Debug)]
pub struct Incomplete(pub Vec<String>);

impl std::fmt::Display for Incomplete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} item(s) left undone; fix the cause and run it again (finished steps are skipped):",
            self.0.len()
        )?;
        for item in &self.0 {
            write!(f, "\n  - {item}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Incomplete {}

//! The error a command returns when it finished some of its work and not the rest.

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

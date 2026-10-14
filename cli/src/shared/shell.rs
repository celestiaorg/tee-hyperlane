//! External programs: docker, nix, phala, celestia-appd, forge, go, cargo, npm.
//!
//! Every call names what it was for. A failure reports that, the command line, the exit status
//! and the end of its output, so nothing fails as a bare "exit 1".

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

pub struct Cmd {
    program: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    envs: Vec<(String, String)>,
    stdin: Option<Vec<u8>>,
    what: String,
}

impl Cmd {
    /// `what` is the purpose, in words: it leads the error if the command fails.
    pub fn new(what: impl Into<String>, program: impl AsRef<std::ffi::OsStr>) -> Self {
        Self {
            program: program.as_ref().to_string_lossy().into_owned(),
            args: Vec::new(),
            cwd: None,
            envs: Vec::new(),
            stdin: None,
            what: what.into(),
        }
    }

    pub fn arg(mut self, a: impl AsRef<std::ffi::OsStr>) -> Self {
        self.args.push(a.as_ref().to_string_lossy().into_owned());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        for a in args {
            self = self.arg(a);
        }
        self
    }

    pub fn cwd(mut self, dir: impl AsRef<Path>) -> Self {
        self.cwd = Some(dir.as_ref().to_path_buf());
        self
    }

    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.envs.push((k.into(), v.into()));
        self
    }

    pub fn stdin(mut self, input: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(input.into());
        self
    }

    fn command(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.args);
        if let Some(dir) = &self.cwd {
            c.current_dir(dir);
        }
        for (k, v) in &self.envs {
            c.env(k, v);
        }
        c
    }

    fn line(&self) -> String {
        let mut s = self.program.clone();
        for a in &self.args {
            s.push(' ');
            if a.contains(' ') || a.is_empty() {
                s.push_str(&format!("'{a}'"));
            } else {
                s.push_str(a);
            }
        }
        if s.len() > 800 {
            s.truncate(800);
            s.push_str(" ...");
        }
        s
    }

    fn spawn_error(&self, e: std::io::Error) -> anyhow::Error {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!(
                "{}: `{}` is not installed or not on PATH (needed for: {})",
                self.what,
                self.program,
                self.line()
            )
        } else {
            anyhow::anyhow!("{}: could not start `{}`: {e}", self.what, self.line())
        }
    }

    /// Run with output captured. Returns stdout; on failure, the error carries the tail of
    /// both streams.
    pub fn output(&self) -> Result<String> {
        let (status, stdout, stderr) = self.capture()?;
        if !status.success() {
            bail!(
                "{}: `{}` exited with {}\n{}",
                self.what,
                self.line(),
                code(&status),
                tail(&format!("{stdout}\n{stderr}"), 30)
            );
        }
        Ok(stdout)
    }

    /// Run with output captured, and hand back the status rather than failing on it. For the
    /// few callers that must tell "no" from "broken" (a query for something absent, say).
    pub fn capture(&self) -> Result<(std::process::ExitStatus, String, String)> {
        let mut c = self.command();
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        c.stdin(if self.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let mut child = c.spawn().map_err(|e| self.spawn_error(e))?;
        if let Some(input) = &self.stdin {
            use std::io::Write;
            child
                .stdin
                .take()
                .context("stdin pipe")?
                .write_all(input)
                .with_context(|| format!("{}: writing to the stdin of `{}`", self.what, self.program))?;
        }
        let out = child
            .wait_with_output()
            .with_context(|| format!("{}: waiting for `{}`", self.what, self.line()))?;
        Ok((
            out.status,
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ))
    }

    /// Run with the terminal attached, for long builds whose progress is worth seeing.
    pub fn run(&self) -> Result<()> {
        let mut c = self.command();
        c.stdin(if self.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::inherit()
        });
        let status = c.status().map_err(|e| self.spawn_error(e))?;
        if !status.success() {
            bail!(
                "{}: `{}` exited with {} (its output is above)",
                self.what,
                self.line(),
                code(&status)
            );
        }
        Ok(())
    }
}

fn code(status: &std::process::ExitStatus) -> String {
    match status.code() {
        Some(c) => format!("status {c}"),
        None => "a signal".into(),
    }
}

/// The last `n` non-empty lines of `s`.
pub fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Whether `program` can be found on PATH.
pub fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

/// The first JSON value in `raw`. CLIs print progress before their JSON, and wrap it
/// differently per command, so the document is found rather than assumed to start at byte 0.
pub fn first_json(raw: &str) -> Option<serde_json::Value> {
    for (i, c) in raw.char_indices() {
        if c == '{' || c == '[' {
            let mut de = serde_json::Deserializer::from_str(&raw[i..]).into_iter::<serde_json::Value>();
            if let Some(Ok(v)) = de.next() {
                return Some(v);
            }
        }
    }
    None
}

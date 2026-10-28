//! Where everything lives, and the settings the operator provides.
//!
//! Settings come from the process environment first and `devnet/.env` second, so a one-off
//! `FOO=x teeism ...` overrides the file. `devnet/.env` is also the relayer unit's
//! EnvironmentFile, so it keeps the syntax systemd reads: `KEY=value`, `#` comments, no `export`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::shared::record::Deployment;

pub struct Ctx {
    pub repo: PathBuf,
    pub state_dir: PathBuf,
    env_file: PathBuf,
    file_env: BTreeMap<String, String>,
}

impl Ctx {
    /// Find the repository from the working directory upwards, so the installed binary works
    /// from anywhere inside the checkout. TEEISM_REPO overrides.
    pub fn load() -> Result<Self> {
        let repo = match std::env::var_os("TEEISM_REPO") {
            Some(p) => PathBuf::from(p),
            None => {
                let cwd = std::env::current_dir().context("reading the working directory")?;
                cwd.ancestors()
                    .find(|d| d.join("deploy/images.lock").is_file() && d.join("tee-hyperlane").is_dir())
                    .map(Path::to_path_buf)
                    .context(
                        "not inside a tee-hyperlane checkout (no deploy/images.lock above the working directory); \
                         cd into the repository or set TEEISM_REPO",
                    )?
            }
        };
        let devnet = repo.join("devnet");
        let state_dir = std::env::var_os("STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| devnet.join(".state"));
        let env_file = std::env::var_os("ENV_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| devnet.join(".env"));
        let file_env = if env_file.is_file() {
            parse_env(
                &std::fs::read_to_string(&env_file)
                    .with_context(|| format!("reading {}", env_file.display()))?,
            )
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            repo,
            state_dir,
            env_file,
            file_env,
        })
    }

    /// A setting, if set and non-empty.
    pub fn var(&self, key: &str) -> Option<String> {
        std::env::var(key)
            .ok()
            .or_else(|| self.file_env.get(key).cloned())
            .filter(|v| !v.trim().is_empty())
    }

    pub fn var_or(&self, key: &str, default: &str) -> String {
        self.var(key).unwrap_or_else(|| default.to_string())
    }

    /// A setting that has to be there, with what to do about it if it is not.
    pub fn require(&self, key: &str, why: &str) -> Result<String> {
        self.var(key)
            .with_context(|| format!("{key} is not set ({why}). Put it in {}", self.env_file.display()))
    }

    /// Record a generated value in devnet/.env, replacing the line if it is already there.
    /// Values the tool mints that must outlive a teardown go back to the file the operator
    /// already keeps, never under .state.
    pub fn set_env(&mut self, key: &str, value: &str) -> Result<()> {
        let existing = std::fs::read_to_string(&self.env_file).unwrap_or_default();
        let mut out = String::new();
        if existing.is_empty() {
            out.push_str("# Written by teeism. See devnet/.env.example.\n");
        }
        for line in existing.lines() {
            if !line.starts_with(&format!("{key}=")) {
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push_str(&format!("{key}=\"{value}\"\n"));
        write_private(&self.env_file, out.as_bytes())?;
        self.file_env.insert(key.to_string(), value.to_string());
        Ok(())
    }

    pub fn devnet(&self) -> PathBuf {
        self.repo.join("devnet")
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.state_dir.join("bin")
    }

    pub fn appd(&self) -> PathBuf {
        self.bin_dir().join("celestia-appd")
    }

    pub fn celestia_home(&self) -> PathBuf {
        self.state_dir.join("celestia")
    }

    pub fn coprocessor_bin(&self) -> PathBuf {
        self.repo.join("tee-hyperlane/target/release/tee-hyperlane")
    }

    pub fn coprocessor_config(&self) -> PathBuf {
        self.state_dir.join("coprocessor.toml")
    }

    pub fn contracts_dir(&self) -> PathBuf {
        self.repo.join("tee-hyperlane/contracts")
    }

    pub fn celestia_app_dir(&self) -> PathBuf {
        self.var("CELESTIA_APP_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.repo.join("../celestia-app-local"))
    }

    fn deployment_file(&self) -> PathBuf {
        self.state_dir.join("deployment.json")
    }

    /// What this deployment has created so far. Empty before the first step.
    pub fn deployment(&self) -> Result<Deployment> {
        let path = self.deployment_file();
        if !path.is_file() {
            return Ok(Deployment::default());
        }
        let raw = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("{} is not a valid deployment record", path.display()))
    }

    /// Record progress. Written after every change that created something, so a run that
    /// stops anywhere resumes from what exists rather than redeploying it.
    pub fn save(&self, d: &Deployment) -> Result<()> {
        std::fs::create_dir_all(&self.state_dir)
            .with_context(|| format!("creating {}", self.state_dir.display()))?;
        let json = serde_json::to_string_pretty(d)?;
        let path = self.deployment_file();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    pub fn env_file(&self) -> &Path {
        &self.env_file
    }
}

/// Write a file only its owner can read, creating its directory.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restricting {} to its owner", path.display()))?;
    }
    Ok(())
}

fn parse_env(raw: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(v);
        out.insert(k.trim().to_string(), v.to_string());
    }
    out
}

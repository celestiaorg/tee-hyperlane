//! Editing a node's TOML config in place, keeping its comments.

use anyhow::{bail, Context, Result};
use std::path::Path;

/// Set `key` under the dotted `table` path ("" for the top level) in a TOML file, failing if
/// the key is not already there: a renamed setting must stop the run, not be silently added
/// under a name the node ignores.
pub fn set_toml(path: &Path, table: &str, key: &str, value: toml_edit::Item) -> Result<()> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = raw.parse().with_context(|| format!("{} is not TOML", path.display()))?;
    let mut t = doc.as_table_mut();
    if !table.is_empty() {
        for part in table.split('.') {
            t = t
                .get_mut(part)
                .and_then(|t| t.as_table_mut())
                .with_context(|| format!("{} has no [{table}] table", path.display()))?;
        }
    }
    if !t.contains_key(key) {
        bail!("{} has no `{key}` under [{table}]; this version names it differently", path.display());
    }
    t[key] = value;
    std::fs::write(path, doc.to_string()).with_context(|| format!("writing {}", path.display()))
}

pub fn tv(v: impl Into<toml_edit::Value>) -> toml_edit::Item {
    toml_edit::Item::Value(v.into())
}

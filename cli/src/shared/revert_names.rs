//! Names for custom Solidity errors. A revert carries only a 4-byte selector; the compiled
//! ABIs under each Foundry `out/` directory say which error it is, so a failure reads as
//! `Duplicate_Collateral()` rather than `0x...`.

use alloy::primitives::keccak256;
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

static BOOK: OnceLock<HashMap<[u8; 4], String>> = OnceLock::new();

/// Read every error out of the artifacts in these Foundry projects. Missing directories are
/// skipped: the book only ever adds names, it never stops a run.
pub fn load(projects: &[&Path]) {
    let mut book = HashMap::new();
    for project in projects {
        let Ok(dirs) = std::fs::read_dir(project.join("out")) else {
            continue;
        };
        for dir in dirs.flatten() {
            let Ok(files) = std::fs::read_dir(dir.path()) else {
                continue;
            };
            for f in files.flatten() {
                if f.path().extension().is_some_and(|e| e == "json") {
                    add_artifact(&mut book, &f.path());
                }
            }
        }
    }
    let _ = BOOK.set(book);
}

fn add_artifact(book: &mut HashMap<[u8; 4], String>, path: &Path) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return;
    };
    let Some(abi) = v["abi"].as_array() else { return };
    for item in abi.iter().filter(|i| i["type"] == "error") {
        let Some(name) = item["name"].as_str() else {
            continue;
        };
        let types: Vec<String> = item["inputs"]
            .as_array()
            .map(|ins| ins.iter().map(canonical_type).collect())
            .unwrap_or_default();
        let sig = format!("{name}({})", types.join(","));
        let sel = keccak256(sig.as_bytes());
        book.insert([sel[0], sel[1], sel[2], sel[3]], sig);
    }
}

fn canonical_type(input: &serde_json::Value) -> String {
    let ty = input["type"].as_str().unwrap_or("");
    match ty.strip_prefix("tuple") {
        Some(suffix) => {
            let inner: Vec<String> = input["components"]
                .as_array()
                .map(|c| c.iter().map(canonical_type).collect())
                .unwrap_or_default();
            format!("({}){suffix}", inner.join(","))
        }
        None => ty.to_string(),
    }
}

/// The name of the first known error selector in `text` (an RPC error message carrying
/// revert data), if any.
pub fn name_in(text: &str) -> Option<String> {
    let book = BOOK.get()?;
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(off) = text[i..].find("0x") {
        let start = i + off + 2;
        let hex_len = bytes[start..]
            .iter()
            .take_while(|b| b.is_ascii_hexdigit())
            .count();
        if hex_len >= 8 {
            if let Ok(sel) = hex::decode(&text[start..start + 8]) {
                if let Some(name) = book.get(&[sel[0], sel[1], sel[2], sel[3]]) {
                    return Some(name.clone());
                }
            }
        }
        i = start + hex_len.max(1);
        if i >= text.len() {
            break;
        }
    }
    None
}

//! What the built-in collectors `files`, `git-history` and `github` share: their params, which
//! a built-in reads from `PRECALL_PARAM_*` since its argv is never expanded (spec §6.2, §6.3),
//! and child processes run by argument vector.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::Path;
use std::process::Command;

/// Param `name` as the core passed it: a string as it is, any other value as JSON.
pub fn param(name: &str) -> Value {
    let var = format!(
        "PRECALL_PARAM_{}",
        name.to_ascii_uppercase().replace('-', "_")
    );
    let text = std::env::var(var).unwrap_or_default();
    serde_json::from_str::<Value>(&text)
        .ok()
        .filter(|v| !v.is_string())
        .unwrap_or(Value::String(text))
}

/// String param `name`.
pub fn param_str(name: &str) -> String {
    match param(name) {
        Value::String(s) => s,
        v => v.to_string(),
    }
}

/// The `path` param, which must name an existing directory or file.
pub fn path_param() -> Result<std::path::PathBuf> {
    let p = param_str("path");
    if p.is_empty() {
        bail!("param `path` is empty: set it to the source's absolute path");
    }
    let path = Path::new(&p);
    if !path.is_absolute() {
        bail!("param `path` `{p}` is not absolute");
    }
    path.canonicalize()
        .with_context(|| format!("param `path` `{p}`"))
}

/// Runs `program args` in `dir` and returns its stdout; a non-zero exit is an error quoting
/// the last line of its stderr.
pub fn output(program: &str, dir: &Path, args: &[&str]) -> Result<String> {
    output_codes(program, dir, args, &[0])
}

/// [`output`] that takes each exit code in `ok` as success.
pub fn output_codes(program: &str, dir: &Path, args: &[&str], ok: &[i32]) -> Result<String> {
    let o = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !o.status.code().is_some_and(|c| ok.contains(&c)) {
        let err = String::from_utf8_lossy(&o.stderr);
        let last = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        bail!("{program} {} failed: {last}", args.first().unwrap_or(&""));
    }
    String::from_utf8(o.stdout).with_context(|| format!("{program} printed text that is not UTF-8"))
}

/// `git args` in `dir`.
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    output("git", dir, args)
}

/// The text on one line (a node's title and body are plain text without line breaks, §3.1),
/// cut to at most `max` characters, ending with `…` when cut.
pub fn cut(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max {
        return text;
    }
    let mut s: String = text.chars().take(max - 1).collect();
    s.push('…');
    s
}

/// Whether `s` looks like an abbreviated or full git object id (SHA-1 or SHA-256).
pub fn is_sha(s: &str) -> bool {
    (4..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The refs of a resolve input (spec §6.4).
pub fn refs_of(input: &str) -> Result<Vec<String>> {
    let v: Value = serde_json::from_str(input).context("the resolve input does not decode")?;
    Ok(v["refs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| r.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default())
}

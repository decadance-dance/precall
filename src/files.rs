//! The built-in `files` collector (spec §10): one file, or the text files of a folder that
//! match `include` and not `exclude`, read by an agent through the collector's runner. Its
//! script entrypoints fingerprint the files, fetch the list the agent reads (with the commit
//! to cite) and resolve references back to text.

use crate::glob;
use crate::sources::{self, git, is_sha};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const MANIFEST: &str = r#"id = "files"
name = "A repository's or a folder's files, read by an agent"
version = "0.1.0"
min_rrecall = "0.1.0"
kind = "collector"

[collector]
# limit: the union of both presets' branches; the core does not narrow it to the preset's,
# the prompt tells the agent to propose only into `fetched.branches`.
scope = ["layout", "how-it-works", "decisions", "blocked", "people"]
source_kinds = ["{ref_kind}"]
fingerprint = ["rrecall", "builtin", "files", "fingerprint"]
fetch = ["rrecall", "builtin", "files", "fetch"]
prompt = "files.md"
resolve = ["rrecall", "builtin", "files", "resolve"]

[params]
path = ""
git = false
preset = "codebase"
ref_kind = "file"
include = ["**"]
exclude = []
"#;

/// The prompt files of this built-in, as (plugin id, file name, text) (spec §6.2).
pub const PROMPTS: [(&str, &str, &str); 1] =
    [("files", "files.md", include_str!("collectors/files.md"))];

/// A resolved text longer than this is cut, so one big file cannot flood a judge's input.
const RESOLVE_MAX: usize = 100_000;

/// What git's own heuristic reads to tell text from binary.
const SNIFF: usize = 8000;

pub fn run(entry: &str, input: &str) -> Result<String> {
    let s = Scope::from_params()?;
    match entry {
        "fingerprint" => s.fingerprint(),
        "fetch" => s.fetch(),
        "resolve" => s.resolve(input),
        _ => Err(crate::error::usage(format!(
            "`files` has no entrypoint `{entry}`"
        ))),
    }
}

/// The preset's branches (spec §10).
fn branches(preset: &str) -> Result<[&'static str; 3]> {
    match preset {
        "codebase" => Ok(["layout", "how-it-works", "decisions"]),
        "notes" => Ok(["decisions", "blocked", "people"]),
        _ => bail!("param `preset` `{preset}` is neither `codebase` nor `notes`"),
    }
}

struct Scope {
    /// The folder the paths are relative to.
    root: PathBuf,
    /// Set when `path` names one file: only it is in scope.
    only: Option<String>,
    git: bool,
    preset: String,
    kind: String,
    include: Vec<String>,
    exclude: Vec<String>,
}

fn strings(name: &str) -> Result<Vec<String>> {
    match sources::param(name) {
        Value::Array(a) => a
            .into_iter()
            .map(|v| match v {
                Value::String(s) => Ok(s),
                _ => bail!("param `{name}` holds a value that is not a string"),
            })
            .collect(),
        _ => bail!("param `{name}` is not a list of globs"),
    }
}

impl Scope {
    fn from_params() -> Result<Scope> {
        let path = sources::path_param()?;
        let (root, only) = if path.is_dir() {
            (path, None)
        } else {
            let name = path
                .file_name()
                .context("param `path` names no file")?
                .to_string_lossy()
                .into_owned();
            (
                path.parent()
                    .context("param `path` has no folder")?
                    .to_path_buf(),
                Some(name),
            )
        };
        let preset = sources::param_str("preset");
        branches(&preset)?;
        Ok(Scope {
            root,
            only,
            git: sources::param("git") == Value::Bool(true),
            preset,
            kind: sources::param_str("ref_kind"),
            include: strings("include")?,
            exclude: strings("exclude")?,
        })
    }

    fn wanted(&self, rel: &str) -> bool {
        self.only.as_deref().is_none_or(|o| o == rel)
            && self.include.iter().any(|g| glob::matches(g, rel))
            && !self.exclude.iter().any(|g| glob::matches(g, rel))
    }

    /// The matched text files, sorted, each with what identifies its content: the blob id at
    /// `HEAD` with git, else a hash of the file.
    fn matched(&self) -> Result<Vec<(String, String)>> {
        let mut out = if self.git {
            self.matched_at_head()?
        } else {
            let mut out = Vec::new();
            self.walk(&self.root, &mut out)?;
            out
        };
        out.sort();
        Ok(out)
    }

    fn matched_at_head(&self) -> Result<Vec<(String, String)>> {
        let tree = git(&self.root, &["ls-tree", "-r", "-z", "HEAD"])?;
        // `git grep -I` lists the files at HEAD that hold text (an empty file holds none).
        // It exits 1 when no file matches.
        let text = sources::output_codes(
            "git",
            &self.root,
            &["grep", "-I", "-l", "-z", "-e", "", "HEAD", "--"],
            &[0, 1],
        )?;
        let text: std::collections::BTreeSet<&str> = text
            .split('\0')
            .filter_map(|p| p.strip_prefix("HEAD:"))
            .collect();
        let mut out = Vec::new();
        for entry in tree.split('\0').filter(|e| !e.is_empty()) {
            let Some((meta, rel)) = entry.split_once('\t') else {
                continue;
            };
            let f: Vec<&str> = meta.split(' ').collect();
            // Regular files only: no submodule, no symlink.
            if let ["100644" | "100755", "blob", id] = f[..]
                && text.contains(rel)
                && self.wanted(rel)
            {
                out.push((rel.to_string(), id.to_string()));
            }
        }
        Ok(out)
    }

    /// Without git: every text file under `dir`, hidden ones and symlinks left out.
    fn walk(&self, dir: &Path, out: &mut Vec<(String, String)>) -> Result<()> {
        for e in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let e = e?;
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let t = e.file_type()?;
            let path = e.path();
            if t.is_dir() {
                self.walk(&path, out)?;
            } else if t.is_file() {
                let rel = rel_of(&self.root, &path);
                if !self.wanted(&rel) {
                    continue;
                }
                let bytes =
                    std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
                if is_text(&bytes) {
                    out.push((rel, sha256_hex(&bytes)));
                }
            }
        }
        Ok(())
    }

    fn fingerprint(&self) -> Result<String> {
        Ok(self
            .matched()?
            .iter()
            .map(|(rel, id)| format!("{id}\t{rel}\n"))
            .collect())
    }

    fn commit(&self) -> Result<String> {
        Ok(git(&self.root, &["rev-parse", "--short=12", "HEAD"])?
            .trim()
            .to_string())
    }

    /// What the agent reads: the folder, the files in scope, how to cite them, the preset's
    /// branches.
    fn fetch(&self) -> Result<String> {
        let files: Vec<String> = self.matched()?.into_iter().map(|(rel, _)| rel).collect();
        let mut f = json!({
            "root": self.root.to_string_lossy(),
            "files": files,
            "preset": self.preset,
            "branches": branches(&self.preset)?,
        });
        // limit: the agent reads the working tree, which may differ from the commit it cites.
        let cite = if self.git {
            let c = self.commit()?;
            f["commit"] = json!(c);
            format!("{}:<path>#L<first>-L<last>@{c}", self.kind)
        } else {
            format!("{}:<path>#L<first>-L<last>", self.kind)
        };
        f["cite"] = json!(cite);
        Ok(f.to_string())
    }

    fn resolve(&self, input: &str) -> Result<String> {
        let mut texts = serde_json::Map::new();
        for r in sources::refs_of(input)? {
            let text = self.text_of(&r);
            texts.insert(r, text.map_or(Value::Null, Value::String));
        }
        Ok(json!({ "texts": texts }).to_string())
    }

    /// `<kind>:<path>[#L<a>[-L<b>]][@<commit>]`, the commit there exactly when `git` is set.
    fn text_of(&self, r: &str) -> Option<String> {
        let loc = r.strip_prefix(&self.kind)?.strip_prefix(':')?;
        let (loc, commit) = if self.git {
            let (l, c) = loc.rsplit_once('@')?;
            (l, Some(c).filter(|c| is_sha(c))?.into())
        } else {
            (loc, None)
        };
        let (rel, lines) = match loc.rsplit_once('#') {
            Some((p, l)) => (p, Some(line_range(l)?)),
            None => (loc, None),
        };
        if rel.is_empty()
            || rel.starts_with('/')
            || rel
                .split('/')
                .any(|s| s.is_empty() || s == "." || s == "..")
            || !self.wanted(rel)
        {
            return None;
        }
        let text = match commit {
            Some(c) => git(&self.root, &["show", &format!("{c}:./{rel}")]).ok()?,
            None => {
                let path = self.root.join(rel);
                if rel.split('/').any(|s| s.starts_with('.'))
                    || !path.canonicalize().ok()?.starts_with(&self.root)
                {
                    return None;
                }
                let bytes = std::fs::read(&path).ok()?;
                if !is_text(&bytes) {
                    return None;
                }
                String::from_utf8_lossy(&bytes).into_owned()
            }
        };
        let text = match lines {
            Some((a, b)) => {
                let picked: Vec<&str> = text.lines().skip(a - 1).take(b + 1 - a).collect();
                if picked.is_empty() {
                    return None;
                }
                picked.join("\n")
            }
            None => text,
        };
        Some(if text.chars().count() > RESOLVE_MAX {
            let mut t: String = text.chars().take(RESOLVE_MAX).collect();
            t.push_str("\n[cut by rrecall]");
            t
        } else {
            text
        })
    }
}

/// `L<a>` or `L<a>-L<b>`, 1-based, a ≤ b.
fn line_range(l: &str) -> Option<(usize, usize)> {
    let num = |s: &str| {
        s.strip_prefix('L')?
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
    };
    let (a, b) = match l.split_once('-') {
        Some((a, b)) => (num(a)?, num(b)?),
        None => (num(l)?, num(l)?),
    };
    (a <= b).then_some((a, b))
}

fn rel_of(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Git's heuristic: text holds no NUL byte in its first 8000 bytes.
fn is_text(bytes: &[u8]) -> bool {
    !bytes[..bytes.len().min(SNIFF)].contains(&0)
}

#[cfg(test)]
mod tests {
    use super::line_range;

    #[test]
    fn line_ranges_are_one_based_and_ordered() {
        assert_eq!(line_range("L4"), Some((4, 4)));
        assert_eq!(line_range("L2-L3"), Some((2, 3)));
        for bad in ["L0", "L3-L2", "4", "L2-3", "Lx", ""] {
            assert_eq!(line_range(bad), None, "{bad}");
        }
    }
}

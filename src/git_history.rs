//! The built-in `git-history` collector (spec §10): merged branches, tags and their release
//! notes, by script with no model. `done` at the granularity of §3.4: one node per tag on the
//! main line (a release); a repository with no tag yet gets one per branch merged into the main
//! line (a finished stream of work). References are `commit:<repo>@<sha>`.

use crate::node::{BODY_MAX, TITLE_MAX};
use crate::sources::{self, cut, git, is_sha};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

pub const MANIFEST: &str = r#"id = "git-history"
name = "Releases and merged branches from git history"
version = "0.1.0"
min_rrecall = "0.1.0"
kind = "collector"
requires = ["git"]

[collector]
scope = ["done"]
source_kinds = ["commit"]
fingerprint = ["rrecall", "builtin", "git-history", "fingerprint"]
command = ["rrecall", "builtin", "git-history", "collect"]
resolve = ["rrecall", "builtin", "git-history", "resolve"]
timeout = "2m"

[params]
path = ""
repo = ""
branch = ""
"#;

/// The length of the abbreviated commit ids in references.
const SHA_LEN: usize = 12;

pub fn run(entry: &str, input: &str) -> Result<String> {
    let dir = sources::path_param()?;
    let repo = repo_name(&dir)?;
    let branch = main_line(&dir);
    match entry {
        "fingerprint" => fingerprint(&dir, &branch),
        "collect" => collect(&dir, &repo, &branch, input),
        "resolve" => resolve(&dir, &repo, input),
        _ => Err(crate::error::usage(format!(
            "`git-history` has no entrypoint `{entry}`"
        ))),
    }
}

/// The `repo` param, else `owner/name` of a GitHub `origin`, else the directory's name.
fn repo_name(dir: &Path) -> Result<String> {
    let p = sources::param_str("repo");
    if !p.is_empty() {
        return Ok(p);
    }
    if let Some(r) = git(dir, &["remote", "get-url", "origin"])
        .ok()
        .and_then(|u| crate::commands::github_repo(u.trim()))
    {
        return Ok(r);
    }
    let top = git(dir, &["rev-parse", "--show-toplevel"])?;
    Path::new(top.trim())
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .context("the repository has no directory name: set param `repo`")
}

/// The `branch` param, else `origin/HEAD`, else `HEAD`.
fn main_line(dir: &Path) -> String {
    let p = sources::param_str("branch");
    if !p.is_empty() {
        return p;
    }
    match git(
        dir,
        &["rev-parse", "--verify", "-q", "refs/remotes/origin/HEAD"],
    ) {
        Ok(_) => "refs/remotes/origin/HEAD".into(),
        Err(_) => "HEAD".into(),
    }
}

/// Everything collect reads: the merges in the main line's history and its tags. A commit
/// that is neither does not change it.
fn fingerprint(dir: &Path, branch: &str) -> Result<String> {
    let merges = git(dir, &["log", "--merges", "--format=%H", branch, "--"])?;
    let tags = tags(dir, branch)?;
    Ok(format!("{merges}\n{tags:?}\n"))
}

#[derive(Debug)]
struct Tag {
    name: String,
    /// The tagged commit.
    commit: String,
    /// Seconds since the epoch: tags of one day keep their order.
    time: i64,
    date: String,
    /// The annotation without its signature; empty for a lightweight tag.
    notes: String,
}

/// The tags whose commit is on `branch`, oldest first (then by name), from one git call.
fn tags(dir: &Path, branch: &str) -> Result<Vec<Tag>> {
    let merged = format!("--merged={branch}");
    // Each record ends with NUL and a newline; notes hold newlines but never NUL.
    let list = git(
        dir,
        &[
            "for-each-ref",
            &merged,
            "--format=%(refname:strip=2)%00%(objecttype)%00%(objectname)%00%(*objectname)%00%(creatordate:unix)%00%(creatordate:short)%00%(contents:subject)%0a%0a%(contents:body)%00",
            "refs/tags",
        ],
    )?;
    let mut tags = Vec::new();
    for rec in list.split("\0\n").filter(|r| !r.trim().is_empty()) {
        let f: Vec<&str> = rec.split('\0').collect();
        let [name, kind, obj, peeled, unix, date, notes] = f[..] else {
            bail!("git for-each-ref printed an unexpected record");
        };
        // A lightweight tag's contents are its commit's message, not release notes.
        let (commit, notes) = if kind == "tag" {
            (peeled, notes.trim())
        } else {
            (obj, "")
        };
        tags.push(Tag {
            name: name.to_string(),
            commit: commit.to_string(),
            time: unix.parse().unwrap_or(0),
            date: date.to_string(),
            notes: notes.to_string(),
        });
    }
    tags.sort_by(|a, b| (a.time, &a.name).cmp(&(b.time, &b.name)));
    Ok(tags)
}

/// A merge commit: its id, date, the branch it merged and the rest of its message.
struct Merge {
    sha: String,
    date: String,
    branch: String,
    body: String,
}

/// The merges in `range`, oldest first; `first_parent` keeps only those into the main line.
fn merges(dir: &Path, range: &str, first_parent: bool) -> Result<Vec<Merge>> {
    let mut args = vec![
        "log",
        "--merges",
        "--reverse",
        "-z",
        "--format=%H%x00%cs%x00%B",
    ];
    if first_parent {
        args.push("--first-parent");
    }
    args.extend([range, "--"]);
    let out = git(dir, &args)?;
    let f: Vec<&str> = out.split('\0').collect();
    Ok(f.as_chunks::<3>()
        .0
        .iter()
        .map(|c| {
            let msg = c[2].trim();
            let (subject, rest) = msg.split_once('\n').unwrap_or((msg, ""));
            Merge {
                sha: c[0].trim().to_string(),
                date: c[1].to_string(),
                branch: branch_of(subject),
                body: rest.trim().to_string(),
            }
        })
        .collect())
}

/// The branch a merge commit's subject names, else the subject.
fn branch_of(subject: &str) -> String {
    if let Some(rest) = subject.strip_prefix("Merge pull request ")
        && let Some((_, from)) = rest.split_once(" from ")
    {
        // `owner/branch`: the branch may hold slashes itself.
        return from
            .split_once('/')
            .map_or(from, |(_, b)| b)
            .trim()
            .to_string();
    }
    for prefix in ["Merge branch '", "Merge remote-tracking branch '"] {
        if let Some(rest) = subject.strip_prefix(prefix)
            && let Some((b, _)) = rest.split_once('\'')
        {
            return b.strip_prefix("origin/").unwrap_or(b).to_string();
        }
    }
    subject.to_string()
}

fn reference(repo: &str, sha: &str) -> String {
    format!("commit:{repo}@{}", &sha[..SHA_LEN.min(sha.len())])
}

/// One `done` node as the history says it should read.
struct Want {
    key: String,
    title: String,
    body: String,
    date: String,
    source: String,
    reason: String,
}

fn wanted(dir: &Path, repo: &str, branch: &str) -> Result<Vec<Want>> {
    let tags = tags(dir, branch)?;
    if tags.is_empty() {
        return Ok(merges(dir, branch, true)?
            .into_iter()
            .map(|m| Want {
                key: format!("merge:{}", &m.sha[..SHA_LEN]),
                title: cut(&m.branch, TITLE_MAX),
                body: cut(&m.body, BODY_MAX),
                date: m.date,
                source: reference(repo, &m.sha),
                reason: format!("merged {}", m.branch),
            })
            .collect());
    }
    let mut out = Vec::new();
    let mut prev: Option<&Tag> = None;
    for t in &tags {
        let body = if !t.notes.is_empty() {
            t.notes.clone()
        } else {
            let range = match prev {
                Some(p) => format!("{}..{}", p.commit, t.commit),
                None => t.commit.clone(),
            };
            let mut names: Vec<String> = Vec::new();
            for m in merges(dir, &range, false)? {
                if !names.contains(&m.branch) {
                    names.push(m.branch);
                }
            }
            if names.is_empty() {
                String::new()
            } else {
                format!("Merged: {}.", names.join(", "))
            }
        };
        out.push(Want {
            key: format!("tag:{}", t.name),
            title: cut(&t.name, TITLE_MAX),
            body: cut(&body, BODY_MAX),
            date: t.date.clone(),
            source: reference(repo, &t.commit),
            reason: format!("tagged {}", t.name),
        });
        prev = Some(t);
    }
    Ok(out)
}

/// Adds for releases and merges the map lacks, updates where the history now reads otherwise;
/// never a removal, since history stays true. A later round withdraws what was sent back.
fn collect(dir: &Path, repo: &str, branch: &str, input: &str) -> Result<String> {
    let input: Value =
        serde_json::from_str(input).context("the collector input does not decode")?;
    if input["round"].as_u64().unwrap_or(1) > 1 {
        return Ok(json!({"proposals": []}).to_string());
    }
    // This collector's nodes by the reference they cite.
    let mut ours: BTreeMap<&str, &Value> = BTreeMap::new();
    for n in input["nodes"].as_array().into_iter().flatten() {
        for s in n["sources"].as_array().into_iter().flatten() {
            if let Some(s) = s.as_str() {
                ours.entry(s).or_insert(n);
            }
        }
    }
    let mut proposals = Vec::new();
    for w in wanted(dir, repo, branch)? {
        match ours.get(w.source.as_str()) {
            None => proposals.push(json!({
                "key": w.key, "op": "add", "parent": "done", "title": w.title, "body": w.body,
                "date": w.date, "state": "done", "sources": [w.source], "reason": w.reason,
            })),
            Some(n) if n["title"] != w.title.as_str() || n["body"] != w.body.as_str() => proposals
                .push(json!({
                    "key": w.key, "op": "update", "id": n["id"], "title": w.title,
                    "body": w.body, "sources": [w.source], "reason": w.reason,
                })),
            Some(_) => {}
        }
    }
    Ok(json!({ "proposals": proposals }).to_string())
}

/// A commit's message, author and date, and the annotation of each tag on it.
fn resolve(dir: &Path, repo: &str, input: &str) -> Result<String> {
    let mut texts = serde_json::Map::new();
    for r in sources::refs_of(input)? {
        let text = r
            .strip_prefix("commit:")
            .and_then(|l| l.rsplit_once('@'))
            .filter(|(rp, sha)| *rp == repo && is_sha(sha))
            .and_then(|(_, sha)| {
                let commit = git(
                    dir,
                    &[
                        "show",
                        "-s",
                        "--format=commit %H%nAuthor: %an%nDate: %cs%n%n%B",
                        sha,
                    ],
                )
                .ok()?;
                let tags = git(
                    dir,
                    &[
                        "tag",
                        "--points-at",
                        sha,
                        "--format=tag %(refname:strip=2)%0a%(contents)",
                    ],
                )
                .unwrap_or_default();
                Some(
                    format!("{}\n\n{}", commit.trim(), tags.trim())
                        .trim()
                        .to_string(),
                )
            });
        texts.insert(r, text.map_or(Value::Null, Value::String));
    }
    Ok(json!({ "texts": texts }).to_string())
}

#[cfg(test)]
mod tests {
    use super::branch_of;

    #[test]
    fn a_merge_subject_names_its_branch() {
        for (subject, want) in [
            (
                "Merge pull request #24 from acme/feature/7-runners",
                "feature/7-runners",
            ),
            ("Merge branch 'feature/lexer'", "feature/lexer"),
            ("Merge branch 'hotfix-1' into main", "hotfix-1"),
            ("Merge remote-tracking branch 'origin/develop'", "develop"),
            ("Squashed work", "Squashed work"),
        ] {
            assert_eq!(branch_of(subject), want, "{subject}");
        }
    }
}

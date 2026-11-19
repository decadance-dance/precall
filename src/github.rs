//! The built-in `github` collector (spec §10): a script fetches pull requests, issues and
//! releases through `gh`, the collector's runner proposes from them, and `gh` resolves the
//! references `gh-pr:<owner>/<repo>#<n>`, `gh-issue:<owner>/<repo>#<n>` and
//! `gh-release:<owner>/<repo>@<tag>`.

use crate::sources;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::Path;

pub const MANIFEST: &str = r#"id = "github"
name = "GitHub pull requests, issues and releases"
version = "0.1.0"
min_rrecall = "0.1.0"
kind = "collector"
requires = ["gh"]

[collector]
scope = ["done", "blocked", "decisions", "people"]
source_kinds = ["gh-pr", "gh-issue", "gh-release"]
fingerprint = ["rrecall", "builtin", "github", "fingerprint"]
fetch = ["rrecall", "builtin", "github", "fetch"]
prompt = "github.md"
resolve = ["rrecall", "builtin", "github", "resolve"]

[params]
repo = ""
limit = 100
"#;

/// A pull request's, issue's or release's body is cut to this many characters in the fetch;
/// resolve gives the whole text.
const BODY_CUT: usize = 2000;

/// A resolved text longer than this is cut.
const RESOLVE_MAX: usize = 100_000;

pub fn run(entry: &str, input: &str) -> Result<String> {
    let repo = sources::param_str("repo");
    if repo.split('/').count() != 2 || repo.split('/').any(str::is_empty) || repo.starts_with('-') {
        bail!("param `repo` `{repo}` is not `<owner>/<name>`");
    }
    match entry {
        "fingerprint" => fingerprint(&repo),
        "fetch" => fetch(&repo),
        "resolve" => resolve(&repo, input),
        _ => Err(crate::error::usage(format!(
            "`github` has no entrypoint `{entry}`"
        ))),
    }
}

fn gh(args: &[&str]) -> Result<Value> {
    let out = sources::output("gh", Path::new("/"), args)?;
    serde_json::from_str(&out).with_context(|| format!("gh {} printed no JSON", args[0]))
}

/// The most recently updated issue or pull request and the newest release: any change to
/// what fetch reads moves one of them.
fn fingerprint(repo: &str) -> Result<String> {
    let issues = gh(&[
        "api",
        &format!("repos/{repo}/issues?state=all&sort=updated&direction=desc&per_page=1"),
    ])?;
    let releases = gh(&["api", &format!("repos/{repo}/releases?per_page=1")])?;
    let first =
        |v: &Value, keys: &[&str]| -> Value { keys.iter().map(|k| v[0][*k].clone()).collect() };
    Ok(format!(
        "{}\n{}\n",
        first(&issues, &["number", "updated_at"]),
        first(&releases, &["id", "tag_name", "published_at"])
    ))
}

fn limit() -> String {
    match sources::param("limit") {
        Value::Number(n) => n.to_string(),
        _ => "100".into(),
    }
}

fn logins(v: &Value) -> Value {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["login"].as_str())
        .collect()
}

fn body(v: &Value) -> Value {
    json!(cut_keep_lines(v.as_str().unwrap_or(""), BODY_CUT))
}

/// Cut to `max` characters, line breaks kept (the agent reads it; it is not a node field).
fn cut_keep_lines(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

fn by_number(mut items: Vec<Value>) -> Vec<Value> {
    items.sort_by_key(|a| a["number"].as_u64());
    items
}

/// The pull requests, issues and releases the agent proposes from, newest `limit` of each.
fn fetch(repo: &str) -> Result<String> {
    let limit = limit();
    let list = |noun: &str, fields: &str| -> Result<Vec<Value>> {
        let v = gh(&[
            noun, "list", "-R", repo, "--state", "all", "--limit", &limit, "--json", fields,
        ])?;
        Ok(v.as_array().cloned().unwrap_or_default())
    };
    let pulls: Vec<Value> = list(
        "pr",
        "number,title,state,author,mergedAt,closedAt,updatedAt,body,labels",
    )?
    .iter()
    .map(|p| {
        json!({
            "number": p["number"], "title": p["title"], "state": p["state"],
            "author": p["author"]["login"], "merged_at": p["mergedAt"],
            "closed_at": p["closedAt"], "updated_at": p["updatedAt"],
            "labels": names(&p["labels"]), "body": body(&p["body"]),
        })
    })
    .collect();
    let issues: Vec<Value> = list(
        "issue",
        "number,title,state,stateReason,author,assignees,closedAt,updatedAt,body,labels",
    )?
    .iter()
    .map(|i| {
        json!({
            "number": i["number"], "title": i["title"], "state": i["state"],
            "state_reason": i["stateReason"], "author": i["author"]["login"],
            "assignees": logins(&i["assignees"]), "closed_at": i["closedAt"],
            "updated_at": i["updatedAt"], "labels": names(&i["labels"]),
            "body": body(&i["body"]),
        })
    })
    .collect();
    let releases: Vec<Value> = gh(&["api", &format!("repos/{repo}/releases?per_page={limit}")])?
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["draft"] != true)
        .map(|r| {
            json!({
                "tag": r["tag_name"], "name": r["name"], "published_at": r["published_at"],
                "prerelease": r["prerelease"], "body": body(&r["body"]),
            })
        })
        .collect();
    let mut releases = releases;
    releases.sort_by(|a, b| a["tag"].to_string().cmp(&b["tag"].to_string()));
    Ok(json!({
        "repo": repo,
        "cite": {
            "pr": format!("gh-pr:{repo}#<number>"),
            "issue": format!("gh-issue:{repo}#<number>"),
            "release": format!("gh-release:{repo}@<tag>"),
        },
        "pulls": by_number(pulls),
        "issues": by_number(issues),
        "releases": releases,
    })
    .to_string())
}

fn names(v: &Value) -> Value {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["name"].as_str())
        .collect()
}

fn resolve(repo: &str, input: &str) -> Result<String> {
    let mut texts = serde_json::Map::new();
    for r in sources::refs_of(input)? {
        let text = text_of(repo, &r);
        texts.insert(r, text.map_or(Value::Null, Value::String));
    }
    Ok(json!({ "texts": texts }).to_string())
}

/// The item as `gh` shows it, pretty JSON; `None` for another repository, a malformed
/// reference or what `gh` cannot show.
fn text_of(repo: &str, r: &str) -> Option<String> {
    let (kind, loc) = r.split_once(':')?;
    let v = match kind {
        "gh-pr" | "gh-issue" => {
            let (rp, n) = loc.rsplit_once('#')?;
            if rp != repo || n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let (noun, fields) = if kind == "gh-pr" {
                (
                    "pr",
                    "number,title,state,author,createdAt,mergedAt,closedAt,body,comments",
                )
            } else {
                (
                    "issue",
                    "number,title,state,stateReason,author,assignees,createdAt,closedAt,body,comments",
                )
            };
            gh(&[noun, "view", n, "-R", repo, "--json", fields]).ok()?
        }
        "gh-release" => {
            let (rp, tag) = loc.split_once('@')?;
            if rp != repo || tag.is_empty() || tag.starts_with('-') {
                return None;
            }
            gh(&[
                "release",
                "view",
                tag,
                "-R",
                repo,
                "--json",
                "tagName,name,publishedAt,body",
            ])
            .ok()?
        }
        _ => return None,
    };
    Some(cut_keep_lines(
        &serde_json::to_string_pretty(&v).ok()?,
        RESOLVE_MAX,
    ))
}

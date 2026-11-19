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

/// The prompt files of this built-in, as (plugin id, file name, text) (spec §6.2).
pub const PROMPTS: [(&str, &str, &str); 1] =
    [("github", "github.md", include_str!("collectors/github.md"))];

/// A resolved text longer than this is cut.
const RESOLVE_MAX: usize = 100_000;

pub fn run(entry: &str, input: &str) -> Result<String> {
    let repo = sources::param_str("repo");
    if repo.split('/').count() != 2 || repo.split('/').any(str::is_empty) || repo.starts_with('-') {
        bail!("param `repo` `{repo}` is not `<owner>/<name>`");
    }
    match entry {
        "fingerprint" => fingerprint(&repo),
        "fetch" => fetch(&repo, input),
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

/// The most recently updated issue or pull request, and a hash of the releases as fetch lists
/// them: an edit to an issue or pull request moves its `updated_at`, but an edited release note
/// moves no date.
fn fingerprint(repo: &str) -> Result<String> {
    let issues = gh(&[
        "api",
        &format!("repos/{repo}/issues?state=all&sort=updated&direction=desc&per_page=1"),
    ])?;
    let newest: Value = ["number", "updated_at"]
        .iter()
        .map(|k| issues[0][*k].clone())
        .collect();
    let releases = Value::from(releases(repo, limit())?).to_string();
    Ok(format!(
        "{newest}\n{}\n",
        crate::files::sha256_hex(releases.as_bytes())
    ))
}

fn limit() -> u64 {
    sources::param("limit").as_u64().unwrap_or(100)
}

/// The newest `limit` published releases, newest first as the API lists them, in the shape
/// fetch gives them; GitHub caps a page at 100, so it pages until `limit` or a short page.
fn releases(repo: &str, limit: u64) -> Result<Vec<Value>> {
    let per_page = limit.clamp(1, 100);
    let mut out = Vec::new();
    for page in 1.. {
        let got = gh(&[
            "api",
            &format!("repos/{repo}/releases?per_page={per_page}&page={page}"),
        ])?;
        let got = got.as_array().cloned().unwrap_or_default();
        out.extend(got.iter().filter(|r| r["draft"] != true).map(|r| {
            json!({
                "tag": r["tag_name"], "name": r["name"], "published_at": r["published_at"],
                "prerelease": r["prerelease"], "body": body(&r["body"]),
            })
        }));
        if out.len() as u64 >= limit || (got.len() as u64) < per_page {
            break;
        }
    }
    out.truncate(limit as usize);
    Ok(out)
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

/// A time of the cursor `since` gives, when it is one (it goes into a search query).
fn cursor_time(input: &Value, key: &str) -> Option<String> {
    input["since"]["cursor"][key]
        .as_str()
        .filter(|t| crate::lock::parse_utc(t).is_some())
        .map(str::to_string)
}

/// The latest of the cursor's time and the times listed under `key` (all UTC, same format).
fn newest(prev: Option<String>, items: &[&Value], key: &str) -> Option<String> {
    items
        .iter()
        .filter_map(|v| v[key].as_str().map(str::to_string))
        .chain(prev)
        .max()
}

/// The pull requests, issues and releases the agent proposes from, newest `limit` of each:
/// pull requests and issues by last update, so closing or merging an old one reaches the agent
/// as the evidence for a removal. With a cursor from the last settled run, only what was updated
/// (pull requests, issues) or published (releases) since; the cursor is the newest of each.
fn fetch(repo: &str, input: &str) -> Result<String> {
    let input: Value = serde_json::from_str(input).context("the fetch input is no JSON")?;
    let (updated, published) = (
        cursor_time(&input, "updated"),
        cursor_time(&input, "published"),
    );
    let limit = limit();
    let mut releases = releases(repo, limit)?;
    // limit: an edit to the notes of a release published before the cursor waits for a full
    // read (`--force`); the release list says nothing about edits.
    if let Some(p) = &published {
        releases.retain(|r| r["published_at"].as_str().is_some_and(|t| t > p.as_str()));
    }
    // limit: when more than `limit` items changed since the cursor, only the newest `limit` come
    // and the cursor moves past the rest, as a full read never lists more than `limit` either.
    let search = match &updated {
        // Inclusive: the newest item of the last run comes once more rather than a same-second
        // update being missed.
        Some(t) => format!("sort:updated-desc updated:>={t}"),
        None => "sort:updated-desc".to_string(),
    };
    let limit = limit.to_string();
    let list = |noun: &str, fields: &str| -> Result<Vec<Value>> {
        // limit: `--search` goes through GitHub's search, which returns at most 1000 items, so a
        // `limit` above 1000 gives 1000; enough for a map's recent work.
        let v = gh(&[
            noun, "list", "-R", repo, "--state", "all", "--search", &search, "--limit", &limit,
            "--json", fields,
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
    let cursor = json!({
        "updated": newest(updated, &pulls.iter().chain(&issues).collect::<Vec<_>>(), "updated_at"),
        "published": newest(published, &releases.iter().collect::<Vec<_>>(), "published_at"),
    });
    Ok(json!({
        "cursor": cursor,
        "repo": repo,
        "cite": {
            "pr": format!("gh-pr:{repo}#<number>"),
            "issue": format!("gh-issue:{repo}#<number>"),
            "release": format!("gh-release:{repo}@<tag>"),
        },
        "pulls": pulls,
        "issues": issues,
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

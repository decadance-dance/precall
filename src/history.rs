//! Answers from the store's git history: `why` and `recent` (spec §2).

use crate::config::{self, STORE_CONFIG};
use crate::error;
use crate::node::Node;
use crate::query;
use crate::store::{Store, git};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
struct Commit {
    hash: String,
    date: String,
    subject: String,
    body: Vec<String>,
}

impl Commit {
    /// The hash as shown: a fixed 12 hex digits, like a source reference's commit (spec §3.3),
    /// whatever `core.abbrev` says, so the same history always prints the same.
    fn short(&self) -> &str {
        self.hash.get(..12).unwrap_or(&self.hash)
    }
}

const FORMAT: &str = "--format=%H%x1f%ad%x1f%s%x1f%b%x1e";

/// How `why` and the html view diff the map, whatever the user's git config says, so both see
/// the same raw JSONL lines: no color, no external diff or text converter, and a node whose
/// line moves to another branch file counts as touched (no rename detection).
const RAW_DIFF: [&str; 4] = [
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--no-renames",
];

fn log(store: &Store, args: &[&str]) -> Result<Vec<Commit>> {
    // Messages come back as UTF-8 whatever `i18n.logOutputEncoding` says.
    let mut all = vec![
        "log",
        "--no-color",
        "--encoding=UTF-8",
        FORMAT,
        "--date=short",
    ];
    all.extend_from_slice(args);
    let out = git(&store.root, &all)?;
    Ok(out
        .split('\x1e')
        .filter_map(|rec| {
            let f: Vec<&str> = rec.trim_start_matches('\n').splitn(4, '\x1f').collect();
            let [hash, date, subject, body] = f[..] else {
                return None;
            };
            Some(Commit {
                hash: hash.into(),
                date: date.into(),
                subject: subject.into(),
                body: body
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(str::to_string)
                    .collect(),
            })
        })
        .collect())
}

fn commit_json(c: &Commit) -> Value {
    json!({ "commit": c.short(), "date": c.date, "subject": c.subject, "body": c.body })
}

/// When a node appeared and changed: the commits that touched its line, oldest first.
pub fn why(store: &Store, id: &str, as_json: bool) -> Result<String> {
    let node = store.get(id)?;
    let commits = if store.is_branch(id) {
        let changed = branch_commits(&config_revisions(store)?, id);
        if changed.is_empty() {
            log(store, &["--max-parents=0"])?
        } else {
            changed
        }
    } else {
        // Ids are slugs, so they need no regex escaping; the pattern matches only this node's own line.
        log(
            store,
            &[
                &RAW_DIFF[..],
                &[
                    "--reverse",
                    "-G",
                    &format!("^[{{]\"id\":\"{id}\","),
                    "--",
                    "map",
                ],
            ]
            .concat(),
        )?
    };
    let commits: Vec<Commit> = commits.into_iter().map(|c| mentioning(c, id)).collect();
    let kids = store.child_map();
    if as_json {
        let history: Vec<Value> = commits.iter().map(commit_json).collect();
        return Ok(format!(
            "{}\n",
            json!({ "node": query::json_node(store, &kids, node), "history": history })
        ));
    }
    let mut out = format!(
        "## {} ({id})\nsources: {}\nhistory:\n",
        query::path(store, id),
        node.sources.join(", ")
    );
    for c in &commits {
        out += &format!("- {} {} {}\n", c.date, c.short(), c.subject);
        for l in &c.body {
            out += &format!("  {l}\n");
        }
    }
    Ok(out)
}

/// Each revision of the store config, oldest first, with its text.
fn config_revisions(store: &Store) -> Result<Vec<(Commit, String)>> {
    Ok(log(store, &["--reverse", "--", STORE_CONFIG])?
        .into_iter()
        .map(|c| {
            let text = git(
                &store.root,
                &["show", &format!("{}:{STORE_CONFIG}", c.hash)],
            )
            .unwrap_or_default();
            (c, text)
        })
        .collect())
}

/// The commits where a branch's entry in the config differs from the revision before. Empty
/// for a default branch, which exists since init, the root commit.
fn branch_commits(revisions: &[(Commit, String)], id: &str) -> Vec<Commit> {
    let mut changed = Vec::new();
    let mut prev = None;
    for (c, text) in revisions {
        let entry = config::branch_entry(text, id);
        if entry != prev {
            changed.push(c.clone());
        }
        prev = entry;
    }
    changed
}

/// The commit with only the body lines that name the node.
fn mentioning(mut c: Commit, id: &str) -> Commit {
    let mention = format!("/{id}");
    c.body
        .retain(|l| l.split_whitespace().any(|w| w.ends_with(&mention)));
    c
}

/// Every node's history for the html view (spec §8): what `why` reports, read with one
/// `git log -p` over the map instead of one log per node.
pub struct MapHistory {
    /// Commits per node id, oldest first, as `why --json` lists them.
    pub nodes: BTreeMap<String, Vec<Value>>,
    /// The newest commit whose subject starts with `run `.
    pub last_run: Option<Value>,
    /// The nodes whose history holds the last run's commit or a later one.
    pub changed: BTreeSet<String>,
}

pub fn map_history(store: &Store) -> Result<MapHistory> {
    let all = log(store, &[])?; // newest first
    let last_run = all.iter().position(|c| c.subject.starts_with("run "));
    let since: BTreeSet<&str> = last_run
        .map(|i| all[..=i].iter().map(|c| c.hash.as_str()).collect())
        .unwrap_or_default();
    let by_hash: BTreeMap<&str, &Commit> = all.iter().map(|c| (c.hash.as_str(), c)).collect();

    // The ids whose line a commit adds or removes: the lines `why`'s `-G` pattern matches.
    let mut touched: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let patches = git(
        &store.root,
        &[
            &["log", "--reverse", "-p"][..],
            &RAW_DIFF,
            &["--unified=0", "--format=%x1e%H", "--", "map"],
        ]
        .concat(),
    )?;
    for rec in patches.split('\x1e').skip(1) {
        let mut lines = rec.lines();
        let hash = lines.next().unwrap_or_default();
        for l in lines {
            let Some(rest) = l
                .strip_prefix("+{\"id\":\"")
                .or_else(|| l.strip_prefix("-{\"id\":\""))
            else {
                continue;
            };
            let Some((id, _)) = rest.split_once("\",") else {
                continue;
            };
            let hashes = touched.entry(id.to_string()).or_default();
            if hashes.last().is_none_or(|h| h != hash) {
                hashes.push(hash.to_string());
            }
        }
    }

    // Read once for every branch: a branch's history comes from the config's revisions.
    let revisions = config_revisions(store)?;
    let root = log(store, &["--max-parents=0"])?;
    let mut nodes = BTreeMap::new();
    let mut changed = BTreeSet::new();
    for id in store.nodes.keys() {
        let commits = if store.is_branch(id) {
            let c = branch_commits(&revisions, id);
            if c.is_empty() { root.clone() } else { c }
        } else {
            touched
                .get(id)
                .into_iter()
                .flatten()
                .filter_map(|h| by_hash.get(h.as_str()).map(|c| (*c).clone()))
                .collect()
        };
        if commits.iter().any(|c| since.contains(c.hash.as_str())) {
            changed.insert(id.clone());
        }
        let history = commits
            .into_iter()
            .map(|c| commit_json(&mentioning(c, id)))
            .collect();
        nodes.insert(id.clone(), history);
    }
    Ok(MapHistory {
        nodes,
        last_run: last_run.map(|i| commit_json(&all[i])),
        changed,
    })
}

/// Days in a `--since` window: `<N>d` or `<N>w`.
fn window_days(since: &str) -> Result<i64> {
    const MAX_DAYS: i64 = 36_500;
    let bad = || {
        error::usage(format!(
            "--since `{since}`: use <N>d or <N>w, such as 7d, at most {MAX_DAYS} days"
        ))
    };
    let (n, per) = if let Some(n) = since.strip_suffix('d') {
        (n, 1)
    } else if let Some(n) = since.strip_suffix('w') {
        (n, 7)
    } else {
        return Err(bad());
    };
    let n: i64 = n.parse().map_err(|_| bad())?;
    n.checked_mul(per)
        .filter(|d| (0..=MAX_DAYS).contains(d))
        .ok_or_else(bad)
}

/// `YYYY-MM-DD` of a day count since 1970-01-01 (proleptic Gregorian, H. Hinnant's algorithm).
fn civil(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{m:02}-{d:02}", yoe + era * 400 + i64::from(m <= 2))
}

fn today_days() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    (secs / 86_400) as i64
}

/// Map commits since the window's first day (UTC) and dated nodes in the window.
pub fn recent(store: &Store, since: &str, as_json: bool) -> Result<String> {
    let today = today_days();
    let (from, to) = (civil(today - window_days(since)?), civil(today));
    let commits = log(store, &[&format!("--since={from} 00:00:00 +0000")])?;
    let mut dated: Vec<&Node> = store
        .nodes
        .values()
        .filter(|n| n.date.as_deref().is_some_and(|d| *d >= *from && *d <= *to))
        .collect();
    dated.sort_by(|a, b| (&b.date, &a.id).cmp(&(&a.date, &b.id)));
    if as_json {
        let kids = store.child_map();
        let commits: Vec<Value> = commits.iter().map(commit_json).collect();
        let nodes: Vec<Value> = dated
            .iter()
            .map(|n| query::json_node(store, &kids, n))
            .collect();
        return Ok(format!(
            "{}\n",
            json!({ "since": from, "commits": commits, "nodes": nodes })
        ));
    }
    let mut out = format!("## Map changes since {from}\n");
    if commits.is_empty() {
        out += "none\n";
    }
    for c in &commits {
        out += &format!("- {} {} {}\n", c.date, c.short(), c.subject);
        for l in &c.body {
            out += &format!("  {l}\n");
        }
    }
    out += &format!("\n## Dated nodes since {from}\n");
    if dated.is_empty() {
        out += "none\n";
    }
    for n in dated {
        out += &format!(
            "- {} {} ({})\n",
            n.date.as_deref().unwrap_or(""),
            query::path(store, &n.id),
            n.id
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), "1970-01-01");
        assert_eq!(civil(11_016), "2000-02-29");
        assert_eq!(civil(19_723), "2024-01-01");
        assert_eq!(civil(-1), "1969-12-31");
    }

    #[test]
    fn windows() {
        assert_eq!(window_days("7d").unwrap(), 7);
        assert_eq!(window_days("2w").unwrap(), 14);
        for bad in [
            "",
            "d",
            "7",
            "7x",
            "-d",
            "1.5d",
            "-3d",
            "é",
            "7é",
            "9223372036854775807w",
            "99999999d",
        ] {
            assert!(window_days(bad).is_err(), "{bad}");
        }
    }
}

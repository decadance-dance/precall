//! Answers from the store's git history: `why` and `recent` (spec §2).

use crate::error;
use crate::node::Node;
use crate::query;
use crate::store::{Store, git};
use anyhow::Result;
use serde_json::{Value, json};

struct Commit {
    hash: String,
    date: String,
    subject: String,
    body: Vec<String>,
}

const FORMAT: &str = "--format=%h%x1f%ad%x1f%s%x1f%b%x1e";

fn log(store: &Store, args: &[&str]) -> Result<Vec<Commit>> {
    let mut all = vec!["log", FORMAT, "--date=short"];
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
    json!({ "commit": c.hash, "date": c.date, "subject": c.subject, "body": c.body })
}

/// When a node appeared and changed: the commits that touched its line, oldest first.
pub fn why(store: &Store, id: &str, as_json: bool) -> Result<String> {
    let node = store.get(id)?;
    let mut commits = if store.is_branch(id) {
        log(
            store,
            &[
                "--reverse",
                "-G",
                &format!("^id = \"{id}\"$"),
                "--",
                crate::config::STORE_CONFIG,
            ],
        )?
    } else {
        // Ids are slugs, so they need no regex escaping; the pattern matches only this node's own line.
        log(
            store,
            &[
                "--reverse",
                "-G",
                &format!("^[{{]\"id\":\"{id}\","),
                "--",
                "map",
            ],
        )?
    };
    if commits.is_empty() && store.is_branch(id) {
        // A default branch exists since init, the root commit.
        commits = log(store, &["--max-parents=0"])?;
    }
    let mention = format!("/{id}");
    for c in &mut commits {
        c.body
            .retain(|l| l.split_whitespace().any(|w| w.ends_with(&mention)));
    }
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
        out += &format!("- {} {} {}\n", c.date, c.hash, c.subject);
        for l in &c.body {
            out += &format!("  {l}\n");
        }
    }
    Ok(out)
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
        out += &format!("- {} {} {}\n", c.date, c.hash, c.subject);
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

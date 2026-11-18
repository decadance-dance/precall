//! One function per command; each returns what goes to stdout.

use crate::config::{self, BranchDef, STORE_CONFIG};
use crate::error;
use crate::history;
use crate::node::{self, Node};
use crate::outline;
use crate::query;
use crate::store::{self, Store};
use anyhow::{Context, Result};
use std::path::Path;

/// The resolved project's store, loaded.
pub fn open(project: Option<&str>) -> Result<Store> {
    let (_, path) = config::resolve_project(project)?;
    Store::load(&path)
}

pub fn init(name_arg: Option<&str>, project_flag: Option<&str>) -> Result<String> {
    let cwd = std::env::current_dir()?;
    let top = store::git(&cwd, &["rev-parse", "--show-toplevel"])
        .ok()
        .map(|s| s.trim().to_string());
    let origin = config::origin_url(&cwd);
    let env = std::env::var("RRECALL_PROJECT")
        .ok()
        .filter(|v| !v.is_empty());
    let name = name_arg
        .map(str::to_string)
        .or(project_flag.map(str::to_string))
        .or(env)
        .or_else(|| origin.as_deref().map(origin_basename))
        .ok_or_else(|| {
            error::usage(
                "name the project: `rrecall init <project>` (no origin remote to name it after)",
            )
        })?;
    config::check_project_name(&name)?;
    let user = config::load_user_config()?;
    let path = user.store_path(&name)?;
    if path.join(STORE_CONFIG).exists() || path.join(".git").exists() {
        return Err(error::usage(format!(
            "a map store for `{name}` already exists at {}",
            path.display()
        )));
    }
    let mut cfg = String::from(
        "# rrecall store config (spec §9). Add branches with [[skeleton.branch]] (id, title, question, granularity).\n",
    );
    let mut found = Vec::new();
    if let Some(top) = &top {
        cfg += "\n# Sources rrecall init found, as the plugin instances `rrecall run` will use.\n";
        cfg += &format!(
            "[[plugins]]\nid = \"repo\"\nuse = \"files\"\nparams = {{ path = {}, git = true, preset = \"codebase\" }}\n",
            config::toml_str(top)
        );
        found.push(format!("repo {top}"));
    }
    if let Some(repo) = origin.as_deref().and_then(github_repo) {
        cfg += &format!(
            "\n[[plugins]]\nid = \"github\"\nparams = {{ repo = {} }}\n",
            config::toml_str(&repo)
        );
        found.push(format!("github {repo}"));
    }
    let created = !path.exists();
    let made = std::fs::create_dir_all(&path)
        .with_context(|| format!("creating {}", path.display()))
        .and_then(|()| store::git(&path, &["init", "-q"]))
        .and_then(|_| {
            let files = [
                (".gitignore", "cache/\nruns/\nviews/\n".to_string()),
                // Map lines stay LF in the working tree whatever core.autocrlf says (spec §5 I6).
                (".gitattributes", "*.jsonl text eol=lf\n".to_string()),
                (STORE_CONFIG, cfg),
            ];
            let changes: Vec<store::Change> = files
                .into_iter()
                .map(|(rel, text)| (rel.to_string(), Some(text)))
                .collect();
            store::write_and_commit(&path, &changes, &format!("init {name}"))
        });
    if let Err(e) = made {
        if created {
            let _ = std::fs::remove_dir_all(&path);
        }
        return Err(e);
    }

    let mut out = format!("Created the map of `{name}` at {}\n", path.display());
    match user.project(&name) {
        None => {
            let mut entry = format!("\n[[projects]]\nname = {}\n", config::toml_str(&name));
            if let Some(url) = &origin {
                entry += &format!("remotes = [{}]\n", config::toml_str(url));
            }
            append(&config::user_config_path()?, &entry)?;
        }
        Some(p) => {
            if let Some(url) = origin.as_ref().filter(|u| !p.remotes.contains(u)) {
                // limit: rrecall appends whole entries only, so it does not edit an existing one.
                out += &format!(
                    "Add {url} to the remotes of project `{name}` in {} to resolve it from this repo.\n",
                    config::user_config_path()?.display()
                );
            }
        }
    }
    let ids: Vec<String> = config::default_branches()
        .into_iter()
        .map(|b| b.id)
        .collect();
    out += &format!("branches: {}\n", ids.join(", "));
    out += &format!(
        "sources: {}\n",
        if found.is_empty() {
            "none found".into()
        } else {
            found.join(", ")
        }
    );
    out += "Next: `rrecall import <outline>` to seed it, `rrecall ls` to read it.\n";
    Ok(out)
}

fn append(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    f.write_all(text.as_bytes())
        .with_context(|| format!("writing {}", path.display()))
}

fn origin_basename(url: &str) -> String {
    let last = url
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("");
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

/// `owner/name` of a GitHub remote URL (ssh or https).
fn github_repo(url: &str) -> Option<String> {
    // `scheme://[user@]host[:port]/path` or scp-like `[user@]host:path`.
    let (authority, path) = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?,
        None => url.split_once(':')?,
    };
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?;
    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    (!owner.is_empty() && !name.is_empty() && !name.contains('/'))
        .then(|| format!("{owner}/{name}"))
}

pub fn ls(project: Option<&str>, depth: usize, json: bool) -> Result<String> {
    Ok(query::ls(&open(project)?, depth, json))
}

pub fn show(
    project: Option<&str>,
    id: &str,
    depth: usize,
    max_tokens: Option<usize>,
    json: bool,
) -> Result<String> {
    query::show(&open(project)?, id, depth, max_tokens, json)
}

pub fn find(project: Option<&str>, text: &str, json: bool) -> Result<String> {
    query::find(&open(project)?, text, json)
}

pub fn why(project: Option<&str>, id: &str, json: bool) -> Result<String> {
    history::why(&open(project)?, id, json)
}

pub fn recent(project: Option<&str>, since: &str, json: bool) -> Result<String> {
    history::recent(&open(project)?, since, json)
}

/// The last run report under `runs/`; the reports themselves come with `rrecall run` (M2).
pub fn status(project: Option<&str>, json: bool) -> Result<String> {
    let s = open(project)?;
    let last = std::fs::read_dir(s.root.join("runs"))
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("report.md").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .max();
    let report = match &last {
        Some(id) => Some(std::fs::read_to_string(
            s.root.join("runs").join(id).join("report.md"),
        )?),
        None => None,
    };
    Ok(match (json, last, report) {
        (true, run, report) => format!("{}\n", serde_json::json!({ "run": run, "report": report })),
        (false, Some(run), Some(report)) => format!("Last run: {run}\n\n{report}"),
        _ => "No run report yet: this map has not been through `rrecall run`.\n".into(),
    })
}

fn stored_node<'a>(s: &'a Store, id: &str) -> Result<&'a Node> {
    let n = s.get(id)?;
    if n.parent.is_none() {
        return Err(error::usage(format!(
            "`{id}` is a branch; branches come from {STORE_CONFIG} (spec §5 I5), so notes and removals apply to the nodes under them"
        )));
    }
    Ok(n)
}

/// Sets or clears the user's note (spec §5 I7); the only field this command writes.
pub fn note(project: Option<&str>, id: &str, text: &str) -> Result<String> {
    let mut s = open(project)?;
    let new = (!text.is_empty()).then(|| text.to_string());
    if stored_node(&s, id)?.note == new {
        return Ok(format!("No change: `{id}` already has this note.\n"));
    }
    let branch = s.branch_of(id).to_string();
    if let Some(n) = s.nodes.get_mut(id) {
        n.note = new.clone();
    }
    let suffix = if new.is_none() { ": cleared" } else { "" };
    s.save_and_commit(Vec::new(), &format!("note {branch}/{id}{suffix}"))?;
    Ok(format!(
        "{} the note on `{id}`.\n",
        if new.is_none() { "Cleared" } else { "Set" }
    ))
}

/// Removes a node by hand, and its descendants only with `recursive`.
pub fn rm(
    project: Option<&str>,
    id: &str,
    reason: Option<&str>,
    recursive: bool,
) -> Result<String> {
    let mut s = open(project)?;
    stored_node(&s, id)?;
    let desc = s.descendants(id);
    if !desc.is_empty() && !recursive {
        return Err(error::usage(format!(
            "`{id}` has {} descendants; pass --recursive to remove them too",
            desc.len()
        )));
    }
    let branch = s.branch_of(id).to_string();
    let mut body = String::new();
    for x in std::iter::once(id.to_string()).chain(desc) {
        s.nodes.remove(&x);
        body += &format!("removed {branch}/{x}\n");
    }
    let reason = reason
        .map(|r| r.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|r| !r.is_empty());
    let subject = format!(
        "rm {branch}/{id}: {}",
        reason.as_deref().unwrap_or("removed by hand")
    );
    s.save_and_commit(Vec::new(), &format!("{subject}\n\n{body}"))?;
    Ok(body)
}

pub const AGENT_INSTRUCTIONS: &str = "\
This project has an rrecall map: a versioned map of what lives where, how it works, who owns what, \
what was done, what is blocked and why decisions were made, as of its last run. Ask it one question \
per call instead of reading the whole map: `rrecall ls` lists branches and ids; `rrecall show <id>` \
prints one node with its sources (`--depth N` for more of its subtree, `--max-tokens N` to cap the size); \
`rrecall find <text>` searches titles and bodies; `rrecall recent --since 7d` shows what changed lately; \
`rrecall why <id>` shows when and why a node appeared. Add `--json` for JSON. \
For the live status of a task, ask the tracker the map points to.
";

pub fn import(project: Option<&str>, file: &Path) -> Result<String> {
    let (_, path) = config::resolve_project(project)?;
    let mut s = Store::load(&path)?;
    if s.nodes.len() > s.branches.len() {
        return Err(error::usage(
            "the map already has nodes; `rrecall import` seeds an empty map",
        ));
    }
    let text =
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let fname = file
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let usage = |line: usize, e: String| error::usage(format!("{fname} line {line}: {e}"));
    let items = outline::parse(&text).map_err(|e| error::usage(format!("{fname}: {e}")))?;
    let source = format!("import:{fname}");
    node::check_source(&source)
        .map_err(|e| error::usage(format!("{e}: rename the outline file")))?;

    let mut new_branches = Vec::new();
    let mut added = 0;
    for item in &items {
        let matched = s
            .branches
            .iter()
            .find(|b| {
                let title = item.title.to_lowercase();
                b.id == title || b.title.to_lowercase() == title
            })
            .map(|b| b.id.clone());
        let branch = match matched {
            Some(id) if !item.detail.is_empty() => {
                return Err(usage(
                    item.line,
                    format!(
                        "`{}` is the branch `{id}`, whose question comes from {STORE_CONFIG}; drop the detail",
                        item.title
                    ),
                ));
            }
            Some(id) => id,
            None => {
                let b = BranchDef {
                    id: node::unique_slug(&item.title, |x| s.id_taken(x)),
                    title: item.title.clone(),
                    question: item.detail.clone(),
                    granularity: String::new(),
                };
                // The same limits as a node: title 80, question as long as a body.
                Node {
                    parent: Some(b.id.clone()),
                    ..new_node(&b.id, &b.title, &b.question, &source)
                }
                .validate()
                .map_err(|e| usage(item.line, e))?;
                let id = b.id.clone();
                new_branches.push(b.clone());
                s.add_branch(b);
                id
            }
        };
        added += add_items(&mut s, &item.children, &branch, &source)
            .map_err(|(line, e)| usage(line, e))?;
    }
    let mut extra = Vec::new();
    if !new_branches.is_empty() {
        let old = std::fs::read_to_string(path.join(STORE_CONFIG))
            .with_context(|| format!("reading {STORE_CONFIG}"))?;
        let toml: String = new_branches.iter().map(config::branch_toml).collect();
        extra.push((STORE_CONFIG.to_string(), Some(old + &toml)));
    }
    s.save_and_commit(
        extra,
        &format!(
            "import {fname}: +{added} nodes, +{} branches",
            new_branches.len()
        ),
    )?;
    let ids: Vec<&str> = new_branches.iter().map(|b| b.id.as_str()).collect();
    Ok(format!(
        "Imported {added} nodes from {fname}{}\n",
        if ids.is_empty() {
            String::new()
        } else {
            format!("; new branches: {}", ids.join(", "))
        }
    ))
}

fn new_node(id: &str, title: &str, body: &str, source: &str) -> Node {
    Node {
        id: id.into(),
        parent: None,
        title: title.into(),
        body: body.into(),
        sources: vec![source.into()],
        date: None,
        state: None,
        order: 0,
        note: None,
    }
}

/// Adds outline items under `parent` with core-assigned ids; returns how many nodes were added.
fn add_items(
    s: &mut Store,
    items: &[outline::Item],
    parent: &str,
    source: &str,
) -> Result<usize, (usize, String)> {
    let mut n = 0;
    // New nodes go after the parent's existing children (spec §3.1 `order`).
    let base = s
        .nodes
        .values()
        .filter(|x| x.parent.as_deref() == Some(parent))
        .count();
    for (i, item) in items.iter().enumerate() {
        let order = base + i;
        let id = node::unique_slug(&item.title, |x| s.id_taken(x));
        let node = Node {
            parent: Some(parent.into()),
            order: order as u32,
            ..new_node(&id, &item.title, &item.detail, source)
        };
        node.validate().map_err(|e| (item.line, e))?;
        s.nodes.insert(id.clone(), node);
        n += 1 + add_items(s, &item.children, &id, source)?;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_names() {
        for (url, base, gh) in [
            (
                "git@github.com:acme/widget.git",
                "widget",
                Some("acme/widget"),
            ),
            (
                "https://github.com/acme/widget",
                "widget",
                Some("acme/widget"),
            ),
            (
                "ssh://git@github.com/acme/widget.git/",
                "widget",
                Some("acme/widget"),
            ),
            ("https://example.org/x/kiln.git", "kiln", None),
            (
                "https://mirror.example/github.com/acme/tulip.git",
                "tulip",
                None,
            ),
            ("git@github.com.evil.example:acme/tulip.git", "tulip", None),
            (
                "https://user@GitHub.com:443/acme/tulip.git",
                "tulip",
                Some("acme/tulip"),
            ),
            ("git://github.com/acme/tulip", "tulip", Some("acme/tulip")),
        ] {
            assert_eq!(origin_basename(url), base, "{url}");
            assert_eq!(github_repo(url).as_deref(), gh, "{url}");
        }
    }
}

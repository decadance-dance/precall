//! One function per command; each returns what goes to stdout.

use crate::config::{self, STORE_CONFIG};
use crate::error;
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
    let top = store::git(&cwd, &["rev-parse", "--show-toplevel"]).ok().map(|s| s.trim().to_string());
    let origin = config::origin_url(&cwd);
    let env = std::env::var("RRECALL_PROJECT").ok().filter(|v| !v.is_empty());
    let name = name_arg
        .map(str::to_string)
        .or(project_flag.map(str::to_string))
        .or(env)
        .or_else(|| origin.as_deref().map(origin_basename))
        .ok_or_else(|| error::usage("name the project: `rrecall init <project>` (no origin remote to name it after)"))?;
    config::check_project_name(&name)?;
    let user = config::load_user_config()?;
    let path = user.store_path(&name)?;
    if path.join(STORE_CONFIG).exists() || path.join(".git").exists() {
        return Err(error::usage(format!("a map store for `{name}` already exists at {}", path.display())));
    }
    std::fs::create_dir_all(&path).with_context(|| format!("creating {}", path.display()))?;
    store::git(&path, &["init", "-q"])?;
    std::fs::write(path.join(".gitignore"), "cache/\nruns/\nviews/\n")?;
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
        cfg += &format!("\n[[plugins]]\nid = \"github\"\nparams = {{ repo = {} }}\n", config::toml_str(&repo));
        found.push(format!("github {repo}"));
    }
    std::fs::write(path.join(STORE_CONFIG), cfg)?;
    let s = Store::load(&path)?;
    s.commit(&format!("init {name}"))?;

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
    let ids: Vec<&str> = s.branches.iter().map(|b| b.id.as_str()).collect();
    out += &format!("branches: {}\n", ids.join(", "));
    out += &format!("sources: {}\n", if found.is_empty() { "none found".into() } else { found.join(", ") });
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
    f.write_all(text.as_bytes()).with_context(|| format!("writing {}", path.display()))
}

fn origin_basename(url: &str) -> String {
    let last = url.trim_end_matches('/').rsplit(['/', ':']).next().unwrap_or("");
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

/// `owner/name` of a GitHub remote URL (ssh or https).
fn github_repo(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("github.com")?;
    let rest = rest.trim_start_matches([':', '/']).trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, name) = rest.split_once('/')?;
    (!owner.is_empty() && !name.is_empty() && !name.contains('/')).then(|| format!("{owner}/{name}"))
}

pub fn ls(project: Option<&str>, depth: usize, json: bool) -> Result<String> {
    Ok(query::ls(&open(project)?, depth, json))
}

pub fn show(project: Option<&str>, id: &str, depth: usize, max_tokens: Option<usize>, json: bool) -> Result<String> {
    query::show(&open(project)?, id, depth, max_tokens, json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_names() {
        for (url, base, gh) in [
            ("git@github.com:acme/widget.git", "widget", Some("acme/widget")),
            ("https://github.com/acme/widget", "widget", Some("acme/widget")),
            ("ssh://git@github.com/acme/widget.git/", "widget", Some("acme/widget")),
            ("https://example.org/x/kiln.git", "kiln", None),
        ] {
            assert_eq!(origin_basename(url), base, "{url}");
            assert_eq!(github_repo(url).as_deref(), gh, "{url}");
        }
    }
}

//! `rrecall plugin list | add | trust` (spec §6.7). None of them runs a plugin's command: the
//! manifest preview and the user's answer are the safety step.

use crate::config;
use crate::error::{self, Fail};
use crate::plugin::{self, Manifest, Plugin, Spec};
use crate::store;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// Installs the plugin at `source` (an existing local directory, else a git URL) into the user
/// plugin directory, after showing it and asking, and records where it came from.
pub fn add(source: &str, git_ref: Option<&str>) -> Result<String> {
    if git_ref.is_some_and(|r| r.is_empty() || r.starts_with('-')) {
        return Err(error::usage("--ref must name a tag, branch or commit"));
    }
    let user = config::load_user_config()?;
    let plugins = config::user_plugins_dir()?;
    let parent = plugins
        .parent()
        .context("the user plugin directory has no parent")?;
    // Staged beside `plugins/`, not inside it, so a run in the meantime never discovers it.
    let stage = Staged(parent.join(format!(".plugin-add-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&stage.0);
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let local = Path::new(source);
    let (source, commit) = if local.exists() && git_ref.is_none() {
        let abs = local
            .canonicalize()
            .with_context(|| format!("reading {source}"))?;
        copy_tree(&abs, &stage.0)?;
        (abs.to_string_lossy().into_owned(), None)
    } else {
        clone(source, git_ref, &stage.0)?
    };
    // limit: a source holds one plugin, at its root; a repository of several plugins is not
    // installable until `add` takes a subdirectory.
    let manifest = plugin::load_manifest(&stage.0)?;
    let id = &manifest.id;
    let dest = plugins.join(id);
    // limit: no update in place; reinstalling means removing the directory and the record.
    if dest.exists() || user.installed.iter().any(|i| &i.id == id) {
        return Err(error::usage(format!(
            "plugin `{id}` is already installed: remove {} and its [[installed]] entry in {} to install it again",
            dest.display(),
            config::user_config_path()?.display()
        )));
    }
    print!("{}", preview(&manifest, &stage.0)?);
    if !confirm(&format!("Install {id} into {}? [y/N] ", dest.display()))? {
        return Err(declined(&format!("plugin `{id}` was not installed")));
    }
    std::fs::create_dir_all(&plugins).with_context(|| format!("creating {}", plugins.display()))?;
    std::fs::rename(&stage.0, &dest).with_context(|| format!("moving into {}", dest.display()))?;
    let recorded = plugin::content_hash(&dest).and_then(|hash| {
        let mut entry = format!(
            "[[installed]]\nid = {}\nsource = {}\n",
            config::toml_str(id),
            config::toml_str(&source)
        );
        if let Some(r) = git_ref {
            entry += &format!("ref = {}\n", config::toml_str(r));
        }
        if let Some(c) = &commit {
            entry += &format!("commit = {}\n", config::toml_str(c));
        }
        entry += &format!("hash = {}\n", config::toml_str(&hash));
        config::append_user_config(&entry)
    });
    if let Err(e) = recorded {
        // Unrecorded, the plugin would load with no trace of where it came from.
        let _ = std::fs::remove_dir_all(&dest);
        return Err(e);
    }
    Ok(format!(
        "Installed {id} {} into {}\n",
        manifest.version,
        dest.display()
    ))
}

/// Trusts a project's `.rrecall/plugins/` directory at its current content hash, after showing
/// every manifest in it and asking.
pub fn trust(dir: &Path) -> Result<String> {
    let dir = dir
        .canonicalize()
        .map_err(|e| error::usage(format!("{}: {e}", dir.display())))?;
    if !dir.is_dir() {
        return Err(error::usage(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    // Hashed before it is shown: a change after the preview is not covered by the answer.
    let hash = plugin::content_hash(&dir)?;
    let user = config::load_user_config()?;
    let recorded = user
        .trusted
        .iter()
        .any(|t| t.hash == hash && t.path.canonicalize().is_ok_and(|p| p == dir));
    if recorded {
        return Ok(format!("{} is already trusted at {hash}\n", dir.display()));
    }
    let found = plugin::discover(std::slice::from_ref(&dir))?;
    if found.is_empty() {
        return Err(error::usage(format!(
            "{} holds no plugin (a subdirectory with {})",
            dir.display(),
            plugin::MANIFEST
        )));
    }
    for p in found.values() {
        print!(
            "== {} ({})\n{}",
            p.manifest.id,
            p.dir.display(),
            preview(&p.manifest, &p.dir)?
        );
    }
    if !confirm(&format!("Trust {}? [y/N] ", dir.display()))? {
        return Err(declined(&format!("{} was not trusted", dir.display())));
    }
    config::append_user_config(&format!(
        "[[trusted]]\npath = {}\nhash = {}\n",
        config::toml_str(&dir.to_string_lossy()),
        config::toml_str(&hash)
    ))?;
    Ok(format!("Trusted {} at {hash}\n", dir.display()))
}

/// The discovered plugins, one line each: origin, version, content hash, the instances that
/// use it and, for an installed one, where it came from.
pub fn list(project: Option<&str>) -> Result<String> {
    let user = config::load_user_config()?;
    let user_dir = config::user_plugins_dir()?;
    let mut out = String::new();
    let mut places = vec![("user", user_dir.clone())];
    let mut uses: BTreeMap<String, Vec<String>> = BTreeMap::new();
    match config::resolve_project(project) {
        Ok((_, store)) => {
            let cfg = config::load_store_config(&store)?;
            let found = plugin::places(&user, &cfg, &store, &user_dir)?;
            places = found
                .dirs
                .into_iter()
                .map(|d| {
                    let origin = if d == user_dir {
                        "user"
                    } else if d == store.join("plugins") {
                        "store"
                    } else {
                        "project"
                    };
                    (origin, d)
                })
                .collect();
            if let Some(d) = found.untrusted {
                out += &format!(
                    "project {}: not trusted; run `rrecall plugin trust {}`\n",
                    d.display(),
                    d.display()
                );
            }
            for e in &cfg.plugins {
                let id = e.uses.clone().unwrap_or_else(|| e.id.clone());
                uses.entry(id).or_default().push(e.id.clone());
            }
        }
        Err(e) if matches!(error::exit_code(&e), 2 | 3) => {
            out += &format!("no project ({e:#}): built-in and user plugins only\n");
        }
        Err(e) => return Err(e),
    }
    let mut groups: Vec<(&str, BTreeMap<String, Plugin>)> = vec![("built-in", plugin::builtin()?)];
    for (origin, dir) in &places {
        groups.push((origin, plugin::discover(std::slice::from_ref(dir))?));
    }
    for (g, (origin, found)) in groups.iter().enumerate() {
        for (id, p) in found {
            let m = &p.manifest;
            let hash = plugin::plugin_hash(p)?;
            let mut line = format!(
                "{id}  {}  {origin}  {}  {hash}",
                format!("{:?}", m.kind()).to_lowercase(),
                m.version
            );
            match groups[g + 1..]
                .iter()
                .rev()
                .find(|(_, f)| f.contains_key(id))
            {
                Some((later, _)) => line += &format!("  replaced by {later}"),
                None => {
                    if let Some(ids) = uses.get(id) {
                        line += &format!("  instances: {}", ids.join(", "));
                    }
                }
            }
            if *origin == "user"
                && let Some(i) = user.installed.iter().find(|i| &i.id == id)
            {
                line += &format!("  from {}", i.source);
                if let Some(r) = &i.git_ref {
                    line += &format!("@{r}");
                }
                if i.hash != hash {
                    line += "  changed since add";
                }
            }
            out += &line;
            out.push('\n');
        }
    }
    Ok(out)
}

/// The manifest as written, then what it needs and every command it will run.
fn preview(m: &Manifest, dir: &Path) -> Result<String> {
    let path = dir.join(plugin::MANIFEST);
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut out = text;
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out += &format!(
        "requires: {}\n",
        if m.requires.is_empty() {
            "nothing".into()
        } else {
            m.requires.join(", ")
        }
    );
    for (key, argv) in m.argvs() {
        let shown: Vec<String> = argv
            .iter()
            .map(|a| {
                if !a.is_empty()
                    && a.chars()
                        .all(|c| c.is_ascii_graphic() && !"\"'\\".contains(c))
                {
                    a.clone()
                } else {
                    config::toml_str(a)
                }
            })
            .collect();
        out += &format!("{key}: {}\n", shown.join(" "));
    }
    let prompts: Vec<(&str, &Option<String>, &Option<String>)> = match &m.spec {
        Spec::Collector(c) => vec![
            ("prompt", &c.prompt, &c.runner),
            ("resolve_prompt", &c.resolve_prompt, &c.runner),
        ],
        Spec::Judge(j) => vec![("prompt", &j.prompt, &j.runner)],
        _ => Vec::new(),
    };
    for (key, file, runner) in prompts {
        if let Some(file) = file {
            out += &format!(
                "{key}: {file}, run by {}\n",
                runner
                    .as_ref()
                    .map_or("the configured runner".into(), |r| format!(
                        "the runner `{r}`"
                    ))
            );
        }
    }
    Ok(out)
}

/// Asks `question` on stderr and reads one answer line from stdin: only `y` or `yes` agrees,
/// so no answer (stdin closed, as in a script that pipes nothing) declines.
fn confirm(question: &str) -> Result<bool> {
    std::io::stdout().flush()?;
    eprint!("{question}");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

fn declined(msg: &str) -> anyhow::Error {
    Fail {
        code: 1,
        msg: msg.into(),
    }
    .into()
}

/// A staging directory, removed when `add` ends without moving it into place.
struct Staged(PathBuf);

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Clones `url` into `to`, checks out `git_ref` when given, and drops `.git`. Returns the
/// source as recorded and the commit installed.
fn clone(url: &str, git_ref: Option<&str>, to: &Path) -> Result<(String, Option<String>)> {
    let to_text = to.to_str().context("the staging path is not UTF-8")?;
    let parent = to.parent().context("the staging path has no parent")?;
    store::git(parent, &["clone", "-q", "--", url, to_text])
        .with_context(|| format!("cloning {url}"))?;
    if let Some(r) = git_ref {
        store::git(to, &["checkout", "-q", "--detach", r, "--"])
            .with_context(|| format!("checking out {r} of {url}"))?;
    }
    let commit = store::git(to, &["rev-parse", "HEAD"])?.trim().to_string();
    std::fs::remove_dir_all(to.join(".git"))
        .with_context(|| format!("removing {}", to.join(".git").display()))?;
    Ok((url.to_string(), Some(commit)))
}

/// Copies the tree at `from` to `to` (which must not exist), symlinks as links and file modes
/// kept, without `from`'s own `.git`.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fn walk(from: &Path, to: &Path, top: bool) -> Result<()> {
        std::fs::create_dir(to).with_context(|| format!("creating {}", to.display()))?;
        for e in std::fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
            let e = e?;
            if top && e.file_name() == ".git" {
                continue;
            }
            let (src, dst) = (e.path(), to.join(e.file_name()));
            let t = e.file_type()?;
            if t.is_symlink() {
                std::os::unix::fs::symlink(std::fs::read_link(&src)?, &dst)
                    .with_context(|| format!("creating {}", dst.display()))?;
            } else if t.is_dir() {
                walk(&src, &dst, false)?;
            } else if t.is_file() {
                std::fs::copy(&src, &dst).with_context(|| format!("copying {}", src.display()))?;
            } else {
                bail!(
                    "{} is neither a file, a directory nor a link",
                    src.display()
                );
            }
        }
        Ok(())
    }
    walk(from, to, true)
}

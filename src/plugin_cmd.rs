//! `rrecall plugin list | add | trust` (spec §6.7). None of them runs a plugin's command: the
//! manifest preview and the user's answer are the safety step.

use crate::config;
use crate::error::{self, Fail};
use crate::plugin::{self, Manifest, Plugin, Spec};
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    let tree = stage.0.join("tree");
    std::fs::create_dir_all(&stage.0).with_context(|| format!("creating {}", stage.0.display()))?;
    let local = Path::new(source);
    // A local path is recorded and cloned absolute: git runs in the staging directory.
    let source = match local.exists() {
        true => local
            .canonicalize()
            .with_context(|| format!("reading {source}"))?
            .to_string_lossy()
            .into_owned(),
        false => source.to_string(),
    };
    let commit = if local.exists() && git_ref.is_none() {
        copy_tree(Path::new(&source), &tree)?;
        None
    } else {
        Some(fetch(&source, git_ref, &stage.0, &tree)?)
    };
    // limit: a source holds one plugin, at its root; a repository of several plugins is not
    // installable until `add` takes a subdirectory.
    let manifest = plugin::load_manifest(&tree)?;
    let id = &manifest.id;
    let dest = plugins.join(id);
    // Discovery knows a plugin by its manifest id, whatever its directory is called.
    let installed = plugin::discover(std::slice::from_ref(&plugins))?;
    // limit: no update in place; reinstalling means removing the directory and the record.
    if let Some(have) = installed.get(id) {
        return Err(error::usage(format!(
            "plugin `{id}` is already installed at {}: remove it (and its [[installed]] entry in {}) to install it again",
            have.dir.display(),
            config::user_config_path()?.display()
        )));
    }
    if dest.exists() || user.installed.iter().any(|i| &i.id == id) {
        return Err(error::usage(format!(
            "plugin `{id}` is already installed: remove {} and its [[installed]] entry in {} to install it again",
            dest.display(),
            config::user_config_path()?.display()
        )));
    }
    print!("{}", preview(&manifest, &tree)?);
    if !confirm(&format!("Install {id} into {}? [y/N] ", dest.display()))? {
        return Err(declined(&format!("plugin `{id}` was not installed")));
    }
    std::fs::create_dir_all(&plugins).with_context(|| format!("creating {}", plugins.display()))?;
    std::fs::rename(&tree, &dest).with_context(|| format!("moving into {}", dest.display()))?;
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
pub fn trust(project: Option<&str>, dir: &Path) -> Result<String> {
    let dir = dir
        .canonicalize()
        .map_err(|e| error::usage(format!("{}: {e}", dir.display())))?;
    // The loader reads only the project's own `.rrecall/plugins/`; trusting any other
    // directory would record a decision nothing ever consults.
    let (name, store) = config::resolve_project(project)?;
    let repo = config::load_store_config(&store)?.repo.ok_or_else(|| {
        error::usage(format!(
            "project `{name}` names no repository (`repo` in {}), so no plugin directory of its own loads",
            store.join(config::STORE_CONFIG).display()
        ))
    })?;
    let expected = repo.join(".rrecall/plugins");
    if expected.canonicalize().ok().as_ref() != Some(&dir) {
        return Err(error::usage(format!(
            "{} is not the plugin directory of project `{name}`: that is {}",
            dir.display(),
            expected.display()
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

/// git as `add` runs it on a source nobody has vetted yet: only the https, ssh, git and file
/// transports (whatever the user's config allows), no hooks, no fsmonitor. It never checks
/// out, so no filter or checkout hook runs: [`fetch`] writes the tree from git's objects.
fn installer_git(dir: &Path, hooks: &Path) -> Command {
    let mut c = Command::new("git");
    c.current_dir(dir)
        // Overrides every protocol setting in git's config (git-config(1), GIT_ALLOW_PROTOCOL).
        .env("GIT_ALLOW_PROTOCOL", "https:ssh:git:file")
        .args(["-c", "protocol.allow=never"])
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()))
        .args(["-c", "core.fsmonitor=false"]);
    c
}

fn git_out(mut c: Command, what: &str) -> Result<Vec<u8>> {
    let o = c
        .output()
        .with_context(|| format!("running git to {what}"))?;
    if !o.status.success() {
        bail!(
            "git failed to {what}: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(o.stdout)
}

/// Clones `url` without a checkout under `stage`, and writes the tree of `git_ref` (else
/// `HEAD`) to `tree` from git's objects. Returns the commit.
fn fetch(url: &str, git_ref: Option<&str>, stage: &Path, tree: &Path) -> Result<String> {
    let hooks = stage.join("hooks");
    let repo = stage.join("repo");
    std::fs::create_dir_all(&hooks).with_context(|| format!("creating {}", hooks.display()))?;
    let mut c = installer_git(stage, &hooks);
    c.args(["clone", "-q", "--no-checkout", "--"])
        .arg(url)
        .arg(&repo);
    git_out(c, &format!("clone {url}"))?;
    let git = |args: &[&str], what: &str| {
        let mut c = installer_git(&repo, &hooks);
        c.args(args);
        git_out(c, what)
    };
    let want = format!("{}^{{commit}}", git_ref.unwrap_or("HEAD"));
    let commit = String::from_utf8(git(
        &["rev-parse", "--verify", "-q", &want],
        "find the ref",
    )?)?
    .trim()
    .to_string();
    let listing = git(
        &["ls-tree", "-r", "-z", "--full-tree", &commit],
        "list the tree",
    )?;
    std::fs::create_dir(tree).with_context(|| format!("creating {}", tree.display()))?;
    for item in listing.split(|b| *b == 0).filter(|i| !i.is_empty()) {
        let item = std::str::from_utf8(item).context("a path in the tree is not UTF-8")?;
        let (meta, rel) = item.split_once('\t').context("an unreadable tree entry")?;
        let mut meta = meta.split(' ');
        let (mode, sha) = (meta.next().unwrap_or(""), meta.nth(1).unwrap_or(""));
        if rel
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == ".." || p == ".git")
        {
            bail!("the tree holds the path `{rel}`, which add does not write");
        }
        let dst = tree.join(rel);
        if let Some(d) = dst.parent() {
            std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
        }
        let blob = || git(&["cat-file", "blob", sha], &format!("read {rel}"));
        match mode {
            "100644" | "100755" => {
                std::fs::write(&dst, blob()?)
                    .with_context(|| format!("writing {}", dst.display()))?;
                let bits = if mode == "100755" { 0o755 } else { 0o644 };
                std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(bits))?;
            }
            "120000" => {
                let target = String::from_utf8(blob()?).context("a link target is not UTF-8")?;
                std::os::unix::fs::symlink(target, &dst)
                    .with_context(|| format!("creating {}", dst.display()))?;
            }
            _ => bail!("`{rel}` is a submodule or another entry add does not install"),
        }
    }
    Ok(commit)
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

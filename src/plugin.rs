//! Plugins (spec §6.1, §6.2, §6.7): the manifest, discovery and trust, and the instances a
//! store's config enables.

use crate::config::{self, BranchDef, PluginEntry, StoreConfig, UserConfig};
use crate::error;
use crate::node::{is_kind, is_slug};
use crate::wire::{Event, Params};
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const MANIFEST: &str = "rrecall-plugin.toml";

/// Source kinds the core owns: branches cite the config, imported nodes their outline.
pub const CORE_KINDS: [&str; 2] = ["config", "import"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Collector,
    Check,
    Judge,
    View,
    Runner,
    Hook,
}

/// `rrecall-plugin.toml` as written; [`load_manifest`] checks it into a [`Manifest`].
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    id: String,
    name: String,
    version: String,
    min_rrecall: String,
    kind: Kind,
    #[serde(default)]
    requires: Vec<String>,
    collector: Option<CollectorSpec>,
    check: Option<CommandSpec>,
    judge: Option<JudgeSpec>,
    view: Option<CommandSpec>,
    runner: Option<CommandSpec>,
    hook: Option<HookSpec>,
    #[serde(default)]
    params: toml::Table,
}

#[derive(Debug, Clone)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub min_rrecall: String,
    pub requires: Vec<String>,
    /// Param names and their defaults; an instance may set only these, with the same type.
    pub params: toml::Table,
    pub spec: Spec,
}

/// The kind's own table.
#[derive(Debug, Clone)]
pub enum Spec {
    Collector(CollectorSpec),
    Check(CommandSpec),
    Judge(JudgeSpec),
    View(CommandSpec),
    Runner(CommandSpec),
    Hook(HookSpec),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectorSpec {
    pub scope: Vec<String>,
    pub source_kinds: Vec<String>,
    #[serde(default)]
    pub fingerprint: Option<Vec<String>>,
    #[serde(default, deserialize_with = "config::duration_opt")]
    pub min_interval: Option<Duration>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub resolve: Option<Vec<String>>,
    #[serde(default)]
    pub resolve_prompt: Option<String>,
    #[serde(default)]
    pub runner: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default, deserialize_with = "config::duration_opt")]
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub command: Vec<String>,
    #[serde(default, deserialize_with = "config::duration_opt")]
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeSpec {
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub runner: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default, deserialize_with = "config::duration_opt")]
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSpec {
    pub events: Vec<Event>,
    pub command: Vec<String>,
    #[serde(default, deserialize_with = "config::duration_opt")]
    pub timeout: Option<Duration>,
}

impl Manifest {
    pub fn kind(&self) -> Kind {
        match self.spec {
            Spec::Collector(_) => Kind::Collector,
            Spec::Check(_) => Kind::Check,
            Spec::Judge(_) => Kind::Judge,
            Spec::View(_) => Kind::View,
            Spec::Runner(_) => Kind::Runner,
            Spec::Hook(_) => Kind::Hook,
        }
    }

    /// The manifest's timeout, else 30s for a hook (spec §6.6) and 10m for the rest.
    pub fn timeout(&self) -> Duration {
        let t = match &self.spec {
            Spec::Collector(c) => c.timeout,
            Spec::Check(c) | Spec::View(c) | Spec::Runner(c) => c.timeout,
            Spec::Judge(j) => j.timeout,
            Spec::Hook(h) => h.timeout,
        };
        t.unwrap_or(Duration::from_secs(match self.kind() {
            Kind::Hook => 30,
            _ => 600,
        }))
    }

    /// Whether a runner runs part of it (a `prompt` or `resolve_prompt`).
    pub fn declarative(&self) -> bool {
        match &self.spec {
            Spec::Collector(c) => c.prompt.is_some() || c.resolve_prompt.is_some(),
            Spec::Judge(j) => j.prompt.is_some(),
            _ => false,
        }
    }

    /// Every argv the manifest declares, with its key.
    fn argvs(&self) -> Vec<(&'static str, &[String])> {
        let all: Vec<(&'static str, Option<&Vec<String>>)> = match &self.spec {
            Spec::Collector(c) => vec![
                ("fingerprint", c.fingerprint.as_ref()),
                ("command", c.command.as_ref()),
                ("resolve", c.resolve.as_ref()),
            ],
            Spec::Judge(j) => vec![("command", j.command.as_ref())],
            Spec::Check(c) | Spec::View(c) | Spec::Runner(c) => vec![("command", Some(&c.command))],
            Spec::Hook(h) => vec![("command", Some(&h.command))],
        };
        all.into_iter()
            .filter_map(|(k, a)| a.map(|a| (k, a.as_slice())))
            .collect()
    }
}

/// Reads and checks `<dir>/rrecall-plugin.toml`. Errors name the plugin and the key.
pub fn load_manifest(dir: &Path) -> Result<Manifest> {
    let dir = &dir
        .canonicalize()
        .with_context(|| format!("reading {}", dir.display()))?;
    let path = dir.join(MANIFEST);
    let real = path
        .canonicalize()
        .with_context(|| format!("reading {}", path.display()))?;
    if !real.starts_with(dir) {
        bail!(
            "{} resolves to {}, outside the plugin directory",
            path.display(),
            real.display()
        );
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    parse_manifest(&text, dir, &path.display().to_string())
}

/// Parses and checks manifest `text`; `origin` names where it came from in errors.
fn parse_manifest(text: &str, dir: &Path, origin: &str) -> Result<Manifest> {
    let raw: RawManifest =
        toml::from_str(text).with_context(|| format!("{origin} does not parse"))?;
    let id = raw.id.clone();
    check_manifest(raw, dir).map_err(|e| anyhow!("plugin `{id}` ({origin}): {e}"))
}

/// The plugins shipped inside the binary, the first place of spec §6.2. Each runs the running
/// binary itself (`rrecall builtin <id>`), by absolute path and never through `PATH`, in the
/// binary's directory.
pub fn builtin() -> Result<BTreeMap<String, Plugin>> {
    // limit: in a test harness the running binary is the harness, which has no `builtin`
    // subcommand; tests that run a built-in point its program at the rrecall binary.
    let exe = std::env::current_exe().context("locating the rrecall binary")?;
    let exe_text = exe
        .to_str()
        .with_context(|| format!("the rrecall binary's path {} is not UTF-8", exe.display()))?;
    let dir = exe
        .parent()
        .context("the rrecall binary has no directory")?
        .to_path_buf();
    let mut found = BTreeMap::new();
    for text in crate::checks::MANIFESTS
        .iter()
        .chain(crate::runner::MANIFESTS.iter())
    {
        let mut manifest = parse_manifest(text, &dir, "built in")?;
        match &mut manifest.spec {
            Spec::Check(c) | Spec::Runner(c) => c.command[0] = exe_text.to_string(),
            _ => {}
        }
        found.insert(
            manifest.id.clone(),
            Plugin {
                manifest,
                dir: dir.clone(),
                builtin: true,
            },
        );
    }
    Ok(found)
}

fn check_manifest(raw: RawManifest, dir: &Path) -> Result<Manifest, String> {
    if !is_slug(&raw.id) {
        return Err(format!("id `{}` is not kebab-case", raw.id));
    }
    if raw.name.trim().is_empty() {
        return Err("name is empty".into());
    }
    parse_version(&raw.version).ok_or(format!("version `{}` is not x.y.z", raw.version))?;
    let min = parse_version(&raw.min_rrecall)
        .ok_or(format!("min_rrecall `{}` is not x.y.z", raw.min_rrecall))?;
    let ours = env!("CARGO_PKG_VERSION");
    if parse_version(ours).is_some_and(|v| v < min) {
        return Err(format!(
            "min_rrecall `{}` is newer than this rrecall ({ours})",
            raw.min_rrecall
        ));
    }
    if raw.requires.iter().any(|r| r.trim().is_empty()) {
        return Err("requires: an entry is empty".into());
    }
    let mut env_names = BTreeMap::new();
    for (name, default) in &raw.params {
        let ok = name.starts_with(|c: char| c.is_ascii_lowercase())
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if !ok {
            return Err(format!(
                "params: `{name}` is not a param name (a-z, 0-9, `_`, `-`)"
            ));
        }
        if let Some(what) = unfit(default) {
            return Err(format!(
                "params: `{name}` holds {what}, which JSON cannot hold"
            ));
        }
        if let Some(other) = env_names.insert(env_name(name), name) {
            return Err(format!(
                "params: `{other}` and `{name}` are both {}",
                env_name(name)
            ));
        }
    }
    let tables = [
        ("collector", raw.collector.is_some()),
        ("check", raw.check.is_some()),
        ("judge", raw.judge.is_some()),
        ("view", raw.view.is_some()),
        ("runner", raw.runner.is_some()),
        ("hook", raw.hook.is_some()),
    ];
    let kind_name = match raw.kind {
        Kind::Collector => "collector",
        Kind::Check => "check",
        Kind::Judge => "judge",
        Kind::View => "view",
        Kind::Runner => "runner",
        Kind::Hook => "hook",
    };
    if let Some((t, _)) = tables.iter().find(|(t, set)| *set && *t != kind_name) {
        return Err(format!("table [{t}] does not match kind `{kind_name}`"));
    }
    let missing = || format!("kind `{kind_name}` needs a [{kind_name}] table");
    let spec = match raw.kind {
        Kind::Collector => Spec::Collector(raw.collector.ok_or_else(missing)?),
        Kind::Check => Spec::Check(raw.check.ok_or_else(missing)?),
        Kind::Judge => Spec::Judge(raw.judge.ok_or_else(missing)?),
        Kind::View => Spec::View(raw.view.ok_or_else(missing)?),
        Kind::Runner => Spec::Runner(raw.runner.ok_or_else(missing)?),
        Kind::Hook => Spec::Hook(raw.hook.ok_or_else(missing)?),
    };
    let t = kind_name;
    match &spec {
        Spec::Collector(c) => {
            if c.scope.is_empty() {
                return Err(format!("[{t}] scope: name at least one branch"));
            }
            if let Some(b) = c.scope.iter().find(|b| !is_slug(b)) {
                return Err(format!("[{t}] scope: `{b}` is not a branch id"));
            }
            if c.source_kinds.is_empty() {
                return Err(format!("[{t}] source_kinds: name at least one kind"));
            }
            for k in &c.source_kinds {
                fill(k, &raw.params).map_err(|e| format!("[{t}] source_kinds: {e}"))?;
            }
            one_of(
                t,
                "fingerprint",
                c.fingerprint.is_some(),
                "min_interval",
                c.min_interval.is_some(),
            )?;
            one_of(
                t,
                "command",
                c.command.is_some(),
                "prompt",
                c.prompt.is_some(),
            )?;
            if c.resolve.is_some() && c.resolve_prompt.is_some() {
                return Err(format!(
                    "[{t}] has both `resolve` and `resolve_prompt`; keep one"
                ));
            }
            for (f, file) in [("prompt", &c.prompt), ("resolve_prompt", &c.resolve_prompt)] {
                if let Some(file) = file {
                    check_file(dir, file).map_err(|e| format!("[{t}] {f}: {e}"))?;
                }
            }
            if c.prompt.is_none() && c.resolve_prompt.is_none() {
                only_with_prompt(t, &c.runner, &c.model)?;
            }
        }
        Spec::Judge(j) => {
            one_of(
                t,
                "command",
                j.command.is_some(),
                "prompt",
                j.prompt.is_some(),
            )?;
            match &j.prompt {
                Some(file) => check_file(dir, file).map_err(|e| format!("[{t}] prompt: {e}"))?,
                None => only_with_prompt(t, &j.runner, &j.model)?,
            }
        }
        Spec::Hook(h) if h.events.is_empty() => {
            return Err(format!("[{t}] events: name at least one event"));
        }
        _ => {}
    }
    let manifest = Manifest {
        id: raw.id,
        name: raw.name,
        version: raw.version,
        min_rrecall: raw.min_rrecall,
        requires: raw.requires,
        params: raw.params,
        spec,
    };
    for (key, argv) in manifest.argvs() {
        if argv.first().is_none_or(|p| p.is_empty()) {
            return Err(format!("[{t}] {key}: the argv needs a program"));
        }
        for a in argv {
            fill(a, &manifest.params).map_err(|e| format!("[{t}] {key}: {e}"))?;
        }
    }
    Ok(manifest)
}

fn one_of(t: &str, a: &str, has_a: bool, b: &str, has_b: bool) -> Result<(), String> {
    match (has_a, has_b) {
        (true, false) | (false, true) => Ok(()),
        (true, true) => Err(format!("[{t}] has both `{a}` and `{b}`; keep one")),
        (false, false) => Err(format!("[{t}] needs `{a}` or `{b}`")),
    }
}

fn only_with_prompt(
    t: &str,
    runner: &Option<String>,
    model: &Option<String>,
) -> Result<(), String> {
    match (runner, model) {
        (Some(_), _) => Err(format!("[{t}] `runner` goes only with a prompt")),
        (_, Some(_)) => Err(format!("[{t}] `model` goes only with a prompt")),
        _ => Ok(()),
    }
}

/// A file named relative to the (canonical) plugin directory that exists and, links resolved,
/// stays inside it: what a plugin uses is what its content hash covers.
fn check_file(dir: &Path, file: &str) -> Result<(), String> {
    let p = Path::new(file);
    if p.is_absolute()
        || p.components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "`{file}` must be a path inside the plugin directory"
        ));
    }
    match dir.join(p).canonicalize() {
        Ok(real) if !real.starts_with(dir) => Err(format!(
            "`{file}` resolves to {}, outside the plugin directory",
            real.display()
        )),
        Ok(real) if real.is_file() => Ok(()),
        _ => Err(format!("`{file}` does not exist in {}", dir.display())),
    }
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.split('.').map(|p| {
        (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            .then(|| p.parse::<u64>().ok())
            .flatten()
    });
    let v = (it.next()??, it.next()??, it.next()??);
    it.next().is_none().then_some(v)
}

/// What a param value holds that its JSON form (input, env) cannot: params are JSON values.
fn unfit(v: &toml::Value) -> Option<&'static str> {
    match v {
        toml::Value::Datetime(_) => Some("a date-time"),
        toml::Value::Float(f) if !f.is_finite() => Some("a non-finite number"),
        toml::Value::Array(a) => a.iter().find_map(unfit),
        toml::Value::Table(t) => t.values().find_map(unfit),
        _ => None,
    }
}

/// `RRECALL_PARAM_<NAME>`: upper-cased, `-` to `_`.
fn env_name(param: &str) -> String {
    format!(
        "RRECALL_PARAM_{}",
        param.to_ascii_uppercase().replace('-', "_")
    )
}

/// A param as an environment value: a string as it is, anything else as JSON.
fn param_text(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        v => serde_json::to_string(v).unwrap_or_default(),
    }
}

/// `template` with each `{name}` replaced by the param's text. A `{...}` holding only param-name
/// characters must name a declared param; other braces (a jq filter) stay as they are.
fn fill(template: &str, params: &toml::Table) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let name_len = after
            .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'))
            .unwrap_or(after.len());
        if name_len > 0 && after[name_len..].starts_with('}') {
            let name = &after[..name_len];
            let v = params
                .get(name)
                .ok_or(format!("`{{{name}}}` in `{template}` names no param"))?;
            out.push_str(&param_text(v));
            rest = &after[name_len + 1..];
        } else {
            out.push('{');
            rest = after;
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// A discovered plugin.
#[derive(Debug, Clone)]
pub struct Plugin {
    pub manifest: Manifest,
    pub dir: PathBuf,
    /// Shipped inside the binary (spec §6.2); `dir` is then the binary's directory.
    pub builtin: bool,
}

/// The plugins under each of `places` (a subdirectory with a manifest is a plugin), by id; a
/// later place replaces an earlier one with the same id (spec §6.2).
pub fn discover(places: &[PathBuf]) -> Result<BTreeMap<String, Plugin>> {
    let mut found = BTreeMap::new();
    for place in places {
        let mut here: BTreeMap<String, Plugin> = BTreeMap::new();
        let entries = match std::fs::read_dir(place) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("reading {}", place.display())),
        };
        let mut dirs = entries
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("reading {}", place.display()))?;
        dirs.sort();
        let place = &place
            .canonicalize()
            .with_context(|| format!("reading {}", place.display()))?;
        for dir in dirs.into_iter().filter(|d| d.join(MANIFEST).is_file()) {
            let dir = dir
                .canonicalize()
                .with_context(|| format!("reading {}", dir.display()))?;
            if !dir.starts_with(place) || dir == *place {
                bail!(
                    "plugin directory {} resolves outside {}",
                    dir.display(),
                    place.display()
                );
            }
            let manifest = load_manifest(&dir)?;
            if let Some(other) = here.get(&manifest.id) {
                bail!(
                    "plugin `{}` is defined twice in {}: {} and {}",
                    manifest.id,
                    place.display(),
                    other.dir.display(),
                    dir.display()
                );
            }
            here.insert(
                manifest.id.clone(),
                Plugin {
                    manifest,
                    dir,
                    builtin: false,
                },
            );
        }
        found.extend(here);
    }
    Ok(found)
}

/// The plugin directories of spec §6.2 after the built-in place, in load order, and the
/// project's `.rrecall/plugins/` when it was skipped for want of trust.
#[derive(Debug)]
pub struct Places {
    pub dirs: Vec<PathBuf>,
    pub untrusted: Option<PathBuf>,
}

pub fn places(
    user: &UserConfig,
    cfg: &StoreConfig,
    store: &Path,
    user_plugins: &Path,
) -> Result<Places> {
    // The first place, the plugins inside the binary, has no directory: see [`builtin`].
    let mut dirs = vec![user_plugins.to_path_buf()];
    let mut untrusted = None;
    if let Some(repo) = &cfg.repo {
        if !repo.is_absolute() {
            bail!(
                "{}: repo `{}` is not an absolute path",
                config::STORE_CONFIG,
                repo.display()
            );
        }
        let dir = repo.join(".rrecall/plugins");
        if dir.is_dir() {
            let dir = dir
                .canonicalize()
                .with_context(|| format!("reading {}", dir.display()))?;
            let hash = content_hash(&dir)?;
            let trusted = user
                .trusted
                .iter()
                .any(|t| t.hash == hash && t.path.canonicalize().is_ok_and(|p| p == dir));
            if trusted {
                dirs.push(dir);
            } else {
                untrusted = Some(dir);
            }
        }
    }
    dirs.push(store.join("plugins"));
    Ok(Places { dirs, untrusted })
}

/// The content hash the lock records for `plugin` (spec §4.2, §6.7): a built-in has no
/// directory of its own, so its hash covers the rrecall version and its manifest.
pub fn plugin_hash(plugin: &Plugin) -> Result<String> {
    use sha2::{Digest, Sha256};
    if !plugin.builtin {
        return content_hash(&plugin.dir);
    }
    let mut text = None;
    for t in crate::checks::MANIFESTS {
        if parse_manifest(t, &plugin.dir, "built in")?.id == plugin.manifest.id {
            text = Some(t);
        }
    }
    let text = text.with_context(|| format!("no built-in plugin `{}`", plugin.manifest.id))?;
    let mut h = Sha256::new();
    h.update(format!("rrecall {}\0", env!("CARGO_PKG_VERSION")));
    h.update(text);
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("sha256:{hex}"))
}

/// `sha256:<hex>` over every entry under `dir`: its relative path, type, exec bit and content
/// (a symlink by its target, not followed).
pub fn content_hash(dir: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    fn walk(root: &Path, dir: &Path, h: &mut Sha256) -> Result<()> {
        let mut entries = std::fs::read_dir(dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.sort();
        for p in entries {
            let meta = std::fs::symlink_metadata(&p)?;
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .into_owned();
            let (tag, content): (&[u8], Vec<u8>) = if meta.file_type().is_symlink() {
                (
                    b"l",
                    std::fs::read_link(&p)?
                        .to_string_lossy()
                        .into_owned()
                        .into_bytes(),
                )
            } else if meta.is_dir() {
                (b"d", Vec::new())
            } else if meta.permissions().mode() & 0o111 != 0 {
                (
                    b"x",
                    std::fs::read(&p).with_context(|| format!("reading {}", p.display()))?,
                )
            } else {
                (
                    b"f",
                    std::fs::read(&p).with_context(|| format!("reading {}", p.display()))?,
                )
            };
            for part in [rel.as_bytes(), tag, &content] {
                h.update((part.len() as u64).to_le_bytes());
                h.update(part);
            }
            if meta.is_dir() {
                walk(root, &p, h)?;
            }
        }
        Ok(())
    }
    let mut h = Sha256::new();
    walk(dir, dir, &mut h)?;
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("sha256:{hex}"))
}

/// One enabled `[[plugins]]` entry, checked against its plugin (spec §6.2).
#[derive(Debug, Clone)]
pub struct Instance {
    /// What the lock, the report, the commit message and `--only` name.
    pub id: String,
    pub plugin: Plugin,
    /// The manifest's defaults overlaid with the entry's params.
    pub params: toml::Table,
    /// For a collector: the source kinds it owns, placeholders filled.
    pub source_kinds: Vec<String>,
    pub runner: Option<String>,
    pub model: Option<String>,
    pub timeout: Duration,
    pub allowed_runners: Option<Vec<String>>,
}

impl Instance {
    /// `template` with params filled. The program stays as written; the process runner resolves
    /// it (a relative path against the plugin directory, a bare name on `PATH`).
    pub fn argv(&self, template: &[String]) -> Vec<String> {
        // Every placeholder was checked against the declared params when the manifest loaded.
        // A built-in's program is the binary's path, set at run time, never a template.
        template
            .iter()
            .enumerate()
            .map(|(i, a)| {
                if i == 0 && self.plugin.builtin {
                    a.clone()
                } else {
                    fill(a, &self.params).unwrap_or_else(|_| a.clone())
                }
            })
            .collect()
    }

    pub fn params_json(&self) -> Params {
        self.params
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::to_value(v).unwrap_or_default()))
            .collect()
    }

    /// `RRECALL_PARAM_<NAME>` and the value, sorted by name.
    pub fn env_params(&self) -> Vec<(String, String)> {
        let mut env: Vec<_> = self
            .params
            .iter()
            .map(|(k, v)| (env_name(k), param_text(v)))
            .collect();
        env.sort();
        env
    }
}

/// The store config's entries as instances, in config order. Errors name the instance.
pub fn instances(
    entries: &[PluginEntry],
    plugins: &BTreeMap<String, Plugin>,
    branches: &[BranchDef],
) -> Result<Vec<Instance>> {
    let mut out: Vec<Instance> = Vec::new();
    for e in entries {
        let inst = instance(e, plugins, branches)
            .map_err(|m| anyhow!("{}: plugin instance `{}`: {m}", config::STORE_CONFIG, e.id))?;
        if out.iter().any(|i| i.id == inst.id) {
            bail!(
                "{}: plugin instance `{}` is defined twice",
                config::STORE_CONFIG,
                inst.id
            );
        }
        for k in &inst.source_kinds {
            if let Some(other) = owner(&out, k) {
                bail!(
                    "{}: plugin instances `{}` and `{}` both own the source kind `{k}`",
                    config::STORE_CONFIG,
                    other.id,
                    inst.id
                );
            }
        }
        out.push(inst);
    }
    Ok(out)
}

fn instance(
    e: &PluginEntry,
    plugins: &BTreeMap<String, Plugin>,
    branches: &[BranchDef],
) -> Result<Instance, String> {
    if !is_slug(&e.id) {
        return Err(format!("id `{}` is not kebab-case", e.id));
    }
    let uses = e.uses.as_deref().unwrap_or(&e.id);
    let plugin = plugins
        .get(uses)
        .ok_or(format!("no plugin `{uses}` was found"))?
        .clone();
    let m = &plugin.manifest;
    let mut params = m.params.clone();
    for (k, v) in &e.params {
        let default = m
            .params
            .get(k)
            .ok_or(format!("plugin `{uses}` has no param `{k}`"))?;
        if v.type_str() != default.type_str() {
            return Err(format!(
                "param `{k}` must be a {}, as its default in plugin `{uses}`",
                default.type_str()
            ));
        }
        if let Some(what) = unfit(v) {
            return Err(format!("param `{k}` holds {what}, which JSON cannot hold"));
        }
        params.insert(k.clone(), v.clone());
    }
    if (e.runner.is_some() || e.model.is_some()) && !m.declarative() {
        return Err(format!(
            "`runner` and `model` apply only to a plugin with a prompt; `{uses}` has none"
        ));
    }
    let mut source_kinds = Vec::new();
    if let Spec::Collector(c) = &m.spec {
        if let Some(b) = c
            .scope
            .iter()
            .find(|b| !branches.iter().any(|x| &x.id == *b))
        {
            return Err(format!(
                "plugin `{uses}` scope: the map has no branch `{b}`"
            ));
        }
        for k in &c.source_kinds {
            let k = fill(k, &params)?;
            if !is_kind(&k) {
                return Err(format!("source kind `{k}` is not a kind (a-z, 0-9, `-`)"));
            }
            if CORE_KINDS.contains(&k.as_str()) {
                return Err(format!("source kind `{k}` belongs to rrecall itself"));
            }
            source_kinds.push(k);
        }
    } else if e.allowed_runners.is_some() {
        return Err("`allowed_runners` applies only to a collector".into());
    }
    Ok(Instance {
        id: e.id.clone(),
        timeout: e.timeout.unwrap_or_else(|| m.timeout()),
        runner: e.runner.clone(),
        model: e.model.clone(),
        allowed_runners: e.allowed_runners.clone(),
        params,
        source_kinds,
        plugin,
    })
}

/// The instance that owns a source kind (spec §3.3).
pub fn owner<'a>(instances: &'a [Instance], kind: &str) -> Option<&'a Instance> {
    instances
        .iter()
        .find(|i| i.source_kinds.iter().any(|k| k == kind))
}

/// Step 1 of a run (spec §4.1): the store's enabled plugin instances. Any error exits 4.
pub fn load(store: &Path) -> Result<Vec<Instance>> {
    let user = (|| -> Result<(UserConfig, PathBuf)> {
        let plugins = config::user_config_path()?
            .parent()
            .map(|d| d.join("plugins"))
            .context("the user config has no directory")?;
        Ok((config::load_user_config()?, plugins))
    })();
    match user {
        Ok((user, plugins)) => load_with(store, &user, &plugins),
        Err(e) => Err(error::aborted(format!("{e:#}"))),
    }
}

pub fn load_with(store: &Path, user: &UserConfig, user_plugins: &Path) -> Result<Vec<Instance>> {
    let loaded = (|| {
        let cfg = config::load_store_config(store)?;
        let places = places(user, &cfg, store, user_plugins)?;
        let mut plugins = builtin()?;
        plugins.extend(discover(&places.dirs)?);
        if let Some(d) = &places.untrusted
            && let Some(e) = cfg
                .plugins
                .iter()
                .find(|e| !plugins.contains_key(e.uses.as_deref().unwrap_or(&e.id)))
        {
            bail!(
                "{}: plugin instance `{}`: no plugin `{}` was found; {} holds plugins that are not trusted: run `rrecall plugin trust {}`",
                config::STORE_CONFIG,
                e.id,
                e.uses.as_deref().unwrap_or(&e.id),
                d.display(),
                d.display()
            );
        }
        instances(&cfg.plugins, &plugins, &cfg.branches()?)
    })();
    loaded.map_err(|e| error::aborted(format!("{e:#}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{StoreConfig, Trusted, UserConfig};
    use crate::testutil::TempDir;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    const GITHUB: &str = r#"
id = "github"
name = "GitHub pull requests and issues"
version = "0.1.0"
min_rrecall = "0.1.0"
kind = "collector"
requires = ["gh"]

[collector]
scope = ["done", "blocked"]
source_kinds = ["gh-pr", "gh-issue"]
fingerprint = ["gh", "api", "repos/{repo}/events", "--jq", ".[0].id"]
resolve = ["./resolve.sh"]
command = ["./collect.sh", "{repo}"]
timeout = "5m"

[params]
repo = ""
"#;

    fn head(id: &str, kind: &str) -> String {
        format!(
            "id = \"{id}\"\nname = \"N\"\nversion = \"1.0.0\"\nmin_rrecall = \"0.1.0\"\nkind = \"{kind}\"\n"
        )
    }

    /// `<root>/<dir>/rrecall-plugin.toml` with `text`; returns the plugin dir.
    fn put(root: &Path, dir: &str, text: &str) -> PathBuf {
        let d = root.join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(MANIFEST), text).unwrap();
        d
    }

    fn manifest_err(text: &str) -> String {
        let t = TempDir::new();
        let d = put(&t.0, "p", text);
        format!("{:#}", load_manifest(&d).unwrap_err())
    }

    #[test]
    fn collector_manifest_of_the_spec() {
        let t = TempDir::new();
        let m = load_manifest(&put(&t.0, "gh", GITHUB)).unwrap();
        assert_eq!(m.id, "github");
        assert_eq!(m.kind(), Kind::Collector);
        assert_eq!(m.requires, ["gh"]);
        assert_eq!(m.timeout(), Duration::from_secs(300));
        let Spec::Collector(c) = &m.spec else {
            panic!("not a collector")
        };
        assert_eq!(c.source_kinds, ["gh-pr", "gh-issue"]);
        assert_eq!(c.command.as_deref().unwrap()[0], "./collect.sh");
        assert_eq!(c.min_interval, None);
    }

    #[test]
    fn every_kind_has_its_table() {
        let t = TempDir::new();
        std::fs::create_dir_all(t.0.join("j")).unwrap();
        std::fs::write(t.0.join("j/prompt.md"), "judge").unwrap();
        for (dir, text, kind, timeout) in [
            (
                "c",
                format!("{}[check]\ncommand = [\"./c\"]\n", head("c", "check")),
                Kind::Check,
                600,
            ),
            (
                "j",
                format!(
                    "{}[judge]\nprompt = \"prompt.md\"\nrunner = \"claude\"\n",
                    head("j", "judge")
                ),
                Kind::Judge,
                600,
            ),
            (
                "v",
                format!(
                    "{}[view]\ncommand = [\"./v\"]\ntimeout = \"1m\"\n",
                    head("v", "view")
                ),
                Kind::View,
                60,
            ),
            (
                "r",
                format!("{}[runner]\ncommand = [\"./r\"]\n", head("r", "runner")),
                Kind::Runner,
                600,
            ),
            (
                "h",
                format!(
                    "{}[hook]\nevents = [\"run.before\", \"commit.after\"]\ncommand = [\"./h\"]\n",
                    head("h", "hook")
                ),
                Kind::Hook,
                30,
            ),
            (
                "s",
                format!(
                    "{}[collector]\nscope = [\"people\"]\nsource_kinds = [\"slack\"]\nmin_interval = \"1d\"\nprompt = \"p.md\"\n",
                    head("s", "collector")
                ),
                Kind::Collector,
                600,
            ),
        ] {
            let d = put(&t.0, dir, &text);
            if dir == "s" {
                std::fs::write(d.join("p.md"), "x").unwrap();
            }
            let m = load_manifest(&d).unwrap_or_else(|e| panic!("{dir}: {e:#}"));
            assert_eq!(m.kind(), kind, "{dir}");
            assert_eq!(m.timeout(), Duration::from_secs(timeout), "{dir}");
        }
    }

    #[test]
    fn manifest_errors_name_the_plugin_and_the_key() {
        let col =
            |body: &str| format!("{}{body}\n[params]\nrepo = \"\"\n", head("gh", "collector"));
        let base = "[collector]\nscope = [\"done\"]\nsource_kinds = [\"gh-pr\"]\n";
        for (text, want) in [
            (
                GITHUB.replace("kind = ", "colour = \"red\"\nkind = "),
                "colour",
            ),
            (GITHUB.replace("timeout = \"5m\"", "retries = 3"), "retries"),
            (
                GITHUB.replace("[params]", "[params]\nwhen = 2026-01-01"),
                "when",
            ),
            (
                format!("{}[check]\ncommand = [\"./c\"]\n", head("v", "view")),
                "[check]",
            ),
            (head("gh", "collector"), "[collector]"),
            (head("gh", "plugin"), "kind"),
            (GITHUB.replace("id = \"github\"", "id = \"Git Hub\""), "id"),
            (
                GITHUB.replace("min_rrecall = \"0.1.0\"", "min_rrecall = \"99.0.0\""),
                "min_rrecall",
            ),
            (
                GITHUB.replace("version = \"0.1.0\"", "version = \"one\""),
                "version",
            ),
            (GITHUB.replace("{repo}/events", "{owner}/events"), "{owner}"),
            (
                GITHUB.replace("timeout = \"5m\"", "timeout = \"5 minutes\""),
                "timeout",
            ),
            (col(&format!("{base}command = [\"./c\"]")), "fingerprint"),
            (
                col(&format!(
                    "{base}fingerprint = [\"f\"]\nmin_interval = \"1d\"\ncommand = [\"./c\"]"
                )),
                "min_interval",
            ),
            (col(&format!("{base}fingerprint = [\"f\"]")), "command"),
            (
                col(&format!(
                    "{base}fingerprint = [\"f\"]\ncommand = [\"./c\"]\nprompt = \"p.md\""
                )),
                "prompt",
            ),
            (
                col(&format!(
                    "{base}fingerprint = [\"f\"]\ncommand = [\"./c\"]\nrunner = \"claude\""
                )),
                "runner",
            ),
            (
                col(&format!("{base}fingerprint = [\"f\"]\ncommand = []")),
                "command",
            ),
            (
                col(&format!(
                    "{base}fingerprint = [\"f\"]\ncommand = [\"./c\"]\nresolve = [\"./r\"]\nresolve_prompt = \"r.md\""
                )),
                "resolve",
            ),
            (
                col(&format!(
                    "{base}fingerprint = [\"f\"]\nprompt = \"missing.md\""
                )),
                "missing.md",
            ),
            (
                col(&format!(
                    "{base}fingerprint = [\"f\"]\nprompt = \"../outside.md\""
                )),
                "outside.md",
            ),
            (
                col(
                    "[collector]\nscope = []\nsource_kinds = [\"x\"]\nfingerprint = [\"f\"]\ncommand = [\"./c\"]",
                ),
                "scope",
            ),
            (
                col(
                    "[collector]\nscope = [\"done\"]\nsource_kinds = []\nfingerprint = [\"f\"]\ncommand = [\"./c\"]",
                ),
                "source_kinds",
            ),
            (
                format!(
                    "{}[hook]\nevents = []\ncommand = [\"./h\"]\n",
                    head("h", "hook")
                ),
                "events",
            ),
            (
                format!(
                    "{}[hook]\nevents = [\"run.after\"]\ncommand = [\"./h\"]\n",
                    head("h", "hook")
                ),
                "run.after",
            ),
            (
                format!(
                    "{}[judge]\ncommand = [\"./j\"]\nmodel = \"m\"\n",
                    head("j", "judge")
                ),
                "model",
            ),
            (
                format!(
                    "{}[check]\ncommand = [\"./c\"]\n[params]\n\"Bad Name\" = 1\n",
                    head("c", "check")
                ),
                "Bad Name",
            ),
            (
                format!(
                    "{}[check]\ncommand = [\"./c\"]\n[params]\na-b = 1\na_b = 2\n",
                    head("c", "check")
                ),
                "RRECALL_PARAM_A_B",
            ),
            ("not toml [".to_string(), MANIFEST),
        ] {
            let e = manifest_err(&text);
            assert!(e.contains(want), "want `{want}` in: {e}");
            assert!(
                e.contains("plugin `") || e.contains(MANIFEST),
                "names the plugin: {e}"
            );
        }
    }

    #[test]
    fn review_params_must_fit_json() {
        for bad in [
            "value = inf",
            "value = nan",
            "value = [1.0, -inf]",
            "value = { a = nan }",
        ] {
            let e = manifest_err(&format!(
                "{}[check]\ncommand = [\"./c\"]\n[params]\n{bad}\n",
                head("c", "check")
            ));
            assert!(e.contains("`value`") && e.contains("JSON"), "{bad}: {e}");
        }
        let t = TempDir::new();
        let root = t.0.join("plugins");
        put(
            &root,
            "c",
            &format!(
                "{}[check]\ncommand = [\"./c\"]\n[params]\nx = 1.0\n",
                head("c", "check")
            ),
        );
        let cfg = entries("[[plugins]]\nid = \"c\"\nparams = { x = inf }\n");
        let e = format!(
            "{:#}",
            instances(
                &cfg.plugins,
                &discover(&[root]).unwrap(),
                &cfg.branches().unwrap()
            )
            .unwrap_err()
        );
        assert!(e.contains("`x`") && e.contains("JSON"), "{e}");
    }

    #[test]
    fn review_plugin_files_stay_inside_the_plugin_tree() {
        let t = TempDir::new();
        let outside = t.0.join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            outside.join("m.toml"),
            format!("{}[check]\ncommand = [\"./c\"]\n", head("c", "check")),
        )
        .unwrap();
        std::fs::write(outside.join("p.md"), "x").unwrap();
        // A manifest that links out of the plugin dir.
        let place = t.0.join("p1");
        std::fs::create_dir_all(place.join("c")).unwrap();
        std::os::unix::fs::symlink(outside.join("m.toml"), place.join("c").join(MANIFEST)).unwrap();
        let e = format!("{:#}", discover(&[place]).unwrap_err());
        assert!(e.contains("outside"), "{e}");
        // A plugin dir that links out of its place.
        let place = t.0.join("p2");
        std::fs::create_dir_all(&place).unwrap();
        std::fs::copy(outside.join("m.toml"), outside.join(MANIFEST)).unwrap();
        std::os::unix::fs::symlink(&outside, place.join("c")).unwrap();
        let e = format!("{:#}", discover(&[place]).unwrap_err());
        assert!(e.contains("outside"), "{e}");
        // A prompt that links out of the plugin dir.
        let d = put(
            &t.0.join("p3"),
            "j",
            &format!("{}[judge]\nprompt = \"p.md\"\n", head("j", "judge")),
        );
        std::os::unix::fs::symlink(outside.join("p.md"), d.join("p.md")).unwrap();
        let e = manifest_err_at(&d);
        assert!(e.contains("p.md") && e.contains("outside"), "{e}");
    }

    fn manifest_err_at(d: &Path) -> String {
        format!("{:#}", load_manifest(d).unwrap_err())
    }

    #[test]
    fn later_places_replace_earlier_ones() {
        let t = TempDir::new();
        let (user, store) = (t.0.join("user"), t.0.join("store"));
        put(
            &user,
            "a",
            &format!("{}[view]\ncommand = [\"./user\"]\n", head("a", "view")),
        );
        put(
            &user,
            "b",
            &format!("{}[view]\ncommand = [\"./b\"]\n", head("b", "view")),
        );
        put(
            &store,
            "a2",
            &format!("{}[view]\ncommand = [\"./store\"]\n", head("a", "view")),
        );
        std::fs::create_dir_all(user.join("not-a-plugin")).unwrap();
        let found = discover(&[user.clone(), store.clone(), t.0.join("absent")]).unwrap();
        assert_eq!(found.keys().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(found["a"].dir, store.join("a2").canonicalize().unwrap());
        // Two plugins with one id in one place are ambiguous.
        put(
            &store,
            "a3",
            &format!("{}[view]\ncommand = [\"./x\"]\n", head("a", "view")),
        );
        let e = format!("{:#}", discover(&[store]).unwrap_err());
        assert!(
            e.contains("`a`") && e.contains("a2") && e.contains("a3"),
            "{e}"
        );
    }

    #[test]
    fn project_plugins_load_only_when_trusted_at_their_hash() {
        let t = TempDir::new();
        let repo = t.0.join("repo");
        let dir = put(
            &repo.join(".rrecall/plugins"),
            "lint",
            &format!("{}[check]\ncommand = [\"./c\"]\n", head("lint", "check")),
        );
        let plugins = repo.join(".rrecall/plugins").canonicalize().unwrap();
        let cfg = StoreConfig {
            repo: Some(repo.clone()),
            ..StoreConfig::default()
        };
        let mut user = UserConfig::default();
        let p = places(&user, &cfg, &t.0.join("store"), &t.0.join("user")).unwrap();
        assert_eq!(p.untrusted.as_deref(), Some(plugins.as_path()));
        assert!(!p.dirs.contains(&plugins));

        user.trusted.push(Trusted {
            path: plugins.clone(),
            hash: content_hash(&plugins).unwrap(),
        });
        let p = places(&user, &cfg, &t.0.join("store"), &t.0.join("user")).unwrap();
        assert_eq!(p.untrusted, None);
        assert_eq!(
            p.dirs,
            [t.0.join("user"), plugins.clone(), t.0.join("store/plugins")]
        );

        // Any change to the content needs trust again.
        std::fs::write(dir.join("c"), "#!/bin/sh\n").unwrap();
        let p = places(&user, &cfg, &t.0.join("store"), &t.0.join("user")).unwrap();
        assert_eq!(p.untrusted.as_deref(), Some(plugins.as_path()));
    }

    #[test]
    fn content_hash_sees_names_content_and_exec_bit() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempDir::new();
        std::fs::create_dir_all(t.0.join("sub")).unwrap();
        std::fs::write(t.0.join("sub/a"), "x").unwrap();
        let h0 = content_hash(&t.0).unwrap();
        assert!(h0.starts_with("sha256:") && h0.len() == 7 + 64, "{h0}");
        assert_eq!(content_hash(&t.0).unwrap(), h0, "stable");
        std::fs::set_permissions(t.0.join("sub/a"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let h1 = content_hash(&t.0).unwrap();
        assert_ne!(h1, h0);
        std::fs::rename(t.0.join("sub/a"), t.0.join("sub/b")).unwrap();
        assert_ne!(content_hash(&t.0).unwrap(), h1);
        std::os::unix::fs::symlink("/etc/hosts", t.0.join("link")).unwrap();
        let h2 = content_hash(&t.0).unwrap();
        std::fs::remove_file(t.0.join("link")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", t.0.join("link")).unwrap();
        assert_ne!(
            content_hash(&t.0).unwrap(),
            h2,
            "a symlink hashes by its target"
        );
    }

    fn catalog(t: &TempDir) -> BTreeMap<String, Plugin> {
        let root = t.0.join("plugins");
        put(&root, "github", GITHUB);
        put(
            &root,
            "files",
            &format!(
                "{}[collector]\nscope = [\"layout\"]\nsource_kinds = [\"{{ref_kind}}\"]\nfingerprint = [\"./fp\"]\ncommand = [\"./collect\"]\n[params]\npath = \"\"\ngit = true\nref_kind = \"file\"\nexclude = [\"*.lock\"]\n",
                head("files", "collector")
            ),
        );
        put(
            &root,
            "audit",
            &format!(
                "{}[hook]\nevents = [\"run.before\"]\ncommand = [\"./h\"]\n",
                head("audit", "hook")
            ),
        );
        discover(&[root]).unwrap()
    }

    fn entries(toml_text: &str) -> StoreConfig {
        toml::from_str(toml_text).unwrap()
    }

    #[test]
    fn instances_in_config_order_with_effective_params() {
        let t = TempDir::new();
        let cfg = entries(
            "[[plugins]]\nid = \"audit\"\n\n[[plugins]]\nid = \"repo\"\nuse = \"files\"\nparams = { path = \"/src\" }\n\n[[plugins]]\nid = \"notes\"\nuse = \"files\"\nparams = { path = \"/n\", git = false, ref_kind = \"note\" }\ntimeout = \"1h\"\n\n[[plugins]]\nid = \"github\"\nparams = { repo = \"o/r\" }\n",
        );
        let branches = cfg.branches().unwrap();
        let ins = instances(&cfg.plugins, &catalog(&t), &branches).unwrap();
        let ids: Vec<_> = ins.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["audit", "repo", "notes", "github"]);
        assert_eq!(ins[1].plugin.manifest.id, "files");
        assert_eq!(ins[1].source_kinds, ["file"]);
        assert_eq!(ins[2].source_kinds, ["note"]);
        assert_eq!(ins[2].timeout, Duration::from_secs(3600));
        assert_eq!(ins[1].timeout, Duration::from_secs(600));
        assert_eq!(owner(&ins, "note").map(|i| i.id.as_str()), Some("notes"));
        assert_eq!(owner(&ins, "gh-pr").map(|i| i.id.as_str()), Some("github"));
        assert!(owner(&ins, "slack").is_none());
        let p = ins[2].params_json();
        assert_eq!(p["git"], serde_json::json!(false));
        assert_eq!(p["exclude"], serde_json::json!(["*.lock"]));
        assert_eq!(
            ins[2].env_params(),
            [
                (
                    "RRECALL_PARAM_EXCLUDE".to_string(),
                    "[\"*.lock\"]".to_string()
                ),
                ("RRECALL_PARAM_GIT".into(), "false".into()),
                ("RRECALL_PARAM_PATH".into(), "/n".into()),
                ("RRECALL_PARAM_REF_KIND".into(), "note".into()),
            ]
        );
        let gh = &ins[3];
        let Spec::Collector(c) = &gh.plugin.manifest.spec else {
            panic!()
        };
        assert_eq!(
            gh.argv(c.fingerprint.as_deref().unwrap()),
            ["gh", "api", "repos/o/r/events", "--jq", ".[0].id"]
        );
        let argv = gh.argv(c.command.as_deref().unwrap());
        assert_eq!(argv[0], "./collect.sh");
        assert_eq!(argv[1], "o/r");
    }

    #[test]
    fn instance_errors_name_the_instance() {
        let t = TempDir::new();
        let cat = catalog(&t);
        for (text, want) in [
            ("[[plugins]]\nid = \"slack\"\n", "`slack`"),
            ("[[plugins]]\nid = \"x\"\nuse = \"nope\"\n", "`nope`"),
            (
                "[[plugins]]\nid = \"github\"\nparams = { owner = \"o\" }\n",
                "owner",
            ),
            (
                "[[plugins]]\nid = \"github\"\nparams = { repo = 1 }\n",
                "repo",
            ),
            ("[[plugins]]\nid = \"Bad Id\"\nuse = \"github\"\n", "Bad Id"),
            (
                "[[plugins]]\nid = \"github\"\n[[plugins]]\nid = \"github\"\n",
                "twice",
            ),
            (
                "[[plugins]]\nid = \"a\"\nuse = \"files\"\n[[plugins]]\nid = \"b\"\nuse = \"files\"\n",
                "`file`",
            ),
            (
                "[[plugins]]\nid = \"a\"\nuse = \"files\"\nparams = { ref_kind = \"import\" }\n",
                "`import`",
            ),
            (
                "[[plugins]]\nid = \"a\"\nuse = \"files\"\nparams = { ref_kind = \"Not A Kind\" }\n",
                "Not A Kind",
            ),
            (
                "[[plugins]]\nid = \"github\"\nrunner = \"codex\"\n",
                "runner",
            ),
            (
                "[[plugins]]\nid = \"audit\"\nallowed_runners = [\"claude\"]\n",
                "allowed_runners",
            ),
        ] {
            let cfg = entries(text);
            let e = format!(
                "{:#}",
                instances(&cfg.plugins, &cat, &cfg.branches().unwrap()).unwrap_err()
            );
            assert!(e.contains(want), "want `{want}` in: {e}");
        }
        // A scope branch the map does not have.
        let cfg = entries("[[plugins]]\nid = \"github\"\n");
        let e = format!(
            "{:#}",
            instances(&cfg.plugins, &cat, &crate::config::default_branches()[..2]).unwrap_err()
        );
        assert!(e.contains("github") && e.contains("`done`"), "{e}");
    }

    #[test]
    fn builtin_checks_load_first_and_a_plugin_with_their_id_replaces_them() {
        let t = TempDir::new();
        let (store, user) = (t.0.join("store"), t.0.join("user"));
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(
            store.join(crate::config::STORE_CONFIG),
            "[[plugins]]\nid = \"no-op\"\n\n[[plugins]]\nid = \"size-limits\"\nparams = { title_max = 60 }\n\n[[plugins]]\nid = \"secrets\"\n",
        )
        .unwrap();
        let ins = load_with(&store, &UserConfig::default(), &user).unwrap();
        let exe = std::env::current_exe().unwrap();
        for (i, id) in ["no-op", "size-limits", "secrets"].iter().enumerate() {
            let p = &ins[i].plugin;
            assert!(p.builtin, "{id}");
            assert_eq!(p.manifest.kind(), Kind::Check, "{id}");
            let Spec::Check(c) = &p.manifest.spec else {
                panic!("{id}: not a check")
            };
            // The running binary, never a `rrecall` found on PATH.
            assert_eq!(
                c.command,
                [exe.to_string_lossy().as_ref(), "builtin", id],
                "{id}"
            );
            assert_eq!(p.dir, exe.parent().unwrap(), "{id}");
        }
        assert_eq!(ins[1].params["title_max"].as_integer(), Some(60));
        assert_eq!(ins[1].params["body_max"].as_integer(), Some(600));
        // A user plugin with a built-in's id replaces it.
        put(
            &user,
            "my-secrets",
            &format!(
                "{}[check]\ncommand = [\"./scan\"]\n",
                head("secrets", "check")
            ),
        );
        let ins = load_with(&store, &UserConfig::default(), &user).unwrap();
        assert!(!ins[2].plugin.builtin);
        assert_eq!(
            ins[2].plugin.dir,
            user.join("my-secrets").canonicalize().unwrap()
        );
        // Without its entry in the store config, a built-in does not run.
        std::fs::write(store.join(crate::config::STORE_CONFIG), "").unwrap();
        assert!(
            load_with(&store, &UserConfig::default(), &user)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_builtin_program_path_is_never_expanded() {
        let mut plugins = builtin().unwrap();
        let p = plugins.get_mut("size-limits").unwrap();
        // A binary installed under a directory whose name looks like a param.
        let exe = "/opt/{title_max}/rrecall".to_string();
        let Spec::Check(c) = &mut p.manifest.spec else {
            panic!("not a check")
        };
        c.command[0] = exe.clone();
        let command = c.command.clone();
        let cfg: StoreConfig = toml::from_str("[[plugins]]\nid = \"size-limits\"\n").unwrap();
        let ins = instances(&cfg.plugins, &plugins, &cfg.branches().unwrap()).unwrap();
        assert_eq!(
            ins[0].argv(&command),
            [exe.as_str(), "builtin", "size-limits"]
        );
    }

    #[test]
    fn load_errors_exit_4() {
        let t = TempDir::new();
        let store = t.0.join("store");
        put(
            &store.join("plugins"),
            "gh",
            &GITHUB.replace("kind = ", "colour = \"red\"\nkind = "),
        );
        std::fs::write(
            store.join(crate::config::STORE_CONFIG),
            "[[plugins]]\nid = \"github\"\n",
        )
        .unwrap();
        let e = load_with(&store, &UserConfig::default(), &t.0.join("user")).unwrap_err();
        assert_eq!(crate::error::exit_code(&e), 4, "{e:#}");
        assert!(format!("{e:#}").contains("colour"), "{e:#}");

        put(&store.join("plugins"), "gh", GITHUB);
        let ins = load_with(&store, &UserConfig::default(), &t.0.join("user")).unwrap();
        assert_eq!(ins[0].id, "github");

        // An unknown plugin whose project dir is untrusted says how to trust it.
        let repo = t.0.join("repo");
        put(
            &repo.join(".rrecall/plugins"),
            "lint",
            &format!("{}[check]\ncommand = [\"./c\"]\n", head("lint", "check")),
        );
        std::fs::write(
            store.join(crate::config::STORE_CONFIG),
            format!(
                "repo = {}\n[[plugins]]\nid = \"lint\"\n",
                crate::config::toml_str(&repo.to_string_lossy())
            ),
        )
        .unwrap();
        let e = load_with(&store, &UserConfig::default(), &t.0.join("user")).unwrap_err();
        assert_eq!(crate::error::exit_code(&e), 4);
        assert!(format!("{e:#}").contains("not trusted"), "{e:#}");
        // Other errors do not blame trust.
        std::fs::write(
            store.join(crate::config::STORE_CONFIG),
            format!(
                "repo = {}\n[[plugins]]\nid = \"github\"\nparams = {{ owner = \"o\" }}\n",
                crate::config::toml_str(&repo.to_string_lossy())
            ),
        )
        .unwrap();
        let e = format!(
            "{:#}",
            load_with(&store, &UserConfig::default(), &t.0.join("user")).unwrap_err()
        );
        assert!(e.contains("owner") && !e.contains("trust"), "{e}");
    }
}

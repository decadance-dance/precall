//! The store config `rrecall.toml` and the user config (spec §9), and project resolution (§2).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const STORE_CONFIG: &str = "rrecall.toml";

/// A branch of the skeleton (spec §3.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchDef {
    pub id: String,
    pub title: String,
    pub question: String,
    #[serde(default)]
    pub granularity: String,
}

pub fn default_branches() -> Vec<BranchDef> {
    [
        (
            "layout",
            "Layout",
            "Where does something live?",
            "one node per component, directory or service",
        ),
        (
            "how-it-works",
            "How it works",
            "How does something work?",
            "one node per mechanism or flow",
        ),
        (
            "people",
            "People",
            "Who is responsible for what?",
            "one node per person or team, with their areas",
        ),
        (
            "done",
            "Done",
            "What was done?",
            "one node per release or finished stream of work, not per commit or PR",
        ),
        (
            "blocked",
            "Blocked",
            "What is open or blocked, and on what?",
            "one node per blocker or open question",
        ),
        (
            "decisions",
            "Decisions",
            "Why was a decision made?",
            "one node per decision, with its reason, date and source",
        ),
    ]
    .into_iter()
    .map(|(id, title, question, granularity)| BranchDef {
        id: id.into(),
        title: title.into(),
        question: question.into(),
        granularity: granularity.into(),
    })
    .collect()
}

/// The id of the computed branch answered by `rrecall recent`; never stored.
pub const COMPUTED_BRANCH: &str = "recent";

/// The store config `rrecall.toml` (spec §9). Unknown keys are errors at every level.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    #[serde(default)]
    pub(crate) skeleton: SkeletonConfig,
    /// The plugin instances, in run order (spec §6.2).
    #[serde(default)]
    pub plugins: Vec<PluginEntry>,
    /// The project's repository, whose `.rrecall/plugins/` loads once trusted (spec §6.7).
    #[serde(default)]
    pub repo: Option<PathBuf>,
    #[serde(default)]
    pub max_rounds: Option<u32>,
    #[serde(default)]
    pub max_cost_usd: Option<f64>,
}

/// One `[[plugins]]` entry: a plugin instance (spec §6.2).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginEntry {
    pub id: String,
    /// The plugin's id; the entry's `id` when absent.
    #[serde(default, rename = "use")]
    pub uses: Option<String>,
    #[serde(default)]
    pub params: toml::Table,
    #[serde(default)]
    pub runner: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default, deserialize_with = "duration_opt")]
    pub timeout: Option<Duration>,
    #[serde(default)]
    pub allowed_runners: Option<Vec<String>>,
}

/// `<N>s`, `<N>m`, `<N>h` or `<N>d`, N a positive integer.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let bad = || format!("`{s}` is not a duration (<N>s, <N>m, <N>h or <N>d)");
    let unit = match s.chars().last() {
        Some('s') => 1,
        Some('m') => 60,
        Some('h') => 3600,
        Some('d') => 86400,
        _ => return Err(bad()),
    };
    let digits = &s[..s.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    match digits.parse::<u64>().ok().and_then(|n| n.checked_mul(unit)) {
        Some(secs) if secs > 0 => Ok(Duration::from_secs(secs)),
        _ => Err(bad()),
    }
}

/// Serde helper for an optional duration field.
pub fn duration_opt<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
    let s = String::deserialize(d)?;
    parse_duration(&s)
        .map(Some)
        .map_err(serde::de::Error::custom)
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct SkeletonConfig {
    #[serde(default)]
    branch: Vec<BranchDef>,
}

/// The default branches followed by the config's `[[skeleton.branch]]` entries.
pub fn load_branches(store: &Path) -> Result<Vec<BranchDef>> {
    load_store_config(store)?.branches()
}

pub fn load_store_config(store: &Path) -> Result<StoreConfig> {
    let path = store.join(STORE_CONFIG);
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("{STORE_CONFIG} does not parse"))
}

impl StoreConfig {
    /// The default branches followed by the config's `[[skeleton.branch]]` entries.
    pub fn branches(&self) -> Result<Vec<BranchDef>> {
        let mut branches = default_branches();
        for b in self.skeleton.branch.iter().cloned() {
            let problem = if !crate::node::is_slug(&b.id) {
                Some("is not a slug (a-z, 0-9, single dashes)")
            } else if b.id == COMPUTED_BRANCH {
                Some("is the computed branch answered by `rrecall recent`")
            } else if branches.iter().any(|x| x.id == b.id) {
                Some("is defined twice")
            } else if b.title.trim().is_empty() || b.title.chars().count() > crate::node::TITLE_MAX
            {
                Some("needs a title of 1 to 80 characters")
            } else if b.question.chars().count() > crate::node::BODY_MAX {
                Some("has a question longer than 600 characters")
            } else {
                None
            };
            if let Some(p) = problem {
                anyhow::bail!("{STORE_CONFIG}: branch `{}` {p}", b.id);
            }
            branches.push(b);
        }
        Ok(branches)
    }
}

/// The `[[skeleton.branch]]` entry with this id in a config text, if the text parses and has one.
pub fn branch_entry(text: &str, id: &str) -> Option<BranchDef> {
    let cfg: StoreConfig = toml::from_str(text).ok()?;
    cfg.skeleton.branch.into_iter().find(|b| b.id == id)
}

/// TOML text of one `[[skeleton.branch]]` entry, to append to `rrecall.toml`.
pub fn branch_toml(b: &BranchDef) -> String {
    format!(
        "\n[[skeleton.branch]]\nid = {}\ntitle = {}\nquestion = {}\ngranularity = {}\n",
        toml_str(&b.id),
        toml_str(&b.title),
        toml_str(&b.question),
        toml_str(&b.granularity)
    )
}

/// The user config `$XDG_CONFIG_HOME/rrecall/config.toml` (spec §9), the parts M1 reads.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    // limit: installed plugins (`plugin add`, spec §6.7) add their record in M4; until then that key is unknown.
    #[serde(default)]
    pub projects: Vec<ProjectEntry>,
    /// Default runner and model for declarative plugins (spec §9).
    #[serde(default)]
    pub runner: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Trusted project plugin directories (spec §6.7).
    #[serde(default)]
    pub trusted: Vec<Trusted>,
}

/// A project's `.rrecall/plugins/` directory the user trusts at this content hash.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trusted {
    pub path: PathBuf,
    pub hash: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectEntry {
    pub name: String,
    #[serde(default)]
    pub store: Option<PathBuf>,
    #[serde(default)]
    pub remotes: Vec<String>,
}

fn xdg_dir(var: &str, fallback: &str) -> Result<PathBuf> {
    match std::env::var_os(var).map(PathBuf::from) {
        Some(p) if p.is_absolute() => Ok(p),
        _ => {
            let home = std::env::var_os("HOME").context("HOME is not set")?;
            Ok(PathBuf::from(home).join(fallback))
        }
    }
}

pub fn user_config_path() -> Result<PathBuf> {
    Ok(xdg_dir("XDG_CONFIG_HOME", ".config")?.join("rrecall/config.toml"))
}

pub fn load_user_config() -> Result<UserConfig> {
    let path = user_config_path()?;
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            toml::from_str(&text).with_context(|| format!("{} does not parse", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(UserConfig::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

impl UserConfig {
    pub fn project(&self, name: &str) -> Option<&ProjectEntry> {
        self.projects.iter().find(|p| p.name == name)
    }

    /// The store directory: the user config's `store`, else `$XDG_DATA_HOME/rrecall/<name>`.
    pub fn store_path(&self, name: &str) -> Result<PathBuf> {
        match self.project(name).and_then(|p| p.store.clone()) {
            Some(p) if p.is_absolute() => Ok(p),
            Some(p) => anyhow::bail!(
                "user config: store `{}` of project `{name}` is not an absolute path",
                p.display()
            ),
            None => Ok(xdg_dir("XDG_DATA_HOME", ".local/share")?
                .join("rrecall")
                .join(name)),
        }
    }
}

/// A project name doubles as a directory name, so it may not climb or nest.
pub fn check_project_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.starts_with('.')
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
    {
        return Err(crate::error::usage(format!(
            "project name `{name}` must be a plain directory name (no `/`, not starting with `.`)"
        )));
    }
    Ok(())
}

/// The `origin` remote URL of the git repository at `dir`, if any.
pub fn origin_url(dir: &Path) -> Option<String> {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    let url = String::from_utf8(o.stdout).ok()?.trim().to_string();
    (o.status.success() && !url.is_empty()).then_some(url)
}

/// Project name and store directory from `--project`, `RRECALL_PROJECT`, or the current repo's origin (spec §2).
pub fn resolve_project(flag: Option<&str>) -> Result<(String, PathBuf)> {
    let user = load_user_config()?;
    let env = std::env::var("RRECALL_PROJECT")
        .ok()
        .filter(|v| !v.is_empty());
    let name = match flag.map(str::to_string).or(env) {
        Some(name) => name,
        None => {
            let cwd = std::env::current_dir()?;
            let Some(url) = origin_url(&cwd) else {
                return Err(crate::error::usage(
                    "no project: pass --project <name>, set RRECALL_PROJECT, or run inside a repo whose origin remote is in the user config",
                ));
            };
            // limit: remote URLs match as exact strings; an ssh and an https URL of one repo are two remotes.
            match user.projects.iter().find(|p| p.remotes.contains(&url)) {
                Some(p) => p.name.clone(),
                None => {
                    return Err(crate::error::no_map(format!(
                        "no map for origin {url}: run `rrecall init` in this repo, or add the URL to a project's remotes in {}",
                        user_config_path()?.display()
                    )));
                }
            }
        }
    };
    check_project_name(&name)?;
    let path = user.store_path(&name)?;
    if !path.join(STORE_CONFIG).is_file() {
        return Err(crate::error::no_map(format!(
            "no map for project `{name}` at {}: run `rrecall init {name}`",
            path.display()
        )));
    }
    Ok((name, path))
}

/// A TOML basic string with its quotes and escapes.
pub fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn defaults_then_config_branches() {
        let t = TempDir::new();
        let extra = BranchDef {
            id: "services".into(),
            title: "Services \"core\"".into(),
            question: "Which services run?".into(),
            granularity: "one node per service".into(),
        };
        let text = format!(
            "# comment kept\n[[plugins]]\nid = \"repo\"\n\n{}",
            branch_toml(&extra)
        );
        std::fs::write(t.0.join(STORE_CONFIG), text).unwrap();
        let b = load_branches(&t.0).unwrap();
        let ids: Vec<_> = b.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "layout",
                "how-it-works",
                "people",
                "done",
                "blocked",
                "decisions",
                "services"
            ]
        );
        assert_eq!(b[6], extra);
        assert_eq!(b[0].title, "Layout");
        assert_eq!(b[0].question, "Where does something live?");
    }

    #[test]
    fn store_config_is_strict() {
        let t = TempDir::new();
        let good = "repo = \"/src/kiln\"\nmax_rounds = 3\nmax_cost_usd = 1.5\n\n[[plugins]]\nid = \"notes\"\nuse = \"files\"\nparams = { path = \"/n\", git = false }\nrunner = \"codex\"\nmodel = \"m\"\ntimeout = \"2m\"\nallowed_runners = [\"codex\"]\n\n[[plugins]]\nid = \"github\"\n";
        std::fs::write(t.0.join(STORE_CONFIG), good).unwrap();
        let cfg = load_store_config(&t.0).unwrap();
        assert_eq!(cfg.repo.as_deref(), Some(Path::new("/src/kiln")));
        assert_eq!(cfg.max_rounds, Some(3));
        assert_eq!(cfg.plugins.len(), 2);
        let notes = &cfg.plugins[0];
        assert_eq!(notes.uses.as_deref(), Some("files"));
        assert_eq!(notes.params["git"], toml::Value::Boolean(false));
        assert_eq!(notes.timeout, Some(std::time::Duration::from_secs(120)));
        assert_eq!(
            notes.allowed_runners.as_deref(),
            Some(&["codex".to_string()][..])
        );
        assert_eq!(cfg.plugins[1].uses, None);
        for bad in [
            "colour = \"red\"\n",
            "[[plugins]]\nid = \"x\"\nenabled = true\n",
            "[[plugins]]\nid = \"x\"\ntimeout = \"soon\"\n",
        ] {
            std::fs::write(t.0.join(STORE_CONFIG), bad).unwrap();
            let e = format!("{:#}", load_store_config(&t.0).unwrap_err());
            assert!(e.contains(STORE_CONFIG), "{bad}: {e}");
            assert!(load_branches(&t.0).is_err(), "{bad}");
        }
    }

    #[test]
    fn user_config_is_strict() {
        let u: UserConfig = toml::from_str(
            "runner = \"claude\"\nmodel = \"default\"\n[[projects]]\nname = \"kiln\"\n[[trusted]]\npath = \"/src/kiln/.rrecall/plugins\"\nhash = \"sha256:00\"\n",
        )
        .unwrap();
        assert_eq!(u.trusted[0].hash, "sha256:00");
        assert_eq!(u.runner.as_deref(), Some("claude"));
        assert!(toml::from_str::<UserConfig>("theme = \"dark\"\n").is_err());
        assert!(toml::from_str::<UserConfig>("[[trusted]]\npath = \"/x\"\n").is_err());
    }

    #[test]
    fn durations() {
        for (s, secs) in [("30s", 30), ("10m", 600), ("2h", 7200), ("1d", 86400)] {
            assert_eq!(
                parse_duration(s),
                Ok(std::time::Duration::from_secs(secs)),
                "{s}"
            );
        }
        for bad in [
            "",
            "10",
            "m",
            "1.5h",
            "-1s",
            "10 m",
            "1w",
            "0s",
            "99999999999999999999d",
        ] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn bad_branch_entries_are_errors() {
        for bad in [
            "[[skeleton.branch]]\nid = \"layout\"\ntitle = \"Again\"\nquestion = \"\"\n",
            "[[skeleton.branch]]\nid = \"Not A Slug\"\ntitle = \"x\"\nquestion = \"\"\n",
            "[[skeleton.branch]]\nid = \"x\"\ntitle = \"x\"\nquestion = \"\"\ncolour = \"red\"\n",
            "[[skeleton.branch]]\nid = \"recent\"\ntitle = \"x\"\nquestion = \"\"\n",
        ] {
            let t = TempDir::new();
            std::fs::write(t.0.join(STORE_CONFIG), bad).unwrap();
            let e = load_branches(&t.0).unwrap_err();
            assert!(format!("{e:#}").contains(STORE_CONFIG), "{bad}: {e:#}");
        }
    }
}

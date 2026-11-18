//! The store config `rrecall.toml` and the user config (spec §9), and project resolution (§2).

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const STORE_CONFIG: &str = "rrecall.toml";

/// A branch of the skeleton (spec §3.4).
#[derive(Debug, Clone, PartialEq, Deserialize)]
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
        ("layout", "Layout", "Where does something live?", "one node per component, directory or service"),
        ("how-it-works", "How it works", "How does something work?", "one node per mechanism or flow"),
        ("people", "People", "Who is responsible for what?", "one node per person or team, with their areas"),
        (
            "done",
            "Done",
            "What was done?",
            "one node per release or finished stream of work, not per commit or PR",
        ),
        ("blocked", "Blocked", "What is open or blocked, and on what?", "one node per blocker or open question"),
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
const COMPUTED_BRANCH: &str = "recent";

#[derive(Deserialize, Default)]
struct StoreConfig {
    // limit: other top-level keys (plugins, max_rounds, ...) are read by M2; M1 ignores them.
    #[serde(default)]
    skeleton: SkeletonConfig,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SkeletonConfig {
    #[serde(default)]
    branch: Vec<BranchDef>,
}

/// The default branches followed by the config's `[[skeleton.branch]]` entries.
pub fn load_branches(store: &Path) -> Result<Vec<BranchDef>> {
    let path = store.join(STORE_CONFIG);
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let cfg: StoreConfig = toml::from_str(&text).with_context(|| format!("{STORE_CONFIG} does not parse"))?;
    let mut branches = default_branches();
    for b in cfg.skeleton.branch {
        let problem = if !crate::node::is_slug(&b.id) {
            Some("is not a slug (a-z, 0-9, single dashes)")
        } else if b.id == COMPUTED_BRANCH {
            Some("is the computed branch answered by `rrecall recent`")
        } else if branches.iter().any(|x| x.id == b.id) {
            Some("is defined twice")
        } else if b.title.trim().is_empty() || b.title.chars().count() > crate::node::TITLE_MAX {
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
        let text = format!("# comment kept\n[[plugins]]\nid = \"repo\"\n\n{}", branch_toml(&extra));
        std::fs::write(t.0.join(STORE_CONFIG), text).unwrap();
        let b = load_branches(&t.0).unwrap();
        let ids: Vec<_> = b.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, ["layout", "how-it-works", "people", "done", "blocked", "decisions", "services"]);
        assert_eq!(b[6], extra);
        assert_eq!(b[0].title, "Layout");
        assert_eq!(b[0].question, "Where does something live?");
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

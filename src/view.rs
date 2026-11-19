//! The built-in `html` view (spec §8): one self-contained `views/index.html` with the map, each
//! node's history and the "changed since the last run" set embedded as data. It runs as
//! `rrecall builtin html` in a run's view step (§4.1 step 10) and in-process for `rrecall open`.

use crate::history;
use crate::query;
use crate::store::Store;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const MANIFEST: &str = r#"id = "html"
name = "One self-contained HTML page of the map"
version = "0.1.0"
min_rrecall = "0.1.0"
kind = "view"

[view]
command = ["rrecall", "builtin", "html"]
"#;

/// The page: inline CSS and JS, and a `{{TITLE}}` and a `{{DATA}}` marker, in that order.
const PAGE: &str = include_str!("view.html");

/// `rrecall builtin html`: renders the store the core names in `RRECALL_STORE`.
pub fn run(input: &str) -> Result<String> {
    let input: Value = serde_json::from_str(input).context("reading the view input")?;
    let project = input["project"]
        .as_str()
        .context("the view input has no `project`")?;
    let root = std::env::var_os("RRECALL_STORE").context("RRECALL_STORE is not set")?;
    write(Path::new(&root), project)?;
    Ok("{}\n".into())
}

/// Renders the store at `root` to `<root>/views/index.html` and returns that path.
pub fn write(root: &Path, project: &str) -> Result<PathBuf> {
    let page = render(&Store::load(root)?, project)?;
    let dir = root.join("views");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("index.html");
    std::fs::write(&path, page).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// The page for a store. Nothing in it depends on the time of rendering: the same map and
/// history give the same bytes.
pub fn render(store: &Store, project: &str) -> Result<String> {
    let mut hist = history::map_history(store)?;
    let kids = store.child_map();
    // Branches in skeleton order, each followed by its subtree depth first.
    let mut nodes = Vec::with_capacity(store.nodes.len());
    for root in store.roots() {
        for id in std::iter::once(root.id.clone()).chain(store.descendants(&root.id)) {
            let mut v = query::json_node(store, &kids, &store.nodes[&id]);
            v["changed"] = json!(hist.changed.contains(&id));
            v["history"] = json!(hist.nodes.remove(&id).unwrap_or_default());
            nodes.push(v);
        }
    }
    let data = json!({ "project": project, "last_run": hist.last_run, "nodes": nodes });
    let (head, rest) = PAGE
        .split_once("{{TITLE}}")
        .context("the page has no title marker")?;
    let (mid, tail) = rest
        .split_once("{{DATA}}")
        .context("the page has no data marker")?;
    Ok(format!(
        "{head}{}{mid}{}{tail}",
        escape_html(project),
        script_safe(&data.to_string())
    ))
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// JSON that cannot end its `<script>` block or open a comment: `<`, `>` and `&` as `\u` escapes,
/// which only occur inside JSON strings, where they decode to the same text.
fn script_safe(json: &str) -> String {
    json.replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_cannot_leave_its_script_block() {
        let s = script_safe(&json!({"note": "</script><!-- & x"}).to_string());
        assert!(
            !s.contains('<') && !s.contains('>') && !s.contains('&'),
            "{s}"
        );
        let back: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(back["note"], "</script><!-- & x");
        assert_eq!(escape_html("a<b>&\"c"), "a&lt;b&gt;&amp;&quot;c");
    }

    #[test]
    fn the_page_has_its_markers_once_and_loads_nothing() {
        assert_eq!(PAGE.matches("{{TITLE}}").count(), 1);
        assert_eq!(PAGE.matches("{{DATA}}").count(), 1);
        assert!(PAGE.find("{{TITLE}}") < PAGE.find("{{DATA}}"));
        for bad in [
            "http://", "https://", "<link", " src=", "@import", "url(", "fetch(",
        ] {
            assert!(!PAGE.contains(bad), "{bad}");
        }
    }
}

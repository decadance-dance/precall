//! The built-in `html` view (spec §8): one self-contained `views/index.html` with the map, each
//! node's history and the "changed since the last run" set embedded as data. It runs as
//! `precall builtin html` in a run's view step (§4.1 step 10) and in-process for `precall open`.

use crate::history;
use crate::query;
use crate::store::Store;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const MANIFEST: &str = r#"id = "html"
name = "One self-contained HTML page of the map"
version = "0.1.0"
min_precall = "0.1.0"
kind = "view"

[view]
command = ["precall", "builtin", "html"]
"#;

/// The page: inline CSS and JS, and a `{{TITLE}}` and a `{{DATA}}` marker, in that order.
const PAGE: &str = include_str!("view.html");

/// `precall builtin html`: renders the store the core names in `PRECALL_STORE`.
pub fn run(input: &str) -> Result<String> {
    let input: Value = serde_json::from_str(input).context("reading the view input")?;
    let project = input["project"]
        .as_str()
        .context("the view input has no `project`")?;
    let root = std::env::var_os("PRECALL_STORE").context("PRECALL_STORE is not set")?;
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
    fill(PAGE, project, &data)
}

/// The template with its markers filled, its line ends LF whatever the checkout it was built
/// from used, so the page's bytes depend only on the map and its history.
fn fill(template: &str, project: &str, data: &Value) -> Result<String> {
    let template = crate::plugin::lf(template);
    let (head, rest) = template
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
    fn the_page_has_its_markers_once_and_its_bytes_do_not_depend_on_line_ends() {
        assert_eq!(PAGE.matches("{{TITLE}}").count(), 1);
        assert_eq!(PAGE.matches("{{DATA}}").count(), 1);
        assert!(PAGE.find("{{TITLE}}") < PAGE.find("{{DATA}}"));
        let data = json!({"nodes": []});
        let lf_page = PAGE.replace("\r\n", "\n");
        let lf = fill(&lf_page, "kiln", &data).unwrap();
        let crlf = fill(&lf_page.replace('\n', "\r\n"), "kiln", &data).unwrap();
        assert_eq!(lf, crlf);
        assert!(!lf.contains('\r'));
    }

    #[test]
    fn the_history_shows_in_the_order_of_the_data() {
        // The inspector lists `n.history` as given, oldest first (spec §8), never reversed.
        assert!(PAGE.contains("\"History, oldest first\""));
        assert!(!PAGE.contains("reverse()"));
    }
}

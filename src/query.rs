//! Query output for agents (spec §7): compact Markdown, or JSON with `--json`.

use crate::node::Node;
use crate::store::Store;
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The token estimate used for `--max-tokens` and the `ls` size budget: one token per 4 bytes, rounded up.
pub fn estimate_tokens(s: &str) -> usize {
    s.len().div_ceil(4)
}

type Kids<'a> = BTreeMap<&'a str, Vec<&'a Node>>;

/// Titles from the branch down to the node.
pub fn path(store: &Store, id: &str) -> String {
    let mut titles = Vec::new();
    let mut cur = store.nodes.get(id);
    while let Some(n) = cur {
        titles.push(n.title.as_str());
        cur = n.parent.as_deref().and_then(|p| store.nodes.get(p));
    }
    titles.reverse();
    titles.join(" > ")
}

fn descendant_count(kids: &Kids, id: &str) -> usize {
    kids.get(id).into_iter().flatten().map(|k| 1 + descendant_count(kids, &k.id)).sum()
}

/// `- Title (id)`, with `+N` when N descendants are not shown.
fn title_line(kids: &Kids, n: &Node, hidden: bool) -> String {
    let more = if hidden { descendant_count(kids, &n.id) } else { 0 };
    if more > 0 { format!("- {} ({}) +{more}", n.title, n.id) } else { format!("- {} ({})", n.title, n.id) }
}

/// The tree of titles and ids, `depth` levels deep (1 = branches only).
pub fn ls(store: &Store, depth: usize, json: bool) -> String {
    let kids = store.child_map();
    let mut lines = Vec::new();
    let mut nodes = Vec::new();
    fn walk<'a>(kids: &Kids<'a>, n: &'a Node, level: usize, depth: usize, lines: &mut Vec<String>, nodes: &mut Vec<Value>) {
        lines.push(format!("{}{}", "  ".repeat(level - 1), title_line(kids, n, level == depth)));
        nodes.push(match &n.parent {
            Some(p) => json!({ "id": n.id, "parent": p, "title": n.title }),
            None => json!({ "id": n.id, "title": n.title }),
        });
        if level < depth {
            for k in kids.get(n.id.as_str()).into_iter().flatten() {
                walk(kids, k, level + 1, depth, lines, nodes);
            }
        }
    }
    for b in store.roots() {
        walk(&kids, b, 1, depth.max(1), &mut lines, &mut nodes);
    }
    if json { format!("{}\n", json!({ "nodes": nodes })) } else { lines.join("\n") + "\n" }
}

fn md_block(store: &Store, kids: &Kids, n: &Node, with_children: bool) -> String {
    let mut s = format!("## {} ({})\n", path(store, &n.id), n.id);
    if !n.body.is_empty() {
        s += &format!("{}\n", n.body);
    }
    s += &format!("sources: {}\n", n.sources.join(", "));
    let meta: Vec<String> = [("date", &n.date), ("state", &n.state)]
        .into_iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}: {v}")))
        .collect();
    if !meta.is_empty() {
        s += &format!("{}\n", meta.join(" · "));
    }
    if let Some(note) = &n.note {
        s += &format!("note: {note}\n");
    }
    if with_children {
        for k in kids.get(n.id.as_str()).into_iter().flatten() {
            s += &title_line(kids, k, true);
            s.push('\n');
        }
    }
    s
}

/// The stored node's fields plus `path` and the ids of its `children`.
pub fn json_node(store: &Store, kids: &Kids, n: &Node) -> Value {
    let mut v = serde_json::to_value(n).unwrap_or(Value::Null);
    if let Value::Object(m) = &mut v {
        m.insert("path".into(), Value::String(path(store, &n.id)));
        let ids: Vec<&str> = kids.get(n.id.as_str()).into_iter().flatten().map(|k| k.id.as_str()).collect();
        m.insert("children".into(), json!(ids));
    }
    v
}

/// One node and its subtree to `depth` (0 = the node, its children as titles).
pub fn show(store: &Store, id: &str, depth: usize, max_tokens: Option<usize>, json: bool) -> Result<String> {
    let root = store.get(id)?;
    let kids = store.child_map();
    let mut order: Vec<(&Node, usize)> = Vec::new();
    fn walk<'a>(kids: &Kids<'a>, n: &'a Node, rel: usize, depth: usize, order: &mut Vec<(&'a Node, usize)>) {
        order.push((n, rel));
        if rel < depth {
            for k in kids.get(n.id.as_str()).into_iter().flatten() {
                walk(kids, k, rel + 1, depth, order);
            }
        }
    }
    walk(&kids, root, 0, depth, &mut order);
    let pieces: Vec<String> = order
        .iter()
        .map(|(n, rel)| {
            if json { json_node(store, &kids, n).to_string() } else { md_block(store, &kids, n, *rel == depth) }
        })
        .collect();
    let footer = |keep: usize| -> String {
        let left = &order[keep..];
        if left.is_empty() {
            return String::new();
        }
        let left_ids: BTreeSet<&str> = left.iter().map(|(n, _)| n.id.as_str()).collect();
        let tops: Vec<String> = left
            .iter()
            .filter(|(n, _)| !n.parent.as_deref().is_some_and(|p| left_ids.contains(p)))
            .map(|(n, rel)| match depth - rel {
                0 => format!("rrecall show {}", n.id),
                d => format!("rrecall show {} --depth {d}", n.id),
            })
            .collect();
        if json {
            let ids: Vec<&str> = left.iter().map(|(n, _)| n.id.as_str()).collect();
            json!(ids).to_string()
        } else {
            format!("[{} nodes left out; see them with: {}]\n", left.len(), tops.join(", "))
        }
    };
    let keep = match max_tokens {
        Some(max) => fit(&pieces, max, &footer),
        None => pieces.len(),
    };
    let left = footer(keep);
    Ok(if json {
        let mut out = format!("{{\"nodes\":[{}]", pieces[..keep].join(","));
        if !left.is_empty() {
            out += &format!(",\"left_out\":{left}");
        }
        out + "}\n"
    } else {
        pieces[..keep].join("\n") + if left.is_empty() { "" } else { "\n" } + &left
    })
}

/// How many leading pieces fit in `max` tokens with the footer; never fewer than one.
// limit: a first node larger than `max` is still printed, so a cut output can exceed `max`.
fn fit(pieces: &[String], max: usize, footer: &dyn Fn(usize) -> String) -> usize {
    let mut bytes: Vec<usize> = vec![0];
    for p in pieces {
        bytes.push(bytes.last().copied().unwrap_or(0) + p.len() + 1);
    }
    (1..=pieces.len())
        .rev()
        .find(|&k| (bytes[k] + footer(k).len()).div_ceil(4) <= max)
        .unwrap_or(1)
}

//! The map store (spec §3.2): `map/<branch-id>.jsonl` files in a local git repository.

use crate::config::{self, BranchDef};
use crate::error;
use crate::node::Node;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const BRANCH_SOURCE: &str = "config:rrecall.toml";

pub struct Store {
    pub root: PathBuf,
    pub branches: Vec<BranchDef>,
    /// Every node by id, branches included (as parent-less nodes built from the config).
    pub nodes: BTreeMap<String, Node>,
}

impl Store {
    pub fn load(root: &Path) -> Result<Store> {
        let branches = config::load_branches(root)?;
        let mut nodes = BTreeMap::new();
        for (i, b) in branches.iter().enumerate() {
            nodes.insert(b.id.clone(), branch_node(b, i));
        }
        // Which file each stored node came from, to check it sits under that file's branch.
        let mut file_of: BTreeMap<String, String> = BTreeMap::new();
        let map_dir = root.join("map");
        let mut entries = match std::fs::read_dir(&map_dir) {
            Ok(rd) => rd.collect::<std::io::Result<Vec<_>>>()?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).context("reading map/"),
        };
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = format!("map/{name}");
            if !name
                .strip_suffix(".jsonl")
                .is_some_and(|b| branches.iter().any(|x| x.id == b))
            {
                bail!(
                    "{rel}: not `<branch-id>.jsonl` for a branch of {}",
                    config::STORE_CONFIG
                );
            }
            let text =
                std::fs::read_to_string(entry.path()).with_context(|| format!("reading {rel}"))?;
            for (i, line) in text.lines().enumerate() {
                let at = || format!("{rel} line {}", i + 1);
                let node: Node = serde_json::from_str(line).with_context(at)?;
                node.validate()
                    .map_err(anyhow::Error::msg)
                    .with_context(at)?;
                if nodes.contains_key(&node.id) {
                    bail!("{}: id `{}` is used twice", at(), node.id);
                }
                file_of.insert(node.id.clone(), rel.clone());
                nodes.insert(node.id.clone(), node);
            }
        }
        let store = Store {
            root: root.to_path_buf(),
            branches,
            nodes,
        };
        for (id, rel) in &file_of {
            let mut cur = id.as_str();
            for _ in 0..=store.nodes.len() {
                match store.nodes.get(cur).and_then(|n| n.parent.as_deref()) {
                    Some(p) if store.nodes.contains_key(p) => cur = p,
                    Some(p) => {
                        bail!("{rel}: node `{id}` has parent `{p}`, which is not in the map")
                    }
                    None => break,
                }
            }
            if store.nodes[cur].parent.is_some() {
                bail!("{rel}: node `{id}` is in a parent cycle");
            }
            if format!("map/{cur}.jsonl") != *rel {
                bail!("{rel}: node `{id}` belongs to branch `{cur}`, so to map/{cur}.jsonl");
            }
        }
        Ok(store)
    }

    /// Adds a branch to the skeleton in memory; the caller writes it to the config.
    pub fn add_branch(&mut self, b: BranchDef) {
        self.nodes
            .insert(b.id.clone(), branch_node(&b, self.branches.len()));
        self.branches.push(b);
    }

    /// Whether an id is taken, counting the computed `recent` branch.
    pub fn id_taken(&self, id: &str) -> bool {
        id == config::COMPUTED_BRANCH || self.nodes.contains_key(id)
    }

    pub fn is_branch(&self, id: &str) -> bool {
        self.nodes.get(id).is_some_and(|n| n.parent.is_none())
    }

    /// The node, or exit 1 when the map has no such id.
    pub fn get(&self, id: &str) -> Result<&Node> {
        self.nodes
            .get(id)
            .ok_or_else(|| error::not_found(format!("no node `{id}` in the map")))
    }

    /// The branch a node sits under (the node itself for a branch).
    pub fn branch_of(&self, id: &str) -> &str {
        let mut cur = self.nodes.get(id).map_or("", |n| n.id.as_str());
        while let Some(p) = self.nodes.get(cur).and_then(|n| n.parent.as_deref()) {
            cur = p;
        }
        cur
    }

    /// Children of every node, siblings in (`order`, `id`) order.
    pub fn child_map(&self) -> BTreeMap<&str, Vec<&Node>> {
        let mut m: BTreeMap<&str, Vec<&Node>> = BTreeMap::new();
        for n in self.nodes.values() {
            if let Some(p) = &n.parent {
                m.entry(p.as_str()).or_default().push(n);
            }
        }
        for v in m.values_mut() {
            v.sort_by(|a, b| (a.order, &a.id).cmp(&(b.order, &b.id)));
        }
        m
    }

    /// Branches in skeleton order.
    pub fn roots(&self) -> Vec<&Node> {
        self.branches.iter().map(|b| &self.nodes[&b.id]).collect()
    }

    /// Ids of a node's descendants, depth first.
    pub fn descendants(&self, id: &str) -> Vec<String> {
        let kids = self.child_map();
        let mut out = Vec::new();
        let mut stack: Vec<&str> = vec![id];
        while let Some(cur) = stack.pop() {
            for k in kids.get(cur).into_iter().flatten().rev() {
                out.push(k.id.clone());
                stack.push(&k.id);
            }
        }
        out
    }

    /// Writes one file per non-empty branch, lines sorted by id; removes files of empty branches.
    pub fn save(&self) -> Result<()> {
        let mut files: BTreeMap<&str, String> = BTreeMap::new();
        for n in self.nodes.values().filter(|n| n.parent.is_some()) {
            let line = serde_json::to_string(n)?;
            let text = files.entry(self.branch_of(&n.id)).or_default();
            text.push_str(&line);
            text.push('\n');
        }
        let dir = self.root.join("map");
        std::fs::create_dir_all(&dir).context("creating map/")?;
        for b in &self.branches {
            let path = dir.join(format!("{}.jsonl", b.id));
            match files.get(b.id.as_str()) {
                Some(text) => std::fs::write(&path, text)
                    .with_context(|| format!("writing {}", path.display()))?,
                None if path.exists() => std::fs::remove_file(&path)?,
                None => {}
            }
        }
        Ok(())
    }
}

impl Store {
    /// Stages everything in the store but the ignored run files and commits it; false when nothing changed.
    pub fn commit(&self, message: &str) -> Result<bool> {
        git(&self.root, &["add", "-A", "."])?;
        if git_status(&self.root, &["diff", "--cached", "--quiet"])? {
            return Ok(false);
        }
        git(&self.root, &["commit", "-q", "-m", message])?;
        Ok(true)
    }
}

/// Runs git in `dir` as rrecall (store commits need no configured identity and are never signed).
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let o = git_cmd(dir, args).output().context("running git")?;
    if !o.status.success() {
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    String::from_utf8(o.stdout).context("git printed non-UTF-8 output")
}

/// True when git exits 0, false when it exits 1; an error otherwise.
fn git_status(dir: &Path, args: &[&str]) -> Result<bool> {
    let o = git_cmd(dir, args).output().context("running git")?;
    match o.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        ),
    }
}

fn git_cmd(dir: &Path, args: &[&str]) -> std::process::Command {
    let mut c = std::process::Command::new("git");
    c.args([
        "-c",
        "user.name=rrecall",
        "-c",
        "user.email=rrecall@localhost",
        "-c",
        "commit.gpgsign=false",
    ])
    .args(["-c", "init.defaultBranch=main", "-C"])
    .arg(dir)
    .args(args);
    c
}

fn branch_node(b: &BranchDef, index: usize) -> Node {
    Node {
        id: b.id.clone(),
        parent: None,
        title: b.title.clone(),
        body: b.question.clone(),
        sources: vec![BRANCH_SOURCE.to_string()],
        date: None,
        state: None,
        order: index as u32,
        note: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const LAYOUT: &str = concat!(
        r#"{"id":"api","parent":"layout","title":"API","body":"HTTP API.","sources":["import:o.txt"],"order":0}"#,
        "\n",
        r#"{"id":"api-auth","parent":"api","title":"Auth","body":"Tokens.","sources":["import:o.txt"],"date":"2026-01-02","state":"open","order":0,"note":"ask Ana"}"#,
        "\n",
        r#"{"id":"web","parent":"layout","title":"Web ✓","body":"","sources":["import:o.txt"],"order":1}"#,
        "\n",
    );

    fn store_with(files: &[(&str, &str)]) -> TempDir {
        let t = TempDir::new();
        std::fs::write(t.0.join(config::STORE_CONFIG), "").unwrap();
        std::fs::create_dir_all(t.0.join("map")).unwrap();
        for (name, text) in files {
            std::fs::write(t.0.join("map").join(name), text).unwrap();
        }
        t
    }

    #[test]
    fn load_and_rewrite_keeps_bytes() {
        let t = store_with(&[("layout.jsonl", LAYOUT)]);
        let s = Store::load(&t.0).unwrap();
        assert_eq!(s.nodes.len(), 6 + 3);
        assert_eq!(s.nodes["layout"].sources, [BRANCH_SOURCE]);
        s.save().unwrap();
        assert_eq!(
            std::fs::read_to_string(t.0.join("map/layout.jsonl")).unwrap(),
            LAYOUT
        );
        let files: Vec<_> = std::fs::read_dir(t.0.join("map")).unwrap().collect();
        assert_eq!(files.len(), 1, "empty branches get no file");
    }

    #[test]
    fn save_sorts_lines_and_moves_nodes_to_their_branch_file() {
        let mut lines: Vec<&str> = LAYOUT.lines().collect();
        lines.reverse();
        let t = store_with(&[("layout.jsonl", &lines.join("\n"))]);
        let mut s = Store::load(&t.0).unwrap();
        s.nodes.get_mut("web").unwrap().parent = Some("done".into());
        s.save().unwrap();
        let layout = std::fs::read_to_string(t.0.join("map/layout.jsonl")).unwrap();
        assert_eq!(layout.lines().count(), 2);
        assert!(layout.starts_with(r#"{"id":"api","#));
        assert!(
            std::fs::read_to_string(t.0.join("map/done.jsonl"))
                .unwrap()
                .starts_with(r#"{"id":"web","#)
        );
    }

    #[test]
    fn corrupt_map_line_names_file_and_line() {
        let cases = [
            (
                "layout.jsonl",
                "{\"id\":\"x\"}\n",
                "map/layout.jsonl line 1",
            ),
            (
                "layout.jsonl",
                &format!(
                    "{LAYOUT}{{\"id\":\"api\",\"parent\":\"layout\",\"title\":\"Dup\",\"body\":\"\",\"sources\":[\"import:o.txt\"],\"order\":2}}\n"
                ),
                "line 4",
            ),
            ("nope.jsonl", LAYOUT, "map/nope.jsonl"),
            ("done.jsonl", LAYOUT, "map/done.jsonl"),
            (
                "layout.jsonl",
                r#"{"id":"a","parent":"ghost","title":"A","body":"","sources":["import:o.txt"],"order":0}"#,
                "ghost",
            ),
            (
                "layout.jsonl",
                r#"{"id":"a","parent":"layout","title":"A","body":"","sources":["import:o.txt"],"order":0,"extra":1}"#,
                "line 1",
            ),
        ];
        for (file, text, want) in cases {
            let t = store_with(&[(file, text)]);
            let e = format!("{:#}", Store::load(&t.0).err().expect(want));
            assert!(e.contains(want), "want `{want}` in: {e}");
        }
    }
}

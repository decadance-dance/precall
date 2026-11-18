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

    /// The serialized map: one text per non-empty branch file (`map/<id>.jsonl`), lines sorted by id.
    pub fn render_map(&self) -> Result<BTreeMap<String, String>> {
        let mut files: BTreeMap<String, String> = BTreeMap::new();
        for n in self.nodes.values().filter(|n| n.parent.is_some()) {
            let line = serde_json::to_string(n)?;
            let text = files
                .entry(format!("map/{}.jsonl", self.branch_of(&n.id)))
                .or_default();
            text.push_str(&line);
            text.push('\n');
        }
        Ok(files)
    }

    /// Branch files whose content differs from the file on disk; `None` removes an emptied file.
    /// Line endings do not count, so a file checked out with CRLF is left alone when its nodes did not change.
    pub fn map_changes(&self) -> Result<Vec<Change>> {
        let files = self.render_map()?;
        let mut changes = Vec::new();
        for b in &self.branches {
            let rel = format!("map/{}.jsonl", b.id);
            let new = files.get(&rel).cloned();
            let disk = std::fs::read_to_string(self.root.join(&rel)).ok();
            let same = match (&disk, &new) {
                (Some(d), Some(n)) => d.replace("\r\n", "\n") == *n,
                (d, n) => d.is_none() && n.is_none(),
            };
            if !same {
                changes.push((rel, new));
            }
        }
        Ok(changes)
    }

    /// Writes the branch files that changed.
    pub fn save(&self) -> Result<()> {
        write_files(&self.root, &self.map_changes()?)
    }

    /// Writes the branch files that changed plus `extra`, and commits exactly those files.
    pub fn save_and_commit(&self, extra: Vec<Change>, message: &str) -> Result<bool> {
        let mut changes = extra;
        changes.extend(self.map_changes()?);
        write_and_commit(&self.root, &changes, message)
    }
}

/// A file a command writes: its path in the store and its new content (`None` removes it).
pub type Change = (String, Option<String>);

fn write_files(root: &Path, changes: &[Change]) -> Result<()> {
    for (rel, new) in changes {
        let path = root.join(rel);
        match new {
            Some(text) => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, text)
            }
            None if path.exists() => std::fs::remove_file(&path),
            None => Ok(()),
        }
        .with_context(|| format!("writing {rel}"))?;
    }
    Ok(())
}

/// Writes `changes` and commits exactly those paths: other files, staged or not, stay out of the
/// commit. If the write or the commit fails, the files get their previous content back.
pub fn write_and_commit(root: &Path, changes: &[Change], message: &str) -> Result<bool> {
    if changes.is_empty() {
        return Ok(false);
    }
    let paths: Vec<&str> = changes.iter().map(|(rel, _)| rel.as_str()).collect();
    let before: Vec<Option<Vec<u8>>> = paths
        .iter()
        .map(|rel| std::fs::read(root.join(rel)).ok())
        .collect();
    let committed =
        match write_files(root, changes).and_then(|()| commit_paths(root, &paths, message)) {
            Ok(committed) => committed,
            Err(mut e) => {
                // Nothing was committed: put the files back as they were.
                for (rel, old) in paths.iter().zip(before) {
                    let path = root.join(rel);
                    let restored = match old {
                        Some(bytes) => std::fs::write(&path, bytes),
                        None if path.exists() => std::fs::remove_file(&path),
                        None => Ok(()),
                    };
                    if let Err(r) = restored {
                        e = e.context(format!("and restoring {rel} failed: {r}"));
                    }
                }
                return Err(e);
            }
        };
    if committed {
        // The commit landed, so the files stay; only the index can lag behind it.
        let mut reset = vec!["reset", "-q", "--"];
        reset.extend(&paths);
        git_status(root, None, &reset).with_context(|| {
            let subject = message.lines().next().unwrap_or("");
            format!(
                "the commit `{subject}` landed, but updating the store's git index failed; \
                 once no other git process uses the store, run: git -C {} {}",
                root.display(),
                reset.join(" ")
            )
        })?;
    }
    Ok(committed)
}

/// Commits the given paths through a private index started from HEAD, leaving the real index alone.
fn commit_paths(root: &Path, paths: &[&str], message: &str) -> Result<bool> {
    let index = root.join(".git").join("rrecall-index");
    let _ = std::fs::remove_file(&index);
    let has_head = git_run(root, None, &["rev-parse", "--verify", "-q", "HEAD"])?
        .status
        .success();
    let committed = (|| -> Result<bool> {
        if has_head {
            git_in(root, Some(&index), &["read-tree", "HEAD"])?;
        }
        let mut add = vec!["add", "-A", "--"];
        add.extend(paths);
        git_in(root, Some(&index), &add)?;
        if has_head && git_status(root, Some(&index), &["diff", "--cached", "--quiet", "HEAD"])? {
            return Ok(false);
        }
        git_in(root, Some(&index), &["commit", "-q", "-m", message])?;
        Ok(true)
    })();
    let _ = std::fs::remove_file(&index);
    committed
}

/// Runs git in `dir` as rrecall (store commits need no configured identity and are never signed).
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    git_in(dir, None, args)
}

fn git_in(dir: &Path, index: Option<&Path>, args: &[&str]) -> Result<String> {
    let o = git_run(dir, index, args)?;
    if !o.status.success() {
        return Err(git_failed(dir, args, &o));
    }
    String::from_utf8(o.stdout).context("git printed non-UTF-8 output")
}

/// True when git exits 0, false when it exits 1; an error otherwise.
fn git_status(dir: &Path, index: Option<&Path>, args: &[&str]) -> Result<bool> {
    let o = git_run(dir, index, args)?;
    match o.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(git_failed(dir, args, &o)),
    }
}

fn git_failed(dir: &Path, args: &[&str], o: &std::process::Output) -> anyhow::Error {
    anyhow::anyhow!(
        "git {} failed in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&o.stderr).trim()
    )
}

fn git_run(dir: &Path, index: Option<&Path>, args: &[&str]) -> Result<std::process::Output> {
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
    if let Some(i) = index {
        c.env("GIT_INDEX_FILE", i);
    }
    c.output().context("running git")
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
        // The serializer reproduces the stored bytes exactly, so nothing needs writing.
        assert_eq!(s.render_map().unwrap()["map/layout.jsonl"], LAYOUT);
        assert!(s.map_changes().unwrap().is_empty());
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

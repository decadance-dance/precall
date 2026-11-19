//! End-to-end tests of the built-in collectors `files`, `git-history` and `github` (spec §10)
//! over fixture repositories built in temp dirs, with a fake `gh` and fake runners on `PATH`.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

/// An isolated world: HOME, XDG dirs, the store `widget` and a fixture repo under `src/widget`.
struct W {
    root: PathBuf,
}

impl Drop for W {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A TOML (and JSON) string literal.
fn q(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

const CHECKS: &str = "\n[[plugins]]\nid = \"no-op\"\n\n[[plugins]]\nid = \"size-limits\"\n\n[[plugins]]\nid = \"secrets\"\n";

impl W {
    fn new() -> W {
        static N: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "rrecall-collectors-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["home", "data", "config", "work", "rec", "src/widget"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let w = W {
            root: root.canonicalize().unwrap(),
        };
        let o = w.raw(&["init", "widget"], None, &[]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        w.git(&["init", "-q", "-b", "main"]);
        w
    }

    fn repo(&self) -> PathBuf {
        self.root.join("src/widget")
    }

    fn store(&self) -> PathBuf {
        self.root.join("data/rrecall/widget")
    }

    fn path(&self) -> String {
        format!(
            "{}/tests/fixtures/bin:{}",
            env!("CARGO_MANIFEST_DIR"),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    /// Runs rrecall with `stdin` and extra environment.
    fn raw(&self, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Out {
        let mut c = Command::new(env!("CARGO_BIN_EXE_rrecall"));
        c.args(args)
            .current_dir(self.root.join("work"))
            .env("HOME", self.root.join("home"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("PATH", self.path())
            .env("FAKE_GH_DIR", self.root.join("rec"))
            .env_remove("RRECALL_PROJECT")
            .env_remove("GIT_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            c.env(k, v);
        }
        let mut child = c.spawn().unwrap();
        {
            use std::io::Write;
            let mut si = child.stdin.take().unwrap();
            si.write_all(stdin.unwrap_or("").as_bytes()).unwrap();
        }
        let o = child.wait_with_output().unwrap();
        Out {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8(o.stdout).unwrap(),
            stderr: String::from_utf8(o.stderr).unwrap(),
        }
    }

    /// `rrecall run` on the store; asserts the exit code.
    fn run(&self, code: i32) -> Out {
        let o = self.raw(&["--project", "widget", "run"], None, &[]);
        assert_eq!(o.code, code, "stdout: {}\nstderr: {}", o.stdout, o.stderr);
        o
    }

    /// Replaces the store config with `plugins` and the built-in checks, and commits it.
    fn config(&self, plugins: &str) {
        let text = format!(
            "repo = {}\n{plugins}{CHECKS}",
            q(&self.repo().to_string_lossy())
        );
        std::fs::write(self.store().join("rrecall.toml"), text).unwrap();
        git_in(&self.store(), &["add", "rrecall.toml"]);
        git_in(&self.store(), &["commit", "-q", "-m", "config"]);
    }

    fn git(&self, args: &[&str]) -> String {
        git_in(&self.repo(), args)
    }

    /// Writes `rel` in the fixture repo and commits it on the current branch.
    fn commit_file(&self, rel: &str, text: &str, msg: &str) {
        let p = self.repo().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
        self.git(&["add", rel]);
        self.git(&["commit", "-q", "-m", msg]);
    }

    /// A branch with one commit, merged into `main` with `--no-ff` and message `msg`.
    fn merge(&self, branch: &str, msg: &str) {
        self.git(&["checkout", "-q", "-b", branch]);
        self.commit_file(&format!("{}.txt", branch.replace('/', "-")), branch, "work");
        self.git(&["checkout", "-q", "main"]);
        self.git(&["merge", "-q", "--no-ff", "-m", msg, branch]);
    }

    fn map(&self, branch: &str) -> Vec<Value> {
        std::fs::read_to_string(self.store().join(format!("map/{branch}.jsonl")))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn store_head(&self) -> String {
        git_in(&self.store(), &["rev-parse", "HEAD"])
    }

    /// `rrecall builtin <id> <entry>` with params as `RRECALL_PARAM_*`.
    fn builtin(&self, id: &str, entry: &str, params: &[(&str, &str)], stdin: &str) -> Out {
        let env: Vec<(String, &str)> = params
            .iter()
            .map(|(k, v)| (format!("RRECALL_PARAM_{}", k.to_uppercase()), *v))
            .collect();
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        self.raw(&["builtin", id, entry], Some(stdin), &env)
    }
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@localhost",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-C",
        ])
        .arg(dir)
        .args(args)
        .env("GIT_COMMITTER_DATE", "2026-03-04T05:06:07Z")
        .env("GIT_AUTHOR_DATE", "2026-03-04T05:06:07Z")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

fn titles(nodes: &[Value]) -> Vec<String> {
    let mut t: Vec<String> = nodes
        .iter()
        .map(|n| n["title"].as_str().unwrap().to_string())
        .collect();
    t.sort();
    t
}

fn texts(o: &Out) -> serde_json::Map<String, Value> {
    assert_eq!(o.code, 0, "{}", o.stderr);
    let v: Value = serde_json::from_str(&o.stdout).unwrap();
    v["texts"].as_object().unwrap().clone()
}

// --- git-history ---

fn history_config(w: &W) {
    w.config(&format!(
        "\n[[plugins]]\nid = \"git-history\"\nparams = {{ path = {}, repo = \"acme/widget\" }}\n",
        q(&w.repo().to_string_lossy())
    ));
}

#[test]
fn git_history_proposes_a_done_node_per_merged_branch_until_there_are_releases() {
    let w = W::new();
    w.commit_file("README.md", "widget\n", "init");
    w.merge(
        "feature/parser",
        "Merge pull request #1 from acme/feature/parser\n\nAdd the parser",
    );
    w.merge("feature/lexer", "Merge branch 'feature/lexer'");
    history_config(&w);
    w.run(0);
    let done = w.map("done");
    assert_eq!(titles(&done), ["feature/lexer", "feature/parser"]);
    let parser = done
        .iter()
        .find(|n| n["title"] == "feature/parser")
        .unwrap();
    assert_eq!(parser["body"], "Add the parser");
    assert_eq!(parser["date"], "2026-03-04");
    assert_eq!(parser["state"], "done");
    let sha = w.git(&["rev-parse", "--short=12", "HEAD~1"]);
    assert_eq!(
        parser["sources"],
        json!([format!("commit:acme/widget@{sha}")])
    );

    // A release: one node for the tag, its annotation as the body; the branch nodes stay.
    w.merge("feature/cli", "Merge branch 'feature/cli'");
    w.git(&[
        "tag",
        "-a",
        "v0.1.0",
        "-m",
        "First release\n\nParser, lexer and CLI.",
    ]);
    w.run(0);
    let done = w.map("done");
    assert_eq!(
        titles(&done),
        ["feature/lexer", "feature/parser", "v0.1.0"],
        "merges after the newest tag wait for the next release"
    );
    let rel = done.iter().find(|n| n["title"] == "v0.1.0").unwrap();
    assert_eq!(rel["body"], "First release Parser, lexer and CLI.");

    // Unchanged sources, and a commit on an unmerged branch: no run, no commit (I1).
    let head = w.store_head();
    w.git(&["checkout", "-q", "-b", "feature/wip"]);
    w.commit_file("wip.txt", "wip", "wip");
    w.git(&["checkout", "-q", "main"]);
    let o = w.run(0);
    assert!(o.stdout.contains("fingerprint unchanged"), "{}", o.stdout);
    assert_eq!(w.store_head(), head);

    // A lightweight tag: its body lists the branches merged since the previous tag.
    w.merge(
        "feature/docs",
        "Merge pull request #4 from acme/feature/docs\n\nWrite docs",
    );
    w.git(&["tag", "v0.2.0"]);
    w.run(0);
    let done = w.map("done");
    let rel = done.iter().find(|n| n["title"] == "v0.2.0").unwrap();
    assert_eq!(rel["body"], "Merged: feature/docs.");
}

#[test]
fn git_history_resolves_its_own_commits_and_nothing_else() {
    let w = W::new();
    w.commit_file("README.md", "widget\n", "init\n\nThe first commit.");
    let sha = w.git(&["rev-parse", "--short=12", "HEAD"]);
    let path = w.repo().to_string_lossy().into_owned();
    let refs = json!({"refs": [
        format!("commit:acme/widget@{sha}"),
        format!("commit:acme/other@{sha}"),
        "commit:acme/widget@--output=x",
        "commit:acme/widget@0123456789ab",
    ]});
    let t = texts(&w.builtin(
        "git-history",
        "resolve",
        &[("path", &path), ("repo", "acme/widget")],
        &refs.to_string(),
    ));
    assert_eq!(t.len(), 4);
    let own = t[&format!("commit:acme/widget@{sha}")].as_str().unwrap();
    assert!(own.contains("The first commit."), "{own}");
    for r in [
        format!("commit:acme/other@{sha}"),
        "commit:acme/widget@--output=x".into(),
        "commit:acme/widget@0123456789ab".into(),
    ] {
        assert_eq!(t[&r], Value::Null, "{r}");
    }
}

//! End-to-end tests of `rrecall run` (spec §4, §5) with fake plugins: one test per invariant,
//! the lock, back pressure, the cost cap, hooks, the commit message, the report and exit codes.

use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn fake() -> String {
    format!(
        "{}/tests/fixtures/bin/fake-plugin",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// A TOML (and JSON) string literal.
fn q(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

/// An isolated world: HOME, XDG dirs, the store `kiln` with a catalog of fake plugins.
struct W {
    root: PathBuf,
}

impl Drop for W {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const SCOPE: &str = r#"["layout", "how-it-works", "decisions", "done"]"#;

impl W {
    fn new() -> W {
        static N: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "rrecall-run-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["home", "data", "config", "work", "rec", "src"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let w = W { root };
        let o = w.raw(&["init", "kiln"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        std::fs::write(w.fp(), "v1").unwrap();
        let fake = q(&fake());
        let fp = q(&w.fp().to_string_lossy());
        let common = "scenario = \"ok\"\nout = \"\"\nrecord = \"\"\n";
        for (id, kind, table, extra) in [
            (
                "col",
                "collector",
                format!(
                    "[collector]\nscope = {SCOPE}\nsource_kinds = [\"{{kind}}\"]\nfingerprint = [\"/bin/cat\", \"{{fp}}\"]\ncommand = [{fake}]\n"
                ),
                format!("fp = {fp}\nkind = \"file\"\n"),
            ),
            (
                "slow",
                "collector",
                format!(
                    "[collector]\nscope = {SCOPE}\nsource_kinds = [\"{{kind}}\"]\nmin_interval = \"1d\"\ncommand = [{fake}]\n"
                ),
                "kind = \"slack\"\n".into(),
            ),
            (
                "prompted",
                "collector",
                format!(
                    "[collector]\nscope = {SCOPE}\nsource_kinds = [\"{{kind}}\"]\nfingerprint = [\"/bin/cat\", \"{{fp}}\"]\nprompt = \"p.md\"\nrunner = \"runner\"\nmodel = \"fake-model\"\n"
                ),
                format!("fp = {fp}\nkind = \"note\"\n"),
            ),
            (
                "check",
                "check",
                format!("[check]\ncommand = [{fake}]\n"),
                String::new(),
            ),
            (
                "judge",
                "judge",
                format!("[judge]\ncommand = [{fake}]\n"),
                String::new(),
            ),
            (
                "pjudge",
                "judge",
                "[judge]\nprompt = \"p.md\"\nrunner = \"runner\"\n".into(),
                String::new(),
            ),
            (
                "runner",
                "runner",
                format!("[runner]\ncommand = [{fake}]\n"),
                String::new(),
            ),
            (
                "gate",
                "hook",
                format!("[hook]\nevents = [\"run.before\"]\ncommand = [{fake}]\n"),
                String::new(),
            ),
            (
                "cgate",
                "hook",
                format!("[hook]\nevents = [\"commit.before\"]\ncommand = [{fake}]\n"),
                String::new(),
            ),
            (
                "audit",
                "hook",
                format!(
                    "[hook]\nevents = [\"collect.after\", \"diff.after\", \"commit.after\", \"view.render\"]\ncommand = [{fake}]\n"
                ),
                String::new(),
            ),
            (
                "view",
                "view",
                format!("[view]\ncommand = [{fake}]\n"),
                String::new(),
            ),
        ] {
            let d = w.store().join("plugins").join(id);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("p.md"), "Propose changes.\n").unwrap();
            std::fs::write(
                d.join("rrecall-plugin.toml"),
                format!(
                    "id = \"{id}\"\nname = \"Fake {id}\"\nversion = \"0.1.0\"\nmin_rrecall = \"0.1.0\"\nkind = \"{kind}\"\n\n{table}\n[params]\n{common}{extra}"
                ),
            )
            .unwrap();
        }
        w
    }

    fn store(&self) -> PathBuf {
        self.root.join("data/rrecall/kiln")
    }

    fn fp(&self) -> PathBuf {
        self.root.join("src/fp")
    }

    fn rec(&self, inst: &str) -> PathBuf {
        self.root.join("rec").join(inst)
    }

    fn raw(&self, args: &[&str]) -> Out {
        let o = Command::new(env!("CARGO_BIN_EXE_rrecall"))
            .args(args)
            .current_dir(self.root.join("work"))
            .env("HOME", self.root.join("home"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env_remove("RRECALL_PROJECT")
            .env_remove("GIT_DIR")
            .output()
            .unwrap();
        Out {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8(o.stdout).unwrap(),
            stderr: String::from_utf8(o.stderr).unwrap(),
        }
    }

    fn cmd(&self, args: &[&str]) -> Out {
        let mut all = vec!["--project", "kiln"];
        all.extend_from_slice(args);
        self.raw(&all)
    }

    fn run(&self, args: &[&str]) -> Out {
        let mut all = vec!["run"];
        all.extend_from_slice(args);
        self.cmd(&all)
    }

    /// Runs and asserts the exit code.
    fn run_code(&self, args: &[&str], code: i32) -> Out {
        let o = self.run(args);
        assert_eq!(
            o.code,
            code,
            "run {args:?}\nstdout: {}\nstderr: {}\nreport: {}",
            o.stdout,
            o.stderr,
            self.report()
        );
        o
    }

    /// One `[[plugins]]` entry; `params` is the inside of an inline table (may be empty).
    fn inst(&self, id: &str, uses: &str, params: &str) -> String {
        let rec = q(&self.rec(id).to_string_lossy());
        let sep = if params.is_empty() { "" } else { ", " };
        format!(
            "\n[[plugins]]\nid = \"{id}\"\nuse = \"{uses}\"\nparams = {{ record = {rec}{sep}{params} }}\n"
        )
    }

    /// Replaces the store config (top-level keys first) and commits it.
    fn config(&self, text: &str) {
        std::fs::write(self.store().join("rrecall.toml"), text).unwrap();
        self.commit("config", &["rrecall.toml"]);
    }

    /// Writes `map/<branch>.jsonl` and commits it.
    fn seed(&self, branch: &str, lines: &[&str]) {
        let rel = format!("map/{branch}.jsonl");
        std::fs::create_dir_all(self.store().join("map")).unwrap();
        std::fs::write(
            self.store().join(&rel),
            lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
        )
        .unwrap();
        self.commit("seed", &[&rel]);
    }

    fn commit(&self, msg: &str, paths: &[&str]) {
        let mut add = vec!["add", "--"];
        add.extend_from_slice(paths);
        self.git(&add);
        self.git(&["commit", "-q", "-m", msg]);
    }

    fn git(&self, args: &[&str]) -> String {
        let o = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@localhost",
                "-c",
                "commit.gpgsign=false",
                "-C",
            ])
            .arg(self.store())
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8(o.stdout).unwrap()
    }

    fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"]).trim().to_string()
    }

    fn message(&self) -> String {
        self.git(&["log", "-1", "--format=%B"])
    }

    /// What a run must leave alone when it changes nothing: HEAD, the tracked files' bytes,
    /// the working tree and the index.
    fn snapshot(&self) -> String {
        let mut s = format!("{}\n{}", self.head(), self.git(&["status", "--porcelain"]));
        for f in self.git(&["ls-files"]).lines() {
            s += &format!(
                "--- {f}\n{}",
                std::fs::read_to_string(self.store().join(f)).unwrap()
            );
        }
        s
    }

    fn calls(&self, inst: &str) -> usize {
        std::fs::read_to_string(self.rec(inst).with_extension("calls"))
            .map_or(0, |t| t.lines().count())
    }

    /// The stdin of the `n`th call of an instance, as JSON.
    fn input(&self, inst: &str, n: usize) -> Value {
        let p = self.rec(inst).with_extension(format!("stdin.{n}"));
        serde_json::from_str(
            &std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())),
        )
        .unwrap()
    }

    fn map(&self, branch: &str) -> String {
        std::fs::read_to_string(self.store().join(format!("map/{branch}.jsonl")))
            .unwrap_or_default()
    }

    fn lock(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.store().join("rrecall.lock")).unwrap())
            .unwrap()
    }

    /// The last run's report.
    fn report(&self) -> String {
        let runs = self.store().join("runs");
        let last = std::fs::read_dir(&runs)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| runs.join(n).join("report.md").is_file())
            .max();
        last.map(|n| std::fs::read_to_string(runs.join(n).join("report.md")).unwrap())
            .unwrap_or_default()
    }
}

const API: &str = r#"{"id":"api","parent":"layout","title":"API","body":"HTTP API.","sources":["file:api.md","gh-pr:o/r#1"],"order":0}"#;
const AUTH: &str = r#"{"id":"auth","parent":"api","title":"Auth","body":"Tokens.","sources":["file:auth.md"],"order":0}"#;
const NOTED: &str = r#"{"id":"noted","parent":"layout","title":"Noted","body":"","sources":["file:n.md"],"order":1,"note":"keep"}"#;
const THEIRS: &str = r#"{"id":"theirs","parent":"layout","title":"Theirs","body":"","sources":["gh-pr:o/r#2"],"order":2}"#;

fn props(items: &[Value]) -> String {
    json!({ "proposals": items }).to_string()
}

fn add(key: &str, parent: &str, title: &str) -> Value {
    json!({"key": key, "op": "add", "parent": parent, "title": title, "body": format!("{title} body."), "sources": [format!("file:{key}.md")], "reason": format!("found {title}")})
}

/// A seeded store with `repo` (kind `file`) returning `out`, and an accepting check.
fn world(out: &str) -> W {
    let w = W::new();
    w.seed("layout", &[API, AUTH, NOTED, THEIRS]);
    w.config(&format!(
        "{}{}{}",
        w.inst("repo", "col", &format!("out = {}", q(out))),
        w.inst(
            "github",
            "col",
            &format!("kind = \"gh-pr\", out = {}", q(&props(&[])))
        ),
        w.inst("check", "check", "scenario = \"all\", out = \"accept\""),
    ));
    w
}

// ---- I1 and fingerprints (spec §4.1 step 2, §4.2) ----

#[test]
fn i1_a_second_run_over_unchanged_sources_calls_no_collector_and_changes_nothing() {
    let w = world(&props(&[add("a", "layout", "Queue")]));
    w.run_code(&[], 0);
    assert_eq!(w.calls("repo"), 1);
    assert!(
        w.map("layout").contains(r#""id":"queue""#),
        "{}",
        w.map("layout")
    );
    let before = w.snapshot();
    let o = w.run_code(&[], 0);
    assert_eq!(w.calls("repo"), 1, "no collector command ran");
    assert_eq!(w.calls("check"), 1, "no check ran");
    assert_eq!(
        w.snapshot(),
        before,
        "the store is byte-identical, no commit"
    );
    assert!(
        w.report().contains("fingerprint unchanged"),
        "{}",
        w.report()
    );
    assert!(o.stdout.contains("nothing to collect"), "{}", o.stdout);
}

#[test]
fn a_changed_fingerprint_collects_again_and_force_ignores_fingerprints() {
    let w = world(&props(&[]));
    w.run_code(&[], 0);
    let fp1 = w.lock()["collectors"]["repo"]["fingerprint"].clone();
    std::fs::write(w.fp(), "v2").unwrap();
    w.run_code(&[], 0);
    assert_eq!((w.calls("repo"), w.calls("github")), (2, 2));
    assert_ne!(w.lock()["collectors"]["repo"]["fingerprint"], fp1);
    let before = w.snapshot();
    w.run_code(&["--force"], 0);
    assert_eq!((w.calls("repo"), w.calls("github")), (3, 3));
    assert_eq!(
        w.snapshot(),
        before,
        "a forced run over unchanged sources writes nothing"
    );
    let lock = w.lock();
    assert!(
        lock["collectors"]["repo"]["hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(lock["collectors"]["repo"]["plugin"], "plugins/col");
    assert!(lock["collectors"]["repo"].get("settled_at").is_none());
    assert_eq!(lock["plugins"]["check"]["plugin"], "plugins/check");
}

#[test]
fn the_lock_names_a_builtin_check_builtin_and_hashes_its_manifest() {
    let w = W::new();
    w.config(&format!(
        "{}\n[[plugins]]\nid = \"no-op\"\n\n[[plugins]]\nid = \"secrets\"\n",
        w.inst(
            "repo",
            "col",
            &format!("out = {}", q(&props(&[add("a", "layout", "Queue")])))
        ),
    ));
    w.run_code(&[], 0);
    let lock = w.lock();
    // A built-in has no directory of its own: never the directory the binary lives in.
    assert_eq!(lock["plugins"]["no-op"]["plugin"], "builtin", "{lock}");
    let h = lock["plugins"]["no-op"]["hash"].as_str().unwrap();
    assert!(h.starts_with("sha256:") && h.len() == 7 + 64, "{h}");
    assert_ne!(h, lock["plugins"]["secrets"]["hash"].as_str().unwrap());
}

#[test]
fn only_runs_one_collector_instance() {
    let w = world(&props(&[]));
    w.run_code(&["--only", "github"], 0);
    assert_eq!((w.calls("repo"), w.calls("github")), (0, 1));
    assert!(w.lock()["collectors"].get("repo").is_none());
    for bad in ["nope", "check"] {
        let o = w.run_code(&["--only", bad], 2);
        assert!(o.stderr.contains(bad), "{}", o.stderr);
    }
}

#[test]
fn a_min_interval_collector_waits_for_its_interval() {
    let w = W::new();
    w.config(&w.inst("chat", "slow", &format!("out = {}", q(&props(&[])))));
    w.run_code(&[], 0);
    w.run_code(&[], 0);
    assert_eq!(w.calls("chat"), 1, "not due within a day");
    let lock = w.lock();
    let at = lock["collectors"]["chat"]["settled_at"].as_str().unwrap();
    assert!(at.ends_with('Z') && at.len() == 20, "{at}");
    let text = std::fs::read_to_string(w.store().join("rrecall.lock")).unwrap();
    std::fs::write(
        w.store().join("rrecall.lock"),
        text.replace(at, "2000-01-01T00:00:00Z"),
    )
    .unwrap();
    w.commit("old lock", &["rrecall.lock"]);
    w.run_code(&[], 0);
    assert_eq!(w.calls("chat"), 2, "due once the interval passed");
}

// ---- The invariants of spec §5 ----

#[test]
fn i2_a_collector_that_forgets_a_node_removes_nothing() {
    let w = world(&props(&[]));
    let before = w.map("layout");
    w.run_code(&[], 0);
    assert_eq!(w.map("layout"), before);
    assert!(
        w.message().starts_with("run ") && w.message().contains(": no map changes"),
        "{}",
        w.message()
    );
}

#[test]
fn a_rewording_is_rejected_by_the_check_and_goes_back_to_its_collector() {
    let reword = json!({"key": "a", "op": "update", "id": "api", "title": "api", "body": "HTTP api", "sources": ["file:api.md"], "reason": "reworded"});
    let w = W::new();
    w.seed("layout", &[API]);
    w.config(&format!(
        "{}{}",
        w.inst("repo", "col", &format!("scenario = \"seq\", out = {}", q(&format!("{}\n{}", props(&[reword]), props(&[]))))),
        // A stand-in for the built-in `no-op` check (#6).
        w.inst("no-op", "check", &format!("out = {}", q(r#"{"verdicts":[{"key":"repo/a","verdict":"reject","reason":"only the wording changed"}]}"#))),
    ));
    let before = w.map("layout");
    w.run_code(&[], 0);
    assert_eq!(w.map("layout"), before);
    assert_eq!(w.calls("repo"), 2, "the rejection went back");
    let r = &w.input("repo", 2)["rejections"][0];
    assert_eq!(r["proposal"]["key"], "a");
    assert_eq!(r["verdicts"][0]["by"], "no-op");
    assert_eq!(r["verdicts"][0]["reason"], "only the wording changed");
    assert_eq!(w.input("repo", 2)["round"], 2);
    assert!(
        w.report().contains("only the wording changed"),
        "{}",
        w.report()
    );
}

#[test]
fn i2_a_removal_without_evidence_is_rejected_by_the_core() {
    let rm = json!({"key": "a", "op": "remove", "id": "auth", "sources": [], "reason": "not seen"});
    let w = world(&props(&[rm]));
    w.run_code(&[], 0);
    assert!(w.map("layout").contains(r#""id":"auth""#));
    let v = &w.input("repo", 2)["rejections"][0]["verdicts"][0];
    assert_eq!(v["by"], "core");
    assert!(v["reason"].as_str().unwrap().contains("I2"), "{v}");
    assert_eq!(w.calls("check"), 0, "no plugin saw it");
}

#[test]
fn i3_a_node_with_a_note_or_children_is_not_removed() {
    let rm = |k: &str, id: &str| json!({"key": k, "op": "remove", "id": id, "sources": [], "evidence": "file:gone.md", "reason": "gone"});
    let w = world(&props(&[rm("a", "api"), rm("b", "noted")]));
    w.run_code(&[], 0);
    for id in ["api", "auth", "noted"] {
        assert!(
            w.map("layout").contains(&format!("\"id\":\"{id}\"")),
            "{id}"
        );
    }
    let report = w.report();
    assert!(
        report.contains("I3") && report.contains("rrecall rm"),
        "{report}"
    );
    // With the child removed too, both go.
    let w = world(&props(&[rm("a", "api"), rm("b", "auth")]));
    w.run_code(&[], 0);
    assert!(
        !w.map("layout").contains("\"id\":\"api\"") && !w.map("layout").contains("\"id\":\"auth\"")
    );
}

#[test]
fn i8_a_collector_may_only_cite_another_collectors_node() {
    let edit = json!({"key": "a", "op": "update", "id": "theirs", "body": "mine now", "sources": ["file:x.md"], "reason": "r"});
    let cite = json!({"key": "b", "op": "add_source", "id": "theirs", "sources": ["file:x.md"], "reason": "also here"});
    // Round 2 proposes nothing: once `b` cites the node with a `file` source, I8 lets `repo`
    // update it, as the spec says.
    let w = world(&format!("{}\n{}", props(&[edit, cite]), props(&[])));
    let cfg = std::fs::read_to_string(w.store().join("rrecall.toml")).unwrap();
    w.config(&cfg.replacen(
        "params = { record",
        "params = { scenario = \"seq\", record",
        1,
    ));
    w.run_code(&[], 0);
    let line = w
        .map("layout")
        .lines()
        .find(|l| l.contains("\"id\":\"theirs\""))
        .unwrap()
        .to_string();
    assert!(line.contains(r#""body":"""#), "{line}");
    assert!(
        line.contains(r#""sources":["gh-pr:o/r#2","file:x.md"]"#),
        "{line}"
    );
    let v = &w.input("repo", 2)["rejections"][0]["verdicts"][0];
    assert!(v["reason"].as_str().unwrap().contains("I8"), "{v}");
}

#[test]
fn i9_two_collectors_that_change_one_node_are_both_sent_back_with_the_other() {
    let w = W::new();
    w.seed("layout", &[API]);
    let mine = json!({"key": "m", "op": "update", "id": "api", "body": "Mine.", "sources": ["file:api.md"], "reason": "r"});
    let theirs = json!({"key": "t", "op": "update", "id": "api", "body": "Theirs.", "sources": ["gh-pr:o/r#1"], "reason": "r"});
    w.config(&format!(
        "{}{}",
        w.inst("repo", "col", &format!("out = {}", q(&props(&[mine])))),
        w.inst(
            "github",
            "col",
            &format!("kind = \"gh-pr\", out = {}", q(&props(&[theirs])))
        ),
    ));
    w.run_code(&[], 0);
    assert!(
        w.map("layout").contains("HTTP API."),
        "neither won: {}",
        w.map("layout")
    );
    assert_eq!((w.calls("repo"), w.calls("github")), (2, 2));
    let v = &w.input("repo", 2)["rejections"][0]["verdicts"][0];
    assert_eq!(v["by"], "core");
    assert_eq!(v["reason"], "conflict");
    assert_eq!(v["conflict_with"]["key"], "t");
    assert_eq!(
        w.input("github", 2)["rejections"][0]["verdicts"][0]["conflict_with"]["key"],
        "m"
    );
}

#[test]
fn i10_a_crashing_collector_is_unreachable_and_collected_again() {
    let w = world(&props(&[]));
    w.config(&format!(
        "{}{}",
        w.inst("repo", "col", "scenario = \"crash\""),
        w.inst("github", "col", &format!("kind = \"gh-pr\", out = {}", q(&props(&[json!({"key": "a", "op": "add", "parent": "layout", "title": "Release 1", "body": "", "sources": ["gh-pr:o/r#5"], "reason": "released"})])))),
    ));
    let github = w.inst("github", "col", &format!("kind = \"gh-pr\", out = {}", q(&props(&[json!({"key": "a", "op": "add", "parent": "layout", "title": "Release 1", "body": "", "sources": ["gh-pr:o/r#5"], "reason": "released"})]))));
    let o = w.run_code(&[], 5);
    assert!(o.stderr.contains("repo"), "{}", o.stderr);
    assert!(
        w.map("layout").contains("release-1"),
        "what was accepted is committed"
    );
    let msg = w.message();
    assert!(
        msg.lines().any(|l| l.starts_with("unreachable: repo (")),
        "{msg}"
    );
    assert!(w.lock()["collectors"].get("repo").is_none());
    assert!(w.lock()["collectors"].get("github").is_some());
    w.config(&format!(
        "{}{github}",
        w.inst("repo", "col", &format!("out = {}", q(&props(&[]))))
    ));
    w.run_code(&[], 0);
    assert_eq!(
        (w.calls("repo"), w.calls("github")),
        (2, 1),
        "only the unsettled one runs again"
    );
}

#[test]
fn i10_a_judge_that_times_out_drops_its_proposals_and_leaves_the_collector_unsettled() {
    let w = world(&props(&[add("a", "layout", "Queue")]));
    let cfg = std::fs::read_to_string(w.store().join("rrecall.toml")).unwrap();
    w.config(&format!(
        "{cfg}{}timeout = \"1s\"\n",
        w.inst("judge", "judge", "scenario = \"sleep\"")
    ));
    let o = w.run_code(&[], 5);
    assert!(!w.map("layout").contains("queue"));
    assert!(o.stderr.contains("judge"), "{}", o.stderr);
    assert!(w.lock()["collectors"].get("repo").is_none());
    assert!(w.report().contains("timed out"), "{}", w.report());
    assert_eq!(w.calls("repo"), 1, "a dropped proposal does not go back");
    w.config(&cfg);
    w.run_code(&[], 0);
    assert_eq!(w.calls("repo"), 2, "collected again");
    assert!(w.map("layout").contains("queue"));
}

#[test]
fn i10_a_forced_run_that_drops_proposals_makes_the_collector_due_again() {
    let w = world(&props(&[]));
    let seq = format!("{}\n{}", props(&[]), props(&[add("a", "layout", "Queue")]));
    let repo = w.inst(
        "repo",
        "col",
        &format!("scenario = \"seq\", out = {}", q(&seq)),
    );
    let judge = w.inst("judge", "judge", "scenario = \"sleep\"");
    w.config(&format!("{repo}{judge}timeout = \"1s\"\n"));
    w.run_code(&[], 0);
    let settled = w.lock()["collectors"]["repo"].clone();
    w.run_code(&["--force"], 5);
    assert_eq!(w.calls("repo"), 2);
    let entry = &w.lock()["collectors"]["repo"];
    assert_eq!(
        entry["fingerprint"], settled["fingerprint"],
        "the settled one stays"
    );
    assert_eq!(entry["unsettled"], true, "{entry}");
    w.config(&repo);
    w.run_code(&[], 0);
    assert_eq!(
        w.calls("repo"),
        3,
        "an unchanged fingerprint does not hide the failure"
    );
    assert!(w.map("layout").contains("queue"), "{}", w.map("layout"));
    assert!(w.lock()["collectors"]["repo"].get("unsettled").is_none());
}

// ---- Back pressure, parents, the commit message (spec §4.1, §4.3) ----

#[test]
fn back_pressure_stops_after_max_rounds_and_reports_what_is_left() {
    let w = world(&props(&[add("a", "layout", "Queue")]));
    let cfg = std::fs::read_to_string(w.store().join("rrecall.toml")).unwrap();
    w.config(&format!(
        "max_rounds = 3\n{cfg}{}",
        w.inst("judge", "judge", "scenario = \"all\", out = \"reject\"")
    ));
    w.run_code(&[], 0);
    assert_eq!(w.calls("repo"), 3);
    assert!(!w.map("layout").contains("queue"));
    assert!(
        w.message().contains("rejected 1, see runs/"),
        "{}",
        w.message()
    );
    assert!(w.report().contains("fake reject"), "{}", w.report());
}

#[test]
fn a_revise_goes_back_with_its_instruction() {
    let w = W::new();
    let outs = format!(
        "{}\n{}",
        props(&[add("a", "layout", "Queue")]),
        props(&[add("a", "layout", "Job queue")])
    );
    w.config(&format!(
        "{}{}",
        w.inst("repo", "col", &format!("scenario = \"seq\", out = {}", q(&outs))),
        w.inst("judge", "judge", &format!("scenario = \"seq\", out = {}", q("{\"verdicts\":[{\"key\":\"repo/a\",\"verdict\":\"revise\",\"reason\":\"vague\",\"instruction\":\"say which queue\"}]}\n{\"verdicts\":[{\"key\":\"repo/a\",\"verdict\":\"accept\",\"reason\":\"ok\"}]}"))),
    ));
    w.run_code(&[], 0);
    let v = &w.input("repo", 2)["rejections"][0]["verdicts"][0];
    assert_eq!(
        (v["verdict"].as_str(), v["instruction"].as_str()),
        (Some("revise"), Some("say which queue"))
    );
    assert!(
        w.map("layout").contains("\"id\":\"job-queue\""),
        "{}",
        w.map("layout")
    );
}

#[test]
fn a_child_of_a_rejected_parent_is_rejected_too() {
    let parent = add("p", "layout", "Queue");
    let child = json!({"key": "c", "op": "add", "parent_key": "p", "title": "Retry", "body": "", "sources": ["file:c.md"], "reason": "r"});
    let w = W::new();
    w.config(&format!(
        "{}{}",
        w.inst("repo", "col", &format!("scenario = \"seq\", out = {}", q(&format!("{}\n{}", props(&[parent, child]), props(&[]))))),
        w.inst("judge", "judge", &format!("out = {}", q(r#"{"verdicts":[{"key":"repo/c","verdict":"accept","reason":"ok"},{"key":"repo/p","verdict":"reject","reason":"unsupported"}]}"#))),
    ));
    w.run_code(&[], 0);
    assert_eq!(w.map("layout"), "");
    let rej = &w.input("repo", 2)["rejections"];
    let child = rej
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["proposal"]["key"] == "c")
        .unwrap();
    assert_eq!(child["verdicts"][0]["reason"], "parent rejected");
}

#[test]
fn the_commit_message_lists_each_change() {
    let w = W::new();
    w.seed("layout", &[API, AUTH, r#"{"id":"old","parent":"layout","title":"Old","body":"","sources":["file:old.md"],"order":3}"#]);
    let p = props(&[
        add("a", "layout", "Queue"),
        json!({"key": "b", "op": "add", "parent_key": "a", "title": "Retry", "body": "", "sources": ["file:retry.md"], "reason": "found retry"}),
        json!({"key": "c", "op": "update", "id": "api", "body": "gRPC API.", "sources": ["file:api.md"], "reason": "switched to gRPC"}),
        json!({"key": "d", "op": "remove", "id": "old", "sources": [], "evidence": "file:old.md@abc", "reason": "file deleted"}),
        json!({"key": "e", "op": "move", "id": "auth", "parent": "how-it-works", "sources": ["file:auth.md"], "reason": "a mechanism"}),
    ]);
    w.config(&w.inst("repo", "col", &format!("out = {}", q(&p))));
    w.run_code(&[], 0);
    let msg = w.message();
    let (subject, body) = msg.split_once("\n\n").unwrap();
    assert!(
        subject.starts_with("run 20") && subject.ends_with("Z: +2 ~2 -1"),
        "{subject}"
    );
    assert_eq!(
        body.trim_end(),
        "added   layout/queue                  by repo  found Queue [file:a.md]\n\
         added   layout/retry                  by repo  found retry [file:retry.md]\n\
         updated layout/api (body)             by repo  switched to gRPC [file:api.md]\n\
         moved   how-it-works/auth (from api)  by repo  a mechanism [file:auth.md]\n\
         removed layout/old                    by repo  evidence: file deleted [file:old.md@abc]"
    );
}

// ---- The cost cap and runners (spec §9, §6.8) ----

/// An add citing the kind `note`, which the `prompted` collector owns.
fn note_add() -> Value {
    json!({"key": "a", "op": "add", "parent": "layout", "title": "Queue", "body": "", "sources": ["note:queue.md"], "reason": "noted"})
}

#[test]
fn the_budget_stops_runner_calls_and_leaves_the_collector_unsettled() {
    let w = W::new();
    let out = json!({"output": {"proposals": [note_add()]}, "cost_usd": 0.6, "model": "fake-1"});
    w.config(&format!(
        "max_cost_usd = 0.5\n{}{}{}",
        w.inst("writer", "prompted", ""),
        w.inst(
            "runner",
            "runner",
            &format!("out = {}", q(&out.to_string()))
        ),
        w.inst("pjudge", "pjudge", ""),
    ));
    let o = w.run_code(&[], 5);
    assert_eq!(w.calls("runner"), 1, "the judge's call was not started");
    assert!(
        w.report().contains("dropped: budget"),
        "the judge was reached: {}",
        w.report()
    );
    assert!(!w.map("layout").contains("queue"));
    assert!(o.stderr.contains("budget"), "{}", o.stderr);
    assert!(w.report().contains("budget"), "{}", w.report());
    assert!(w.report().contains("$0.60"), "{}", w.report());
    let lock: Value = serde_json::from_str(
        &std::fs::read_to_string(w.store().join("rrecall.lock")).unwrap_or("{}".into()),
    )
    .unwrap();
    assert!(lock["collectors"].get("writer").is_none());
    let input = w.input("runner", 1);
    assert_eq!(input["prompt"], "Propose changes.\n");
    assert_eq!(input["model"], "fake-model");
    assert_eq!(input["read_only"], true);
    assert_eq!(input["input"]["round"], 1);
}

#[test]
fn a_runner_reports_the_model_that_goes_into_the_lock() {
    let w = W::new();
    let c = json!({"output": {"proposals": [note_add()]}, "cost_usd": 0.1, "model": "fake-1"});
    let j = json!({"output": {"verdicts": [{"key": "writer/a", "verdict": "accept", "reason": "ok"}]}, "cost_usd": 0.1, "model": "fake-2"});
    w.config(&format!(
        "max_cost_usd = 5.0\n{}{}{}",
        w.inst("writer", "prompted", ""),
        w.inst(
            "runner",
            "runner",
            &format!("scenario = \"seq\", out = {}", q(&format!("{c}\n{j}")))
        ),
        w.inst("pjudge", "pjudge", ""),
    ));
    w.run_code(&[], 0);
    assert!(w.map("layout").contains("queue"));
    let lock = w.lock();
    assert_eq!(lock["collectors"]["writer"]["runner"], "runner");
    assert_eq!(lock["collectors"]["writer"]["model"], "fake-1");
}

// ---- Hooks, the store lock, views, status, exits 4 (spec §6.6, §4.1, §2) ----

#[test]
fn a_denying_run_before_hook_aborts_before_collecting() {
    let w = world(&props(&[add("a", "layout", "Queue")]));
    let cfg = std::fs::read_to_string(w.store().join("rrecall.toml")).unwrap();
    w.config(&format!(
        "{}{cfg}",
        w.inst(
            "gate",
            "gate",
            &format!("out = {}", q(r#"{"deny":"store frozen"}"#))
        )
    ));
    let before = w.snapshot();
    let o = w.run_code(&[], 4);
    assert!(o.stderr.contains("store frozen"), "{}", o.stderr);
    assert_eq!(w.calls("repo"), 0);
    assert_eq!(w.snapshot(), before);
    assert_eq!(
        w.input("gate", 1)["data"],
        json!({"collectors": ["repo", "github"], "force": false})
    );
}

#[test]
fn a_denying_commit_before_hook_leaves_the_store_unchanged() {
    let w = world(&props(&[add("a", "layout", "Queue")]));
    let cfg = std::fs::read_to_string(w.store().join("rrecall.toml")).unwrap();
    w.config(&format!(
        "{cfg}{}",
        w.inst(
            "cgate",
            "cgate",
            &format!("out = {}", q(r#"{"deny":"not today"}"#))
        )
    ));
    let before = w.snapshot();
    let o = w.run_code(&[], 4);
    assert!(o.stderr.contains("not today"), "{}", o.stderr);
    assert_eq!(w.calls("repo"), 1);
    assert_eq!(w.snapshot(), before, "map, lock, HEAD and index unchanged");
    let data = &w.input("cgate", 1)["data"];
    assert_eq!(
        data["diff"][0],
        json!({"change": "added", "id": "queue", "parent": "layout", "by": "repo", "key": "a"})
    );
    assert!(
        data["message"]
            .as_str()
            .unwrap()
            .contains("added   layout/queue"),
        "{data}"
    );
}

#[test]
fn observing_hooks_get_each_event_and_their_failures_do_not_count() {
    let w = world(&props(&[add("a", "layout", "Queue")]));
    let cfg = std::fs::read_to_string(w.store().join("rrecall.toml")).unwrap();
    w.config(&format!(
        "{cfg}{}",
        w.inst("audit", "audit", "out = \"{}\"")
    ));
    w.run_code(&[], 0);
    let events: Vec<Value> = (1..=w.calls("audit"))
        .map(|n| w.input("audit", n))
        .collect();
    let names: Vec<&str> = events
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "collect.after",
            "collect.after",
            "diff.after",
            "commit.after",
            "view.render"
        ]
    );
    assert_eq!(events[0]["data"]["collector"], "repo");
    assert_eq!(events[0]["data"]["round"], 1);
    assert_eq!(events[0]["data"]["proposals"][0]["key"], "a");
    assert_eq!(events[2]["data"]["diff"][0]["id"], "queue");
    assert_eq!(events[3]["data"]["commit"].as_str().unwrap(), w.head());
    assert_eq!(events[4]["data"]["commit"].as_str().unwrap(), w.head());
    // A failing observer is reported only.
    std::fs::write(w.fp(), "v2").unwrap();
    w.config(&format!(
        "{cfg}{}",
        w.inst("audit", "audit", "scenario = \"crash\"")
    ));
    w.run_code(&[], 0);
    assert!(w.report().contains("hook `audit` failed"), "{}", w.report());
}

#[test]
fn a_second_run_waits_for_the_store_lock_or_fails_fast() {
    let w = world(&props(&[]));
    let held = rrecall::lock::acquire(&w.store(), true).unwrap().unwrap();
    let o = w.run_code(&["--no-wait"], 4);
    assert!(o.stderr.contains("lock"), "{}", o.stderr);
    let spawn = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_rrecall"))
            .args(["--project", "kiln"])
            .args(args)
            .current_dir(w.root.join("work"))
            .env("HOME", w.root.join("home"))
            .env("XDG_DATA_HOME", w.root.join("data"))
            .env("XDG_CONFIG_HOME", w.root.join("config"))
            .env_remove("RRECALL_PROJECT")
            .spawn()
            .unwrap()
    };
    let mut run = spawn(&["run"]);
    let mut note = spawn(&["note", "api", "mine"]);
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(run.try_wait().unwrap().is_none(), "the run waits");
    assert!(note.try_wait().unwrap().is_none(), "a note waits too");
    assert_eq!(w.calls("repo"), 0);
    drop(held);
    assert!(run.wait().unwrap().success());
    assert!(note.wait().unwrap().success());
    assert_eq!(w.calls("repo"), 1);
    assert!(
        w.map("layout").contains(r#""note":"mine""#),
        "the note survived the run"
    );
}

#[test]
fn a_view_failure_is_reported_and_keeps_the_exit_code() {
    let w = world(&props(&[add("a", "layout", "Queue")]));
    let cfg = std::fs::read_to_string(w.store().join("rrecall.toml")).unwrap();
    w.config(&format!(
        "{cfg}{}",
        w.inst("view", "view", "scenario = \"crash\"")
    ));
    w.run_code(&[], 0);
    assert_eq!(w.calls("view"), 1);
    assert!(w.report().contains("view `view` failed"), "{}", w.report());
    assert_eq!(w.input("view", 1)["project"], "kiln");
}

#[test]
fn status_summarizes_the_last_report() {
    let w = world(&props(&[add("a", "layout", "Queue")]));
    w.run_code(&[], 0);
    let s = w.cmd(&["status"]);
    assert_eq!(s.code, 0);
    assert!(s.stdout.starts_with("Last run: 20"), "{}", s.stdout);
    assert!(
        s.stdout.contains("## Summary") && s.stdout.contains("result: exit 0"),
        "{}",
        s.stdout
    );
    assert!(!s.stdout.contains("## Proposals"), "{}", s.stdout);
    assert!(
        w.report().contains("## Proposals") && w.report().contains("repo/a"),
        "{}",
        w.report()
    );
}

#[test]
fn a_manifest_error_aborts_with_exit_4_and_changes_nothing() {
    let w = world(&props(&[]));
    let m = w.store().join("plugins/col/rrecall-plugin.toml");
    let text = std::fs::read_to_string(&m).unwrap();
    std::fs::write(
        &m,
        text.replace(
            "kind = \"collector\"",
            "colour = \"red\"\nkind = \"collector\"",
        ),
    )
    .unwrap();
    let before = w.snapshot();
    let o = w.run_code(&[], 4);
    assert!(o.stderr.contains("colour"), "{}", o.stderr);
    assert_eq!(w.snapshot(), before);
}

#[test]
fn malformed_collector_output_names_the_plugin_and_the_item() {
    let w = world(
        r#"{"proposals":[{"key":"a","op":"add","parent":"layout","title":"T","body":"","sources":["file:x"],"reason":"r","note":"mine"}]}"#,
    );
    let o = w.run_code(&[], 5);
    assert!(o.stderr.contains("repo"), "{}", o.stderr);
    let r = w.report();
    assert!(
        r.contains("plugin `repo`") && r.contains("proposal 1") && r.contains("note"),
        "{r}"
    );
}

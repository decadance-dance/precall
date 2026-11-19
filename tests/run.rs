//! End-to-end tests of `rrecall run` (spec §4, §5) with fake plugins: one test per invariant,
//! the lock, back pressure, the cost cap, hooks, the commit message, the report and exit codes.

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn fake() -> String {
    format!(
        "{}/tests/fixtures/bin/fake-plugin",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// `PATH` with the fake `claude` and `codex` first.
fn agents_path() -> String {
    format!(
        "{}/tests/fixtures/bin:{}",
        env!("CARGO_MANIFEST_DIR"),
        std::env::var("PATH").unwrap_or_default()
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
        // The fake collectors resolve every reference: a judge may not accept a claim whose
        // only sources are unresolved (spec §6.4).
        let resolve = q(&format!(
            "{}/tests/fixtures/bin/fake-resolve",
            env!("CARGO_MANIFEST_DIR")
        ));
        let fp = q(&w.fp().to_string_lossy());
        for (id, kind, table, extra) in [
            (
                "col",
                "collector",
                format!(
                    "[collector]\nscope = {SCOPE}\nsource_kinds = [\"{{kind}}\"]\nfingerprint = [\"/bin/cat\", \"{{fp}}\"]\ncommand = [{fake}]\nresolve = [{resolve}]\n"
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
                    "[collector]\nscope = {SCOPE}\nsource_kinds = [\"{{kind}}\"]\nfingerprint = [\"/bin/cat\", \"{{fp}}\"]\nprompt = \"p.md\"\nresolve = [{resolve}]\nrunner = \"runner\"\nmodel = \"fake-model\"\n"
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
            w.plugin(id, kind, &table, &extra);
        }
        w
    }

    /// Writes the store plugin `id` with a `p.md` prompt and the fake's common params.
    fn plugin(&self, id: &str, kind: &str, table: &str, extra: &str) {
        let common = "scenario = \"ok\"\nout = \"\"\nrecord = \"\"\n";
        let d = self.store().join("plugins").join(id);
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
        self.raw_env(args, &[])
    }

    fn raw_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Out {
        let o = Command::new(env!("CARGO_BIN_EXE_rrecall"))
            .args(args)
            .envs(envs.iter().copied())
            .current_dir(self.root.join("work"))
            .env("HOME", self.root.join("home"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("PATH", agents_path())
            .env_remove("CODEX_HOME")
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
    let without_times = |mut l: Value| {
        for e in l["collectors"].as_object_mut().unwrap().values_mut() {
            e.as_object_mut().unwrap().remove("settled_at");
        }
        l
    };
    let (map, lock) = (w.map("layout"), without_times(w.lock()));
    w.run_code(&["--force"], 0);
    assert_eq!((w.calls("repo"), w.calls("github")), (3, 3));
    assert_eq!(w.map("layout"), map, "a forced run over unchanged sources");
    assert_eq!(without_times(w.lock()), lock, "only the settled times move");
    let lock = w.lock();
    assert!(
        lock["collectors"]["repo"]["hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(lock["collectors"]["repo"]["plugin"], "plugins/col");
    assert!(lock["collectors"]["repo"]["settled_at"].is_string());
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
fn the_report_keeps_accepting_verdicts_and_the_collector_gets_only_the_others() {
    let w = world(&props(&[add("a", "layout", "Queue")]));
    let cfg = std::fs::read_to_string(w.store().join("rrecall.toml")).unwrap();
    w.config(&format!(
        "max_rounds = 2\n{cfg}{}",
        w.inst("judge", "judge", "scenario = \"all\", out = \"reject\"")
    ));
    w.run_code(&[], 0);
    let report = w.report();
    assert!(report.contains("  - check accept: fake accept"), "{report}");
    assert!(report.contains("  - judge reject: fake reject"), "{report}");
    let back = &w.input("repo", 2)["rejections"][0]["verdicts"];
    assert_eq!(back.as_array().unwrap().len(), 1, "{back}");
    assert_eq!(back[0]["by"], "judge");
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
    // Each removal is named by the branch it was under before the set went.
    let msg = w.message();
    assert!(msg.contains("removed layout/api"), "{msg}");
    assert!(msg.contains("removed layout/auth"), "{msg}");
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
fn i10_a_failing_fingerprint_marks_the_collector_unsettled_until_it_collects_again() {
    let w = world(&props(&[]));
    let own = w.root.join("src/fp-repo");
    std::fs::write(&own, "v1").unwrap();
    w.config(&format!(
        "{}{}",
        w.inst(
            "repo",
            "col",
            &format!(
                "fp = {}, out = {}",
                q(&own.to_string_lossy()),
                q(&props(&[]))
            )
        ),
        w.inst(
            "github",
            "col",
            &format!("kind = \"gh-pr\", out = {}", q(&props(&[])))
        ),
    ));
    w.run_code(&[], 0);
    // The other collector is due, so the run commits; the failing one gets the mark.
    std::fs::remove_file(&own).unwrap();
    std::fs::write(w.fp(), "v2").unwrap();
    w.run_code(&[], 5);
    assert_eq!(w.lock()["collectors"]["repo"]["unsettled"], true);
    std::fs::write(&own, "v1").unwrap();
    w.run_code(&[], 0);
    assert_eq!(w.calls("repo"), 2, "collected again at the old fingerprint");
    assert!(w.lock()["collectors"]["repo"].get("unsettled").is_none());
    // Nothing else is due: the mark is still committed, and a repeat failure changes nothing.
    std::fs::remove_file(&own).unwrap();
    w.run_code(&[], 5);
    assert_eq!(w.lock()["collectors"]["repo"]["unsettled"], true);
    let before = w.snapshot();
    w.run_code(&[], 5);
    assert_eq!(w.snapshot(), before);
    std::fs::write(&own, "v1").unwrap();
    w.run_code(&[], 0);
    assert_eq!((w.calls("repo"), w.calls("github")), (3, 2));
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
    let prompt = input["prompt"].as_str().unwrap();
    assert!(prompt.starts_with(rrecall::wire::PREAMBLE), "{prompt}");
    assert!(
        prompt.contains("Propose changes.\n\n## Input\n"),
        "{prompt}"
    );
    assert_eq!(input["output_schema"], rrecall::wire::collect_schema());
    assert_eq!(input["model"], "fake-model");
    assert_eq!(input["read_only"], true);
    assert_eq!(input["input"]["round"], 1);
}

#[test]
fn a_runner_reports_the_model_that_goes_into_the_lock() {
    let w = W::new();
    // Review round 1: what the runner reports reaches the tracked lock only redacted.
    let model = ["fake-1 api_key=", "Zq3vK9mW2xR7tY5pL8nB"].concat();
    let c = json!({"output": {"proposals": [note_add()]}, "cost_usd": 0.1, "model": model});
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
    assert_eq!(
        lock["collectors"]["writer"]["model"],
        "fake-1 api_key=[redacted:high-entropy-assignment]"
    );
}

#[test]
fn a_resolve_prompt_runs_through_its_collectors_runner_and_feeds_the_judges() {
    let w = W::new();
    let fake = q(&fake());
    w.plugin(
        "rcol",
        "collector",
        &format!(
            "[collector]\nscope = {SCOPE}\nsource_kinds = [\"{{kind}}\"]\nfingerprint = [\"/bin/cat\", \"{{fp}}\"]\ncommand = [{fake}]\nresolve_prompt = \"p.md\"\nrunner = \"runner\"\n"
        ),
        &format!("fp = {}\nkind = \"note\"\n", q(&w.fp().to_string_lossy())),
    );
    let texts = json!({"output": {"texts": {"note:queue.md": "The queue."}}, "cost_usd": 0.2, "model": "fake-1"});
    w.config(&format!(
        "{}{}{}",
        w.inst(
            "notes",
            "rcol",
            &format!("out = {}", q(&props(&[note_add()])))
        ),
        w.inst(
            "runner",
            "runner",
            &format!("out = {}", q(&texts.to_string()))
        ),
        w.inst("judge", "judge", "scenario = \"all\", out = \"accept\""),
    ));
    w.run_code(&[], 0);
    assert_eq!(
        w.input("judge", 1)["proposals"][0]["sources"],
        json!({"note:queue.md": "The queue."})
    );
    let input = w.input("runner", 1);
    assert!(
        input["prompt"]
            .as_str()
            .unwrap()
            .contains("\"note:queue.md\"")
    );
    assert_eq!(input["input"], json!({"refs": ["note:queue.md"]}));
    assert_eq!(input["output_schema"], rrecall::wire::resolve_schema());
    let runs = w.store().join("runs");
    let run = std::fs::read_dir(&runs)
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    assert!(run.join("runner.notes.resolve.stdout").is_file());
    // The resolve was this collector's only model call: its runner and model go into the lock.
    let lock = w.lock();
    assert_eq!(lock["collectors"]["notes"]["runner"], "runner");
    assert_eq!(lock["collectors"]["notes"]["model"], "fake-1");
    assert!(w.report().contains("$0.20"), "{}", w.report());
}

#[test]
fn a_resolve_refused_by_the_budget_drops_its_proposals() {
    let w = W::new();
    let fake = q(&fake());
    w.plugin(
        "rcol",
        "collector",
        &format!(
            "[collector]\nscope = {SCOPE}\nsource_kinds = [\"{{kind}}\"]\nfingerprint = [\"/bin/cat\", \"{{fp}}\"]\ncommand = [{fake}]\nresolve_prompt = \"p.md\"\nrunner = \"runner\"\n"
        ),
        &format!("fp = {}\nkind = \"memo\"\n", q(&w.fp().to_string_lossy())),
    );
    let memo = json!({"key": "m", "op": "add", "parent": "layout", "title": "Memo", "body": "", "sources": ["memo:m.md"], "reason": "memo"});
    let out = json!({"output": {"proposals": [note_add()]}, "cost_usd": 0.6, "model": "fake-1"});
    w.config(&format!(
        "max_cost_usd = 0.5\n{}{}{}{}",
        w.inst("writer", "prompted", ""),
        w.inst("notes", "rcol", &format!("out = {}", q(&props(&[memo])))),
        w.inst(
            "runner",
            "runner",
            &format!("out = {}", q(&out.to_string()))
        ),
        w.inst("judge", "judge", "scenario = \"all\", out = \"accept\""),
    ));
    w.run_code(&[], 5);
    assert_eq!(w.calls("runner"), 1, "the resolve was refused");
    assert!(w.map("layout").contains("Queue") && !w.map("layout").contains("Memo"));
    assert!(w.report().contains("dropped: budget"), "{}", w.report());
    assert!(w.lock()["collectors"].get("notes").is_none());
}

#[test]
fn allowed_runners_refuses_a_config_that_would_give_a_source_to_another_runner() {
    let w = W::new();
    let runners = format!(
        "{}{}",
        w.inst("runner", "runner", ""),
        w.inst("runner2", "runner", "")
    );
    for (cfg, want) in [
        (
            format!(
                "{}allowed_runners = [\"runner2\"]\n{runners}",
                w.inst("writer", "prompted", "")
            ),
            "`writer`",
        ),
        (
            format!(
                "{}allowed_runners = [\"runner\"]\n{runners}{}runner = \"runner2\"\n",
                w.inst("repo", "col", ""),
                w.inst("pjudge", "pjudge", "")
            ),
            "`pjudge`",
        ),
        (
            format!(
                "{}allowed_runners = [\"nope\"]\n{runners}",
                w.inst("repo", "col", "")
            ),
            "`nope`",
        ),
    ] {
        w.config(&cfg);
        let o = w.run_code(&[], 4);
        assert!(
            o.stderr.contains("allowed_runners") && o.stderr.contains(want),
            "{cfg}\n{}",
            o.stderr
        );
    }
}

#[test]
fn a_declarative_collector_gets_no_nodes_of_a_source_that_does_not_allow_its_runner() {
    let w = W::new();
    w.seed("layout", &[API, AUTH, NOTED, THEIRS]);
    let update = json!({"key": "u", "op": "update", "id": "api", "title": "API", "body": "HTTP API, v2.", "sources": ["file:api.md"], "reason": "v2"});
    let cite = json!({"key": "t", "op": "add_source", "id": "theirs", "sources": ["note:t.md"], "reason": "cited"});
    let cite_api = json!({"key": "s", "op": "add_source", "id": "api", "sources": ["note:api.md"], "reason": "cited"});
    let r1 = json!({"output": {"proposals": [cite_api, cite]}, "cost_usd": 0.0, "model": "fake-1"});
    let r2 = json!({"output": {"proposals": []}, "cost_usd": 0.0, "model": "fake-1"});
    w.config(&format!(
        "{}allowed_runners = [\"runner2\"]\n{}{}{}",
        w.inst("repo", "col", &format!("out = {}", q(&props(&[update])))),
        w.inst("writer", "prompted", ""),
        w.inst(
            "runner",
            "runner",
            &format!("scenario = \"seq\", out = {}", q(&format!("{r1}\n{r2}")))
        ),
        w.inst("runner2", "runner", ""),
    ));
    w.run(&[]);
    // `api`, `auth` and `noted` cite `file:`, which `repo` keeps from `runner`.
    let ids = |v: &Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(ids(&w.input("runner", 1)["input"]["nodes"]), ["theirs"]);
    assert_eq!(
        ids(&w.input("repo", 1)["nodes"]).len(),
        4,
        "a command collector sees all"
    );
    // Both changed `api` and went back; `writer` is not shown `repo`'s proposal.
    let back = |v: &Value| v["rejections"][0]["verdicts"][0].clone();
    assert!(back(&w.input("repo", 2))["conflict_with"].is_object());
    let wv = back(&w.input("runner", 2)["input"]);
    assert_eq!(wv["reason"], "conflict");
    assert!(wv.get("conflict_with").is_none(), "{wv}");
}

#[test]
fn judge_feedback_from_a_call_that_saw_a_withheld_node_does_not_reach_a_disallowed_runner() {
    let cite_api = json!({"key": "a", "op": "add_source", "id": "api", "sources": ["note:api.md"], "reason": "cited"});
    let cite = json!({"key": "t", "op": "add_source", "id": "theirs", "sources": ["note:t.md"], "reason": "cited"});
    // The verdicts `writer` gets back in round 2, by proposal key.
    let back = |proposals: &[&Value]| -> BTreeMap<String, Value> {
        let w = W::new();
        w.seed("layout", &[API, AUTH, NOTED, THEIRS]);
        let r1 = json!({"output": {"proposals": proposals}, "cost_usd": 0.0, "model": "fake-1"});
        let r2 = json!({"output": {"proposals": []}, "cost_usd": 0.0, "model": "fake-1"});
        w.config(&format!(
            "{}allowed_runners = [\"runner2\"]\n{}{}{}{}",
            w.inst("repo", "col", &format!("out = {}", q(&props(&[])))),
            w.inst("writer", "prompted", ""),
            w.inst(
                "runner",
                "runner",
                &format!("scenario = \"seq\", out = {}", q(&format!("{r1}\n{r2}")))
            ),
            w.inst("runner2", "runner", ""),
            // One command judge call sees every proposal and `api` as a `current`; its words
            // about any of them may quote `api`.
            w.inst("judge", "judge", "scenario = \"all\", out = \"revise\""),
        ));
        w.run(&[]);
        w.input("runner", 2)["input"]["rejections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["proposal"]["key"].as_str().unwrap().to_string(),
                    r["verdicts"][0].clone(),
                )
            })
            .collect()
    };
    for (key, v) in back(&[&cite_api, &cite]) {
        assert_eq!(v["verdict"], "revise", "{key}");
        assert!(
            v["reason"].as_str().unwrap().starts_with("withheld"),
            "{key}: {v}"
        );
        assert!(v.get("instruction").is_none(), "{key}: {v}");
    }
    let theirs = &back(&[&cite])["t"];
    assert_eq!(
        (theirs["reason"].as_str(), theirs["instruction"].as_str()),
        (Some("fake revise"), Some("fake instruction"))
    );
    // An add under `layout` shows the judge the nodes there (§6.5 `nodes`), `api` among them;
    // under `how-it-works`, where `repo` has no node, it shows none of `repo`'s.
    let adding = |parent: &str| json!({"key": "n", "op": "add", "parent": parent, "title": "Notes", "body": "", "sources": ["note:n.md"], "reason": "noted"});
    let v = &back(&[&adding("layout")])["n"];
    assert!(v["reason"].as_str().unwrap().starts_with("withheld"), "{v}");
    let v = &back(&[&adding("how-it-works")])["n"];
    assert_eq!(v["reason"], "fake revise", "{v}");
}

/// The fake `claude` answers with `out` (see tests/fixtures/bin/claude).
fn agent(w: &W, scenario: &str, out: &Value) {
    let d = w.root.join("home/agent");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("scenario"), scenario).unwrap();
    std::fs::write(d.join("out"), out.to_string()).unwrap();
}

#[test]
fn the_builtin_claude_runner_runs_a_declarative_collector() {
    let w = W::new();
    agent(&w, "ok", &json!({"proposals": [note_add()]}));
    w.config(&format!(
        "{}runner = \"claude\"\nmodel = \"opus-test\"\n\n[[plugins]]\nid = \"claude\"\n{}",
        w.inst("writer", "prompted", ""),
        w.inst("judge", "judge", "scenario = \"all\", out = \"accept\""),
    ));
    w.run_code(&[], 0);
    assert!(w.map("layout").contains("Queue"));
    let lock = w.lock();
    assert_eq!(lock["collectors"]["writer"]["runner"], "claude");
    assert_eq!(lock["collectors"]["writer"]["model"], "claude-fake-1");
    assert!(w.report().contains("$0.25"), "{}", w.report());
    let argv = std::fs::read_to_string(w.root.join("home/agent/claude.argv.1")).unwrap();
    assert!(argv.contains("\n--model\nopus-test\n"), "{argv}");
    let stdin = std::fs::read_to_string(w.root.join("home/agent/claude.stdin.1")).unwrap();
    assert!(stdin.starts_with(rrecall::wire::PREAMBLE) && stdin.contains("Propose changes."));
}

#[test]
fn the_cost_cap_counts_what_the_builtin_runner_reports() {
    let w = W::new();
    agent(&w, "ok", &json!({"proposals": [note_add()]}));
    w.config(&format!(
        "max_cost_usd = 0.2\n{}runner = \"claude\"\n{}runner = \"claude\"\n\n[[plugins]]\nid = \"claude\"\n",
        w.inst("writer", "prompted", ""),
        w.inst("pjudge", "pjudge", ""),
    ));
    w.run_code(&[], 5);
    let calls = std::fs::read_to_string(w.root.join("home/agent/claude.calls")).unwrap();
    assert_eq!(
        calls.lines().count(),
        1,
        "the judge's call was refused at $0.25 of $0.20"
    );
    assert!(w.report().contains("budget"), "{}", w.report());
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
fn the_commit_shows_the_net_diff_so_changes_that_cancel_out_do_not_appear() {
    let w = W::new();
    let rm = json!({"key": "b", "op": "remove", "id": "queue", "sources": [], "evidence": "file:gone.md", "reason": "gone"});
    let col = format!(
        "{}\n{}",
        props(&[add("a", "layout", "Queue"), add("b", "layout", "Maybe")]),
        props(&[rm])
    );
    let verdicts = format!(
        "{}\n{}",
        json!({"verdicts": [
            {"key": "repo/a", "verdict": "accept", "reason": "fine"},
            {"key": "repo/b", "verdict": "reject", "reason": "not a fact"}]}),
        json!({"verdicts": [{"key": "repo/b", "verdict": "accept", "reason": "fine"}]})
    );
    w.config(&format!(
        "{}{}{}",
        w.inst(
            "repo",
            "col",
            &format!("scenario = \"seq\", out = {}", q(&col))
        ),
        w.inst(
            "check",
            "check",
            &format!("scenario = \"seq\", out = {}", q(&verdicts))
        ),
        w.inst("cgate", "cgate", &format!("out = {}", q("{}"))),
    ));
    w.run_code(&[], 0);
    assert_eq!(w.calls("repo"), 2, "the rejected key went back");
    assert_eq!(w.map("layout"), "", "added, then removed");
    let data = &w.input("cgate", 1)["data"];
    assert_eq!(data["diff"], json!([]), "{data}");
    let msg = w.message();
    assert!(
        msg.lines().next().unwrap().ends_with("no map changes"),
        "{msg}"
    );
    assert!(!msg.contains("queue"), "{msg}");
    // The report still shows each round's effects.
    assert!(w.report().contains("queue"), "{}", w.report());
}

#[test]
fn a_node_moved_away_and_back_shows_its_new_order_as_an_update() {
    let w = W::new();
    w.seed("layout", &[API, NOTED, THEIRS]);
    let mv = |key: &str, to: &str| json!({"key": key, "op": "move", "id": "api", "parent": to, "sources": ["file:api.md"], "reason": "moved"});
    let col = format!(
        "{}\n{}",
        props(&[mv("a", "decisions"), add("b", "layout", "Maybe")]),
        props(&[mv("b", "layout")])
    );
    let verdicts = format!(
        "{}\n{}",
        json!({"verdicts": [
            {"key": "repo/a", "verdict": "accept", "reason": "fine"},
            {"key": "repo/b", "verdict": "reject", "reason": "not a fact"}]}),
        json!({"verdicts": [{"key": "repo/b", "verdict": "accept", "reason": "fine"}]})
    );
    w.config(&format!(
        "{}{}{}",
        w.inst(
            "repo",
            "col",
            &format!("scenario = \"seq\", out = {}", q(&col))
        ),
        w.inst(
            "check",
            "check",
            &format!("scenario = \"seq\", out = {}", q(&verdicts))
        ),
        w.inst("cgate", "cgate", &format!("out = {}", q("{}"))),
    ));
    w.run_code(&[], 0);
    let shown = w.git(&["show", "--format=", "HEAD", "--", "map/layout.jsonl"]);
    assert!(
        shown.contains(r#"-{"id":"api""#),
        "the order changed: {shown}"
    );
    let diff = &w.input("cgate", 1)["data"]["diff"];
    assert_eq!(
        diff,
        &json!([{"change": "updated", "id": "api", "parent": "layout", "fields": ["order"], "by": "repo", "key": "b"}])
    );
    let msg = w.message();
    assert!(msg.lines().next().unwrap().ends_with("+0 ~1 -0"), "{msg}");
    assert!(msg.contains("updated layout/api (order)"), "{msg}");
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
fn a_lock_checked_out_with_crlf_is_not_rewritten_and_no_commit_is_reported() {
    let w = world(&props(&[]));
    let cfg = std::fs::read_to_string(w.store().join("rrecall.toml")).unwrap();
    w.config(&format!(
        "{cfg}{}",
        w.inst("audit", "audit", "out = \"{}\"")
    ));
    w.git(&["config", "core.autocrlf", "true"]);
    w.run_code(&[], 0);
    // A collector that keeps crashing leaves its unsettled entry, so the lock renders the same.
    let crashing = format!(
        "{}{}",
        w.inst("repo", "col", "scenario = \"crash\""),
        w.inst("audit", "audit", "out = \"{}\"")
    );
    w.config(&crashing);
    w.run_code(&[], 5);
    std::fs::remove_file(w.store().join("rrecall.lock")).unwrap();
    w.git(&["checkout", "--", "rrecall.lock"]);
    let lock = std::fs::read_to_string(w.store().join("rrecall.lock")).unwrap();
    assert!(lock.contains("\r\n"), "git checked the lock out with CRLF");
    let (head, calls) = (w.head(), w.calls("audit"));
    let o = w.run_code(&[], 5);
    assert_eq!(w.head(), head);
    assert_eq!(
        std::fs::read_to_string(w.store().join("rrecall.lock")).unwrap(),
        lock,
        "the checkout is left alone"
    );
    let events: Vec<Value> = (calls + 1..=w.calls("audit"))
        .map(|n| w.input("audit", n)["event"].clone())
        .collect();
    assert!(!events.contains(&json!("commit.after")), "{events:?}");
    assert!(!o.stdout.contains(&head[..7]), "{}", o.stdout);
    // A lock change git's own attributes clean away: nothing lands, so nothing is reported.
    w.config(&format!(
        "{cfg}{}",
        w.inst("audit", "audit", "out = \"{}\"")
    ));
    w.git(&["config", "filter.blank.clean", "cat >/dev/null"]);
    std::fs::write(
        w.store().join(".gitattributes"),
        "rrecall.lock filter=blank\n",
    )
    .unwrap();
    w.commit("attributes", &[".gitattributes"]);
    w.git(&["add", "--renormalize", "rrecall.lock"]);
    w.git(&["commit", "-q", "-m", "blank lock"]);
    let (head, calls) = (w.head(), w.calls("audit"));
    std::fs::write(w.fp(), "v2").unwrap();
    let o = w.run_code(&[], 0);
    assert_ne!(w.lock()["collectors"]["repo"]["fingerprint"], json!(null));
    assert_eq!(w.head(), head, "git found nothing to commit");
    let events: Vec<Value> = (calls + 1..=w.calls("audit"))
        .map(|n| w.input("audit", n)["event"].clone())
        .collect();
    assert!(!events.contains(&json!("commit.after")), "{events:?}");
    assert!(o.stdout.contains("commit: none"), "{}", o.stdout);
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
    // With no proposals the warnings follow the summary; `run` and `status` still stop there.
    let w = world(&props(&[]));
    let cfg = std::fs::read_to_string(w.store().join("rrecall.toml")).unwrap();
    w.config(&format!(
        "{cfg}{}",
        w.inst("view", "view", "scenario = \"crash\"")
    ));
    let o = w.run_code(&[], 0);
    assert!(!w.report().contains("## Proposals"), "{}", w.report());
    assert!(w.report().contains("## Warnings"), "{}", w.report());
    assert!(!o.stdout.contains("## Warnings"), "{}", o.stdout);
    let o = w.cmd(&["status"]);
    assert!(
        o.stdout.contains("## Summary") || o.stdout.contains("- result"),
        "{}",
        o.stdout
    );
    assert!(!o.stdout.contains("## Warnings"), "{}", o.stdout);
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

// ---- Credentials a plugin prints (spec §6.3) ----

#[test]
fn a_credential_a_plugin_prints_stays_out_of_the_run_dir_and_the_commit_message() {
    // Built from parts, so this file holds none whole.
    let tok = ["gh", "p_", "Zq3vK9mW2xR7tY5pL8nB4cD6fG1hJ0kAa2Ss"].concat();
    let pw = ["Xk9mQ2vR7t", "Y5pL8nB4cD"].concat();
    let mut leaky = add(&tok, "layout", "Queue");
    leaky["body"] = json!(format!("DB_PASSWORD={pw}"));
    let w = W::new();
    w.seed("layout", &[API]);
    w.config(&format!(
        "{}{}\n[[plugins]]\nid = \"secrets\"\n{}",
        w.inst(
            "repo",
            "col",
            &format!(
                "scenario = \"leak\", out = {}",
                q(&props(&[leaky, add("b", "layout", "Cache")]))
            )
        ),
        w.inst(
            "github",
            "col",
            &format!(
                "kind = \"gh-pr\", scenario = \"leak-crash\", out = {}",
                q(&format!("login failed with {tok}"))
            )
        ),
        w.inst("check", "check", "scenario = \"all\", out = \"accept\""),
    ));
    w.run_code(&[], 5);
    assert!(
        w.map("layout").contains("Cache"),
        "the clean proposal lands"
    );
    let report = w.report();
    assert!(
        report.contains("secrets reject") && report.contains("[redacted:github-token]"),
        "{report}"
    );
    let runs = w.store().join("runs");
    let run = std::fs::read_dir(&runs)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut kept = vec![("commit message".to_string(), w.message())];
    for f in std::fs::read_dir(&run).unwrap() {
        let f = f.unwrap().path();
        kept.push((
            f.display().to_string(),
            std::fs::read_to_string(&f).unwrap(),
        ));
    }
    let collected = run.join("repo.collect.stdout");
    assert!(
        std::fs::read_to_string(&collected)
            .unwrap()
            .contains("DB_PASSWORD=[redacted:high-entropy-assignment]"),
        "the log is kept, redacted"
    );
    assert!(kept.len() > 8, "{kept:?}");
    for (name, text) in kept {
        assert!(
            !text.contains(&tok) && !text.contains(&pw),
            "{name} holds a credential"
        );
    }
}

/// Review round 1: without the `secrets` check, a credential still reaches no tracked map file.
#[test]
fn without_the_secrets_check_the_map_gets_a_credential_redacted() {
    let pw = ["Xk9mQ2vR7t", "Y5pL8nB4cD"].concat();
    let mut a = add("a", "layout", "Queue");
    a["body"] = json!(format!("DB_PASSWORD={pw}"));
    let w = world(&props(&[a]));
    w.run_code(&[], 0);
    let map = w.map("layout");
    assert!(
        map.contains("DB_PASSWORD=[redacted:high-entropy-assignment]") && !map.contains(&pw),
        "{map}"
    );
}

/// Review round 1: a verdict reason is redacted whole before the report shortens it, so the
/// cut cannot leave a credential too short to be found.
#[test]
fn a_reason_is_redacted_before_the_report_shortens_it() {
    let tok = ["gh", "p_", "Zq3vK9mW2xR7tY5pL8nB4cD6fG1hJ0kAa2Ss"].concat();
    let reason = format!("{} {tok}", "x".repeat(1980));
    let verdicts = json!({"verdicts": [{"key": "repo/a", "verdict": "accept", "reason": reason}]});
    let w = world(&props(&[add("a", "layout", "Queue")]));
    w.config(&format!(
        "{}{}",
        w.inst(
            "repo",
            "col",
            &format!("out = {}", q(&props(&[add("a", "layout", "Queue")])))
        ),
        w.inst(
            "check",
            "check",
            &format!("out = {}", q(&verdicts.to_string()))
        ),
    ));
    w.run_code(&[], 0);
    let report = w.report();
    assert!(
        report.contains(" [redacted:") && !report.contains(&tok[..12]),
        "{report}"
    );
}

/// Review round 2: a proposal is validated on its own values before any redaction, so one
/// that breaks a limit is rejected, not stored with a marker in place of what broke it.
#[test]
fn a_proposal_is_validated_before_its_fields_are_redacted() {
    let mut a = add("a", "layout", "Queue");
    a["title"] = json!(
        [
            "-----BEGIN RSA ",
            "PRIVATE KEY-----\n-----END RSA PRIVATE KEY-----"
        ]
        .concat()
    );
    let w = world(&props(&[a]));
    w.run(&[]);
    assert!(!w.map("layout").contains("redacted"), "{}", w.map("layout"));
    assert!(w.report().contains("rejected"), "{}", w.report());
}

/// The built-in `slack` collector (spec §10) configured as the README shows: on a `claude`
/// instance with `connectors`, which the fake `claude` answers for the collect call, then the
/// resolve call. Invented project, invented messages.
#[test]
fn the_builtin_slack_collector_reads_through_a_connector_runner_once_a_day() {
    let w = W::new();
    let r = "slack:C0123ABCDEF/1795033170.000100";
    let add = json!({"key": "pg", "op": "add", "parent": "decisions", "title": "Ledger on Postgres",
        "body": "The ledger moves to Postgres for its row locks.", "sources": [r], "date": "2026-11-02",
        "reason": "decided in a channel"});
    agent(&w, "ok", &json!({"proposals": [add]}));
    let texts = json!({"texts": {r: "Ana: we go with Postgres for the ledger, it has row locks"}});
    std::fs::write(w.root.join("home/agent/out.2"), texts.to_string()).unwrap();
    let readme = include_str!("../README.md");
    let snippet = readme
        .split("```toml\n")
        .nth(1)
        .and_then(|s| s.split("```").next())
        .unwrap();
    let judge = w.inst("judge", "judge", "scenario = \"all\", out = \"accept\"");
    // The `claude` instance init enables, then the snippet.
    let plain = "[[plugins]]\nid = \"claude\"\n\n";
    w.config(&format!("{plain}{snippet}{judge}"));
    w.run_code(&[], 0);
    assert!(w.map("decisions").contains(r), "{}", w.map("decisions"));
    let agent_rec =
        |name: &str| std::fs::read_to_string(w.root.join("home/agent").join(name)).unwrap();
    let argv = agent_rec("claude.argv.1");
    assert!(
        argv.contains("\n--setting-sources\nuser\n") && !argv.contains("--safe-mode"),
        "{argv}"
    );
    assert!(
        argv.contains(",mcp__claude_ai_Slack__slack_read_channel,"),
        "{argv}"
    );
    let collect = agent_rec("claude.stdin.1");
    assert!(collect.contains("# Collector: slack") && collect.contains("eng-platform"));
    assert!(collect.contains("\"lookback_days\": 14"), "{collect}");
    let resolve = agent_rec("claude.stdin.2");
    assert!(resolve.contains("# Resolve: slack") && resolve.contains(r));
    assert!(w.input("judge", 1).to_string().contains("it has row locks"));
    assert_eq!(w.lock()["collectors"]["slack"]["runner"], "claude-slack");
    // Due once a day: a second run calls no agent.
    w.run_code(&[], 0);
    assert_eq!(agent_rec("claude.calls").lines().count(), 2);
    // Slack text may not reach a runner its list leaves out.
    let only_plain = snippet.replace(
        "allowed_runners = [\"claude-slack\", \"claude\"]",
        "allowed_runners = [\"claude\"]",
    );
    assert_ne!(only_plain, snippet);
    w.config(&format!("{plain}{only_plain}{judge}"));
    w.run_code(&[], 4);
}

#[test]
fn a_relative_claude_config_dir_is_resolved_against_the_dir_rrecall_runs_in() {
    let w = W::new();
    agent(&w, "ok", &json!({"proposals": []}));
    let readme = include_str!("../README.md");
    let snippet = readme
        .split("```toml\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    w.config(&format!("[[plugins]]\nid = \"claude\"\n\n{snippet}"));
    let o = w.raw_env(
        &["--project", "kiln", "run", "--force"],
        &[("CLAUDE_CONFIG_DIR", "login")],
    );
    assert_eq!(o.code, 0, "{}", o.stderr);
    // Plugins run in their own dir; the login is the one next to where rrecall was started.
    let env = std::fs::read_to_string(w.root.join("home/agent/claude.env.1")).unwrap();
    let want = format!(
        "CLAUDE_SECURESTORAGE_CONFIG_DIR={}",
        std::fs::canonicalize(w.root.join("work"))
            .unwrap()
            .join("login")
            .display()
    );
    assert!(env.lines().any(|l| l == want), "{env}");
}

// ---- Built-in judges (spec §10, §6.5) ----

/// `world` with `repo` adding `a`, the built-in `claude` runner and built-in judge `judge`,
/// whose fake agent answers `verdicts`.
fn judged(judge: &str, verdicts: Value) -> W {
    let w = world(&props(&[add("a", "layout", "Queue")]));
    agent(&w, "ok", &verdicts);
    let cfg = std::fs::read_to_string(w.store().join("rrecall.toml")).unwrap();
    w.config(&format!(
        "{cfg}\n[[plugins]]\nid = \"claude\"\n\n[[plugins]]\nid = \"{judge}\"\nrunner = \"claude\"\n"
    ));
    w
}

fn verdict(key: &str, v: &str, reason: &str) -> Value {
    let mut j = json!({"key": key, "verdict": v, "reason": reason});
    if v == "revise" {
        j["instruction"] = json!("cite the queue's design doc");
    }
    json!({"verdicts": [j]})
}

#[test]
fn a_builtin_judge_runs_its_embedded_prompt_through_the_runner_and_its_reject_holds() {
    for judge in ["support", "removal", "duplicate"] {
        let w = judged(judge, verdict("repo/a", "reject", "no source backs it"));
        w.run_code(&[], 0);
        assert!(!w.map("layout").contains("Queue"), "{judge}");
        assert!(w.report().contains("no source backs it"), "{judge}");
        let stdin = std::fs::read_to_string(w.root.join("home/agent/claude.stdin.1")).unwrap();
        let prompt = rrecall::plugin::builtin_prompt(judge, &format!("{judge}.md")).unwrap();
        assert!(
            stdin.starts_with(rrecall::wire::PREAMBLE) && stdin.contains(prompt.trim_end()),
            "{judge}: {stdin}"
        );
        assert!(stdin.contains("\"repo/a\""), "{judge}");
        let lock = w.lock();
        assert_eq!(lock["plugins"][judge]["plugin"], "builtin", "{judge}");
    }
}

#[test]
fn a_builtin_judge_revise_goes_back_to_the_collector_with_its_instruction() {
    let w = judged("support", verdict("repo/a", "revise", "vague"));
    w.run_code(&[], 0);
    let back = &w.input("repo", 2)["rejections"][0]["verdicts"][0];
    assert_eq!(back["by"], "support");
    assert_eq!(back["instruction"], "cite the queue's design doc");
}

#[test]
fn a_builtin_judge_that_omits_a_verdict_accepts_nothing() {
    let w = judged("duplicate", json!({"verdicts": []}));
    let o = w.run_code(&[], 5);
    assert!(!w.map("layout").contains("Queue"));
    assert!(o.stderr.contains("duplicate"), "{}", o.stderr);
    assert!(w.lock()["collectors"].get("repo").is_none(), "unsettled");
}

const SYNC: &str = r#"{"id":"sync","parent":"how-it-works","title":"Sync","body":"Nightly.","sources":["file:sync.md"],"order":0}"#;

#[test]
fn checks_and_judges_get_the_nodes_of_the_branches_their_batch_adds_to() {
    let w = world(&props(&[add("a", "layout", "Queue")]));
    w.seed("how-it-works", &[SYNC]);
    let cfg = std::fs::read_to_string(w.store().join("rrecall.toml")).unwrap();
    w.config(&format!(
        "{cfg}{}",
        w.inst("judge", "judge", "scenario = \"all\", out = \"accept\"")
    ));
    w.run_code(&[], 0);
    for inst in ["check", "judge"] {
        let input = w.input(inst, 1);
        let ids: Vec<&str> = input["nodes"]
            .as_array()
            .unwrap_or_else(|| panic!("{inst}: {input}"))
            .iter()
            .map(|n| n["id"].as_str().unwrap())
            .collect();
        // Not `sync` (another branch), not the branch `layout` itself, not the new node.
        assert_eq!(ids, ["api", "auth", "noted", "theirs"], "{inst}");
        assert_eq!(input["nodes"][0]["title"], "API", "{inst}");
    }
}

/// `world` whose `repo` also resolves: it answers `out` to collect and `texts` to resolve.
fn resolving(out: &str, texts: Value) -> W {
    let w = world("");
    let fake = q(&fake());
    w.plugin(
        "rcol",
        "collector",
        &format!(
            "[collector]\nscope = {SCOPE}\nsource_kinds = [\"file\"]\nfingerprint = [\"/bin/cat\", \"{{fp}}\"]\ncommand = [{fake}]\nresolve = [{fake}]\n"
        ),
        &format!("fp = {}\n", q(&w.fp().to_string_lossy())),
    );
    let lines = format!("{out}\n{}", json!({ "texts": texts }));
    w.config(&format!(
        "{}{}",
        w.inst(
            "repo",
            "rcol",
            &format!("scenario = \"seq\", out = {}", q(&lines))
        ),
        w.inst("check", "check", "scenario = \"all\", out = \"accept\""),
    ));
    w
}

#[test]
fn a_judge_may_not_accept_a_claim_whose_only_sources_are_unresolved() {
    for (texts, accepted) in [
        (json!({"file:a.md": null}), false),
        (json!({}), false),
        (json!({"file:a.md": "The queue."}), true),
    ] {
        let w = resolving(&props(&[add("a", "layout", "Queue")]), texts.clone());
        let cfg = std::fs::read_to_string(w.store().join("rrecall.toml")).unwrap();
        // One round: the fake's collect and resolve answers would run out in a second.
        w.config(&format!(
            "max_rounds = 1\n{cfg}{}",
            w.inst("judge", "judge", "scenario = \"all\", out = \"accept\"")
        ));
        w.run_code(&[], 0);
        assert_eq!(w.map("layout").contains("Queue"), accepted, "{texts}");
        assert_eq!(
            w.report().contains("only unresolved sources"),
            !accepted,
            "{texts}: {}",
            w.report()
        );
    }
}

#[test]
fn a_builtin_judge_accept_with_resolved_sources_enters_the_map() {
    let w = resolving(
        &props(&[add("a", "layout", "Queue")]),
        json!({"file:a.md": "The queue."}),
    );
    agent(&w, "ok", &verdict("repo/a", "accept", "the source says so"));
    let cfg = std::fs::read_to_string(w.store().join("rrecall.toml")).unwrap();
    w.config(&format!(
        "{cfg}\n[[plugins]]\nid = \"claude\"\n\n[[plugins]]\nid = \"support\"\nrunner = \"claude\"\n"
    ));
    w.run_code(&[], 0);
    assert!(w.map("layout").contains("Queue"));
    let stdin = std::fs::read_to_string(w.root.join("home/agent/claude.stdin.1")).unwrap();
    assert!(
        stdin.contains("The queue."),
        "the resolved text reaches the agent"
    );
}

const PARENT: &str = r#"{"id":"parent","parent":"layout","title":"Parent","body":"","sources":["file:p.md"],"order":0}"#;
const QUEUE: &str = r#"{"id":"queue","parent":"layout","title":"Queue","body":"","sources":["file:q.md"],"order":1}"#;

/// `repo` moves `parent` to `how-it-works` and adds a second Queue under it; `check` answers
/// `check_out` (verdicts for `repo/m` and `repo/q`), `judge` runs with `judge_params`.
fn moving(check_out: &str, judge_params: &str) -> W {
    let w = W::new();
    w.seed("layout", &[PARENT, QUEUE]);
    let m = json!({"key": "m", "op": "move", "id": "parent", "parent": "how-it-works", "sources": ["file:p.md"], "reason": "moved"});
    let a = json!({"key": "q", "op": "add", "parent": "parent", "title": "Queue", "body": "", "sources": ["file:q2.md"], "reason": "found"});
    w.config(&format!(
        "max_rounds = 1\n{}{}{}{}",
        w.inst("repo", "col", &format!("out = {}", q(&props(&[m, a])))),
        w.inst("check", "check", &format!("out = {}", q(check_out))),
        w.inst("judge", "judge", judge_params),
        w.inst("dup", "judge", "scenario = \"all\", out = \"accept\""),
    ));
    w
}

fn verdicts(m: &str, q: &str) -> String {
    json!({"verdicts": [
        {"key": "repo/m", "verdict": m, "reason": format!("move {m}")},
        {"key": "repo/q", "verdict": q, "reason": format!("add {q}")},
    ]})
    .to_string()
}

fn node_ids(input: &Value) -> Vec<String> {
    input["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_judge_sees_the_branch_an_add_lands_in_without_a_move_a_check_rejected() {
    let w = moving(
        &verdicts("reject", "accept"),
        "scenario = \"all\", out = \"accept\"",
    );
    w.run_code(&[], 0);
    // The judges get only the add; it lands in `layout`, next to the Queue it may restate.
    let input = w.input("dup", 1);
    assert_eq!(input["proposals"].as_array().unwrap().len(), 1, "{input}");
    assert_eq!(node_ids(&input), ["parent", "queue"]);
}

#[test]
fn an_add_whose_branch_changed_after_it_was_judged_goes_back() {
    // Both judges see the move and the add in `how-it-works`; `judge` then rejects the move,
    // which would leave the add in `layout`, where no judge compared it with the nodes there.
    let w = moving(
        &verdicts("accept", "accept"),
        &format!("out = {}", q(&verdicts("reject", "accept"))),
    );
    w.run_code(&[], 0);
    assert!(node_ids(&w.input("dup", 1)).is_empty());
    assert!(!w.map("layout").contains("queue-2"), "{}", w.map("layout"));
    assert!(w.report().contains("judged under"), "{}", w.report());
}

#[test]
fn an_add_a_judge_saw_without_a_destination_does_not_commit() {
    let w = W::new();
    let anchor = r#"{"id":"anchor","parent":"how-it-works","title":"Anchor","body":"","sources":["file:a.md"],"order":0}"#;
    w.seed("layout", &[PARENT, QUEUE]);
    w.seed("how-it-works", &[anchor]);
    let fake = q(&fake());
    w.plugin(
        "lcol",
        "collector",
        &format!(
            "[collector]\nscope = [\"layout\"]\nsource_kinds = [\"{{kind}}\"]\nfingerprint = [\"/bin/cat\", \"{{fp}}\"]\ncommand = [{fake}]\nresolve = [{}]\n",
            q(&format!(
                "{}/tests/fixtures/bin/fake-resolve",
                env!("CARGO_MANIFEST_DIR")
            ))
        ),
        &format!(
            "fp = {}\nkind = \"gh-pr\"\n",
            q(&w.fp().to_string_lossy())
        ),
    );
    let mp = json!({"key": "mp", "op": "move", "id": "parent", "parent": "anchor", "sources": ["file:p.md"], "reason": "moved"});
    let ma = json!({"key": "ma", "op": "move", "id": "anchor", "parent": "layout", "sources": ["file:a.md"], "reason": "moved"});
    let a = json!({"key": "q", "op": "add", "parent": "parent", "title": "Queue", "body": "", "sources": ["gh-pr:o/r#9"], "reason": "found"});
    let say = |vs: &[(&str, &str)]| {
        json!({"verdicts": vs.iter().map(|(k, v)| json!({"key": k, "verdict": v, "reason": v})).collect::<Vec<_>>()}).to_string()
    };
    w.config(&format!(
        "max_rounds = 1\n{}{}{}{}{}",
        w.inst("repo", "col", &format!("out = {}", q(&props(&[mp, ma])))),
        w.inst("lay", "lcol", &format!("out = {}", q(&props(&[a])))),
        w.inst(
            "check",
            "check",
            &format!(
                "out = {}",
                q(&say(&[
                    ("repo/mp", "accept"),
                    ("repo/ma", "reject"),
                    ("lay/q", "accept")
                ]))
            )
        ),
        w.inst(
            "judge",
            "judge",
            &format!(
                "out = {}",
                q(&say(&[("repo/mp", "reject"), ("lay/q", "accept")]))
            )
        ),
        w.inst("dup", "judge", "scenario = \"all\", out = \"accept\""),
    ));
    w.run_code(&[], 0);
    // Without the anchor move, the parent move takes the add out of `lay`'s scope, so no judge
    // compared it with `layout`, where it would land once the parent move is rejected.
    assert_eq!(node_ids(&w.input("dup", 1)), ["anchor"]);
    assert!(!w.map("layout").contains("queue-2"), "{}", w.map("layout"));
}

/// A store plugin `fetching`: the `prompted` collector plus a `fetch` argv (spec §6.1).
fn fetching(w: &W) {
    let fake = q(&fake());
    let fp = q(&w.fp().to_string_lossy());
    w.plugin(
        "fetching",
        "collector",
        &format!(
            "[collector]\nscope = {SCOPE}\nsource_kinds = [\"note\"]\nfingerprint = [\"/bin/cat\", \"{{fp}}\"]\nfetch = [{fake}]\nprompt = \"p.md\"\nrunner = \"runner\"\n"
        ),
        &format!("fp = {fp}\n"),
    );
}

#[test]
fn a_fetch_output_reaches_every_collect_input_of_its_instance_once_per_run() {
    let w = W::new();
    fetching(&w);
    let add = add("a", "layout", "Kiln");
    let r = json!({"output": {"proposals": [add]}, "cost_usd": 0.0, "model": "fake-1"});
    w.config(&format!(
        "max_rounds = 2\n{}{}{}",
        w.inst("writer", "fetching", "out = '{\"pulls\": [7]}'"),
        w.inst("runner", "runner", &format!("out = {}", q(&r.to_string()))),
        w.inst("check", "check", "scenario = \"all\", out = \"revise\""),
    ));
    w.run(&[]);
    // The fetch got the plugin input on stdin and ran once, though the collector ran twice.
    assert_eq!(w.calls("writer"), 1);
    assert_eq!(w.input("writer", 1)["params"]["out"], "{\"pulls\": [7]}");
    assert!(
        std::fs::read_to_string(w.rec("writer"))
            .unwrap()
            .contains("argv:")
    );
    assert_eq!(w.calls("runner"), 2);
    for n in [1, 2] {
        assert_eq!(
            w.input("runner", n)["input"]["fetched"],
            json!({"pulls": [7]}),
            "call {n}"
        );
    }
    let runs = w.store().join("runs");
    let run = std::fs::read_dir(&runs)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(run.join("writer.fetch.stdout").exists());
}

#[test]
fn a_failing_or_malformed_fetch_makes_its_collector_unreachable() {
    for (scenario, out) in [
        ("crash", ""),
        ("ok", "{\"a\": 1, \"a\": 2}"),
        ("ok", "not json"),
    ] {
        let w = W::new();
        fetching(&w);
        w.config(&format!(
            "{}{}",
            w.inst(
                "writer",
                "fetching",
                &format!("scenario = {}, out = {}", q(scenario), q(out))
            ),
            w.inst("runner", "runner", ""),
        ));
        let o = w.run_code(&[], 5);
        assert!(
            o.stderr.contains("writer"),
            "{scenario} {out}: {}",
            o.stderr
        );
        assert_eq!(w.calls("runner"), 0, "{scenario} {out}: no collect call");
        assert!(
            w.lock()["collectors"].get("writer").is_none(),
            "not settled"
        );
    }
}

#[test]
fn a_collector_without_fetch_gets_no_fetched_key() {
    let w = W::new();
    let r = json!({"output": {"proposals": []}, "cost_usd": 0.0, "model": "fake-1"});
    w.config(&format!(
        "{}{}",
        w.inst("writer", "prompted", ""),
        w.inst("runner", "runner", &format!("out = {}", q(&r.to_string()))),
    ));
    w.run_code(&[], 0);
    assert!(w.input("runner", 1)["input"].get("fetched").is_none());
}

// ---- Incremental collection (spec §4.2, §6.4) ----

/// `world` with `repo` answering line N of `outs` on its Nth call.
fn seq_world(outs: &[String]) -> W {
    let w = world(&props(&[]));
    w.config(&format!(
        "{}{}",
        w.inst(
            "repo",
            "col",
            &format!("scenario = \"seq\", out = {}", q(&outs.join("\n")))
        ),
        w.inst("check", "check", "scenario = \"all\", out = \"accept\""),
    ));
    w
}

fn with_cursor(items: &[Value], cursor: Value) -> String {
    json!({ "proposals": items, "cursor": cursor }).to_string()
}

#[test]
fn the_cursor_of_a_settled_run_reaches_the_next_input_as_since() {
    let w = seq_world(&[
        with_cursor(&[], json!("c1")),
        with_cursor(&[], json!({"n": 2})),
    ]);
    w.run_code(&[], 0);
    assert_eq!(w.input("repo", 1)["since"], Value::Null, "first run: full read");
    let lock = w.lock();
    let entry = &lock["collectors"]["repo"];
    assert_eq!(entry["cursor"], "c1");
    assert!(entry["setup"].as_str().unwrap().starts_with("sha256:"));
    let at = entry["settled_at"].as_str().unwrap().to_string();
    std::fs::write(w.fp(), "v2").unwrap();
    w.run_code(&[], 0);
    assert_eq!(w.input("repo", 2)["since"], json!({"at": at, "cursor": "c1"}));
    assert_eq!(w.lock()["collectors"]["repo"]["cursor"], json!({"n": 2}));
}

#[test]
fn a_settled_run_without_a_cursor_clears_it_and_keeps_the_time() {
    let w = seq_world(&[with_cursor(&[], json!("c1")), props(&[]), props(&[])]);
    w.run_code(&[], 0);
    std::fs::write(w.fp(), "v2").unwrap();
    w.run_code(&[], 0);
    let lock = w.lock();
    assert!(lock["collectors"]["repo"].get("cursor").is_none(), "{lock}");
    let at = lock["collectors"]["repo"]["settled_at"].clone();
    std::fs::write(w.fp(), "v3").unwrap();
    w.run_code(&[], 0);
    assert_eq!(w.input("repo", 3)["since"], json!({"at": at, "cursor": null}));
}

#[test]
fn an_unsettled_run_does_not_advance_the_cursor() {
    let w = seq_world(&[
        with_cursor(&[], json!("c1")),
        with_cursor(&[add("a", "layout", "Queue")], json!("c2")),
    ]);
    w.run_code(&[], 0);
    let settled = w.lock()["collectors"]["repo"].clone();
    std::fs::write(w.fp(), "v2").unwrap();
    let outs = [
        with_cursor(&[], json!("c1")),
        with_cursor(&[add("a", "layout", "Queue")], json!("c2")),
    ];
    w.config(&format!(
        "{}{}",
        w.inst(
            "repo",
            "col",
            &format!("scenario = \"seq\", out = {}", q(&outs.join("\n")))
        ),
        w.inst("check", "check", "scenario = \"crash\""),
    ));
    w.run_code(&[], 5);
    let entry = &w.lock()["collectors"]["repo"];
    assert_eq!(entry["unsettled"], true);
    assert_eq!(entry["cursor"], "c1", "the cursor stays at the settled run");
    assert_eq!(entry["settled_at"], settled["settled_at"]);
    w.config(&format!(
        "{}{}",
        w.inst(
            "repo",
            "col",
            &format!("scenario = \"seq\", out = {}", q(&outs.join("\n")))
        ),
        w.inst("check", "check", "scenario = \"all\", out = \"accept\""),
    ));
    w.run_code(&[], 0);
    assert_eq!(
        w.input("repo", 3)["since"],
        json!({"at": settled["settled_at"], "cursor": "c1"}),
        "the retry reads from the last settled run"
    );
    assert_eq!(w.lock()["collectors"]["repo"]["cursor"], "c2");
}

#[test]
fn a_params_change_force_or_an_old_entry_reads_everything() {
    let outs = [with_cursor(&[], json!("c1"))];
    let w = seq_world(&outs);
    w.run_code(&[], 0);
    // A params change drops the cursor.
    w.config(&format!(
        "{}{}",
        w.inst(
            "repo",
            "col",
            &format!("scenario = \"seq\", out = {}", q(&format!(" {}", outs[0])))
        ),
        w.inst("check", "check", "scenario = \"all\", out = \"accept\""),
    ));
    w.run_code(&[], 0);
    assert_eq!(w.calls("repo"), 2);
    assert_eq!(w.input("repo", 2)["since"], Value::Null, "params changed");
    // `--force` reads everything.
    w.run_code(&["--force"], 0);
    assert_eq!(w.input("repo", 3)["since"], Value::Null, "--force");
    assert_eq!(w.lock()["collectors"]["repo"]["cursor"], "c1");
    // An entry written before the lock kept the setup: its settled state is unknown.
    let mut lock = w.lock();
    lock["collectors"]["repo"]
        .as_object_mut()
        .unwrap()
        .remove("setup");
    std::fs::write(
        w.store().join("rrecall.lock"),
        format!("{}\n", serde_json::to_string_pretty(&lock).unwrap()),
    )
    .unwrap();
    w.commit("old lock", &["rrecall.lock"]);
    std::fs::write(w.fp(), "v2").unwrap();
    w.run_code(&[], 0);
    assert_eq!(w.input("repo", 4)["since"], Value::Null, "old entry");
}

#[test]
fn a_cursor_that_looks_like_a_credential_is_not_kept() {
    let token = format!("ghp_{}", "aB3dE5fG7hJ9kL1mN3pQ5rS7tU9vW1xY3z5A");
    let w = seq_world(&[with_cursor(&[], json!({ "t": token }))]);
    w.run_code(&[], 0);
    let text = std::fs::read_to_string(w.store().join("rrecall.lock")).unwrap();
    assert!(!text.contains(&token), "{text}");
    assert!(w.lock()["collectors"]["repo"].get("cursor").is_none());
    assert!(w.report().contains("cursor"), "{}", w.report());
}

#[test]
fn an_oversized_or_null_cursor_fails_the_output() {
    for cursor in [json!("x".repeat(5000)), Value::Null] {
        let w = seq_world(&[with_cursor(&[], cursor.clone())]);
        let o = w.run_code(&[], 5);
        assert!(o.stderr.contains("cursor"), "{}", o.stderr);
        assert!(w.lock()["collectors"].get("repo").is_none());
    }
}

#[test]
fn a_fetch_returns_the_cursor_and_gets_since() {
    let w = W::new();
    fetching(&w);
    let r = json!({"output": {"proposals": []}, "cost_usd": 0.0, "model": "fake-1"});
    w.config(&format!(
        "{}{}",
        w.inst("writer", "fetching", "out = '{\"cursor\": {\"n\": 7}, \"pulls\": []}'"),
        w.inst("runner", "runner", &format!("out = {}", q(&r.to_string()))),
    ));
    w.run_code(&[], 0);
    assert_eq!(w.input("writer", 1)["since"], Value::Null);
    assert_eq!(w.input("runner", 1)["input"]["since"], Value::Null);
    assert_eq!(w.lock()["collectors"]["writer"]["cursor"], json!({"n": 7}));
    let at = w.lock()["collectors"]["writer"]["settled_at"].clone();
    std::fs::write(w.fp(), "v2").unwrap();
    w.run_code(&[], 0);
    let since = json!({"at": at, "cursor": {"n": 7}});
    assert_eq!(w.input("writer", 2)["since"], since);
    assert_eq!(w.input("runner", 2)["input"]["since"], since);
}

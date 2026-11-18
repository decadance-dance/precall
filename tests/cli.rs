//! End-to-end tests: the `rrecall` binary against stores and repos built in temp dirs.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

/// An isolated world: its own HOME, XDG dirs and a work directory.
struct Env {
    root: PathBuf,
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Env {
    fn new() -> Env {
        static N: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "rrecall-cli-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["home", "data", "config", "work"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        Env { root }
    }

    fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    fn store(&self, project: &str) -> PathBuf {
        self.root.join("data/rrecall").join(project)
    }

    fn user_config(&self) -> PathBuf {
        self.root.join("config/rrecall/config.toml")
    }

    fn run_in(&self, cwd: &Path, args: &[&str]) -> Out {
        let o = Command::new(env!("CARGO_BIN_EXE_rrecall"))
            .args(args)
            .current_dir(cwd)
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

    fn run(&self, args: &[&str]) -> Out {
        self.run_in(&self.work(), args)
    }

    /// Runs and asserts exit 0; returns stdout.
    fn ok(&self, args: &[&str]) -> String {
        let o = self.run(args);
        assert_eq!(
            o.code, 0,
            "rrecall {args:?}\nstdout: {}\nstderr: {}",
            o.stdout, o.stderr
        );
        o.stdout
    }

    /// A project repository with an optional origin remote.
    fn repo(&self, name: &str, origin: Option<&str>) -> PathBuf {
        let dir = self.root.join("repos").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        if let Some(url) = origin {
            git(&dir, &["remote", "add", "origin", url]);
        }
        dir
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.work().join(rel);
        std::fs::write(&p, text).unwrap();
        p
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
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

const ORIGIN: &str = "git@github.com:lanternfish-dev/lanternfish.git";

// ---- init and project resolution (spec §2) ----

#[test]
fn init_creates_store_with_default_skeleton_and_found_sources() {
    let e = Env::new();
    let repo = e.repo("lanternfish", Some(ORIGIN));
    let o = e.run_in(&repo, &["init"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let store = e.store("lanternfish");
    let cfg = std::fs::read_to_string(store.join("rrecall.toml")).unwrap();
    assert!(
        cfg.contains("id = \"repo\"") && cfg.contains("use = \"files\""),
        "{cfg}"
    );
    assert!(
        cfg.contains(&format!(
            "path = \"{}\"",
            repo.canonicalize().unwrap().display()
        )),
        "{cfg}"
    );
    assert!(
        cfg.contains("repo = \"lanternfish-dev/lanternfish\""),
        "{cfg}"
    );
    assert_eq!(
        std::fs::read_to_string(store.join(".gitignore")).unwrap(),
        "cache/\nruns/\nviews/\n"
    );
    assert_eq!(git(&store, &["remote"]), "", "no remote is ever added");
    assert_eq!(git(&store, &["log", "--format=%s"]), "init lanternfish\n");
    assert_eq!(git(&store, &["status", "--porcelain"]), "");
    let user = std::fs::read_to_string(e.user_config()).unwrap();
    assert!(
        user.contains("name = \"lanternfish\"") && user.contains(ORIGIN),
        "{user}"
    );
    // The origin now resolves the project from inside the repo.
    let ls = e.run_in(&repo, &["ls"]);
    assert_eq!(ls.code, 0, "{}", ls.stderr);
    for b in [
        "Layout (layout)",
        "How it works (how-it-works)",
        "People (people)",
        "Done (done)",
        "Blocked (blocked)",
        "Decisions (decisions)",
    ] {
        assert!(ls.stdout.contains(b), "{b} in {}", ls.stdout);
    }
}

#[test]
fn init_names_and_store_overrides() {
    let e = Env::new();
    // Outside a repo with no name: usage error.
    assert_eq!(e.run(&["init"]).code, 2);
    // A store path from the user config wins over the default.
    let custom = e.root.join("elsewhere/kiln");
    std::fs::create_dir_all(e.user_config().parent().unwrap()).unwrap();
    std::fs::write(
        e.user_config(),
        format!(
            "[[projects]]\nname = \"kiln\"\nstore = \"{}\"\n",
            custom.display()
        ),
    )
    .unwrap();
    e.ok(&["init", "kiln"]);
    assert!(custom.join("rrecall.toml").exists());
    assert!(!e.store("kiln").exists());
    let cfg = std::fs::read_to_string(custom.join("rrecall.toml")).unwrap();
    assert!(
        !cfg.contains("[[plugins]]"),
        "no sources found outside a repo: {cfg}"
    );
    // A second init of the same project is a usage error.
    let again = e.run(&["init", "kiln"]);
    assert_eq!(again.code, 2, "{}", again.stderr);
    assert!(again.stderr.contains("already"), "{}", again.stderr);
    // A name that is not a plain directory name is refused.
    assert_eq!(e.run(&["init", "../escape"]).code, 2);
}

#[test]
fn project_from_flag_env_or_origin() {
    let e = Env::new();
    e.ok(&["init", "kiln"]);
    assert!(e.ok(&["--project", "kiln", "ls"]).contains("(layout)"));
    let o = Command::new(env!("CARGO_BIN_EXE_rrecall"))
        .arg("ls")
        .current_dir(e.work())
        .env("HOME", e.root.join("home"))
        .env("XDG_DATA_HOME", e.root.join("data"))
        .env("XDG_CONFIG_HOME", e.root.join("config"))
        .env("RRECALL_PROJECT", "kiln")
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    // --project wins over the origin of the current repo.
    let repo = e.repo("other", Some("https://example.org/other.git"));
    assert_eq!(e.run_in(&repo, &["--project", "kiln", "ls"]).code, 0);
}

#[test]
fn exit_codes() {
    let e = Env::new();
    // 2: no way to tell the project.
    let o = e.run(&["ls"]);
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stderr.contains("--project"), "{}", o.stderr);
    // 2: bad arguments.
    assert_eq!(e.run(&["ls", "--depth", "x"]).code, 2);
    assert_eq!(e.run(&["frobnicate"]).code, 2);
    // 3: a named project without a store, and an origin the user config does not know.
    let o = e.run(&["--project", "ghost", "ls"]);
    assert_eq!(o.code, 3, "{}", o.stderr);
    let repo = e.repo("stray", Some("https://example.org/stray.git"));
    let o = e.run_in(&repo, &["ls"]);
    assert_eq!(o.code, 3, "{}", o.stderr);
    assert!(
        o.stderr.contains("https://example.org/stray.git"),
        "{}",
        o.stderr
    );
    // 1: not found, 0: done.
    e.ok(&["init", "kiln"]);
    assert_eq!(
        e.run(&["--project", "kiln", "show", "nothing-here"]).code,
        1
    );
    assert_eq!(e.run(&["--project", "kiln", "show", "layout"]).code, 0);
    // 1: any other error, such as a corrupt map file.
    std::fs::create_dir_all(e.store("kiln").join("map")).unwrap();
    std::fs::write(e.store("kiln").join("map/layout.jsonl"), "not json\n").unwrap();
    let o = e.run(&["--project", "kiln", "ls"]);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("map/layout.jsonl line 1"), "{}", o.stderr);
}

#[test]
fn help_lists_m1_commands_only() {
    let e = Env::new();
    let help = e.ok(&["--help"]);
    for c in [
        "init",
        "import",
        "ls",
        "show",
        "find",
        "why",
        "recent",
        "status",
        "note",
        "rm",
        "agent-instructions",
    ] {
        assert!(
            help.contains(&format!("  {c} ")) || help.contains(&format!("  {c}\n")),
            "{c} missing:\n{help}"
        );
    }
    for later in ["  run", "  open", "  doctor", "  plugin"] {
        assert!(!help.contains(later), "{later} listed:\n{help}");
    }
}

// ---- import (spec §2) ----

const SMALL_OUTLINE: &str = "\
Layout
  API gateway | Terminates TLS and routes /v1 to the sync service.
    Rate limits | 50 requests per minute per token.
  Web client | Svelte app under web/.
Decisions
  SQLite for feeds | One file per user keeps backups simple.
Services | Which services run in production?
  Sync service | Merges read state across devices.
  Fetcher | Polls feeds every 15 minutes.
";

/// Rebuilds the outline from `show <branch> --depth 99 --json` of every branch.
fn outline_from_map(e: &Env, project: &str) -> String {
    let ls: serde_json::Value =
        serde_json::from_str(&e.ok(&["--project", project, "ls", "--depth", "1", "--json"]))
            .unwrap();
    let mut out = String::new();
    for b in ls["nodes"].as_array().unwrap() {
        let id = b["id"].as_str().unwrap();
        let show: serde_json::Value = serde_json::from_str(&e.ok(&[
            "--project",
            project,
            "show",
            id,
            "--depth",
            "99",
            "--json",
        ]))
        .unwrap();
        let nodes = show["nodes"].as_array().unwrap();
        if nodes.len() == 1 && !["services"].contains(&id) {
            continue; // an empty default branch was not in the outline
        }
        let mut level = std::collections::HashMap::new();
        for n in nodes {
            let l = n["parent"].as_str().map_or(0, |p| level[p] + 1);
            level.insert(n["id"].as_str().unwrap().to_string(), l);
            let (title, body) = (n["title"].as_str().unwrap(), n["body"].as_str().unwrap());
            let default_question = l == 0 && id != "services";
            out += &"  ".repeat(l);
            out += title;
            if !body.is_empty() && !default_question {
                out += &format!(" | {body}");
            }
            out += "\n";
        }
    }
    out
}

#[test]
fn import_round_trip() {
    let e = Env::new();
    e.ok(&["init", "kiln"]);
    let file = e.write("outline.txt", SMALL_OUTLINE);
    let out = e.ok(&["--project", "kiln", "import", file.to_str().unwrap()]);
    assert!(out.contains("services"), "{out}");
    assert_eq!(outline_from_map(&e, "kiln"), SMALL_OUTLINE);

    let store = e.store("kiln");
    let api = e.ok(&["--project", "kiln", "show", "api-gateway", "--json"]);
    assert!(api.contains(r#""sources":["import:outline.txt"]"#), "{api}");
    let cfg = std::fs::read_to_string(store.join("rrecall.toml")).unwrap();
    assert!(cfg.contains("[[skeleton.branch]]\nid = \"services\"\ntitle = \"Services\"\nquestion = \"Which services run in production?\""), "{cfg}");
    // The new branch and the nodes land in one commit.
    assert_eq!(
        git(&store, &["log", "--format=%s"]),
        "import outline.txt: +6 nodes, +1 branches\ninit kiln\n"
    );
    let files = git(&store, &["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(
        files,
        "map/decisions.jsonl\nmap/layout.jsonl\nmap/services.jsonl\nrrecall.toml\n"
    );
    assert_eq!(git(&store, &["status", "--porcelain"]), "");
    // Import seeds an empty map only.
    let again = e.run(&["--project", "kiln", "import", file.to_str().unwrap()]);
    assert_eq!(again.code, 2, "{}", again.stderr);
}

#[test]
fn import_rejects_bad_outlines_and_changes_nothing() {
    let e = Env::new();
    e.ok(&["init", "kiln"]);
    let long = "x".repeat(81);
    for (text, want) in [
        ("Layout | a detail on a default branch\n  API\n", "line 1"),
        (&*format!("Layout\n  {long}\n"), "line 2"),
        ("Layout\n    API\n  Web\n", "line 3"),
    ] {
        let file = e.write("bad.txt", text);
        let o = e.run(&["--project", "kiln", "import", file.to_str().unwrap()]);
        assert_eq!(o.code, 2, "{text}: {}", o.stderr);
        assert!(o.stderr.contains(want), "{text}: {}", o.stderr);
    }
    let store = e.store("kiln");
    assert_eq!(git(&store, &["status", "--porcelain"]), "");
    assert_eq!(git(&store, &["log", "--format=%s"]), "init kiln\n");
}

// ---- queries (spec §7) on the invented 300-node fixture ----

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/lanternfish-outline.txt"
);

/// A store `lanternfish` seeded from the 300-node fixture.
fn fixture_env() -> Env {
    let e = Env::new();
    e.ok(&["init", "lanternfish"]);
    let out = e.ok(&["--project", "lanternfish", "import", FIXTURE]);
    assert!(out.contains("Imported 300 nodes"), "{out}");
    e
}

fn q(e: &Env, args: &[&str]) -> String {
    let mut all = vec!["--project", "lanternfish"];
    all.extend_from_slice(args);
    e.ok(&all)
}

/// Tokens are estimated as bytes / 4, rounded up: the usual rate for English text and ids.
fn tokens(s: &str) -> usize {
    s.len().div_ceil(4)
}

#[test]
fn ls_on_300_node_fixture_stays_under_1500_tokens() {
    let e = fixture_env();
    let ls = q(&e, &["ls"]);
    assert!(tokens(&ls) < 1500, "ls is {} tokens:\n{ls}", tokens(&ls));
    assert_eq!(
        ls.lines().count(),
        7 + 63,
        "branches and their children:\n{ls}"
    );
    assert!(
        ls.starts_with("- Layout (layout)\n  - Sync server (sync-server) +10\n"),
        "{ls}"
    );
    assert!(
        ls.contains("\n- Services (services)\n  - sync-server (sync-server-3) +"),
        "{ls}"
    );
    assert!(!ls.contains("HTTP routes"), "depth 3 is one show away");
    let ls1 = q(&e, &["ls", "--depth", "1"]);
    assert_eq!(ls1.lines().next(), Some("- Layout (layout) +52"));
    assert_eq!(ls1.lines().count(), 7);
    assert!(q(&e, &["ls", "--depth", "3"]).contains("    - HTTP routes (http-routes) +4\n"));
}

#[test]
fn show_prints_block_with_children_as_titles() {
    let e = fixture_env();
    let show = q(&e, &["show", "sync-server"]);
    assert_eq!(
        show,
        "## Layout > Sync server (sync-server)\n\
         Rust service under server/ that stores read state and subscriptions per user.\n\
         sources: import:lanternfish-outline.txt\n\
         - HTTP routes (http-routes) +4\n\
         - Storage layer (storage-layer) +2\n\
         - Migrations (migrations)\n\
         - Config loader (config-loader)\n"
    );
    let deep = q(&e, &["show", "sync-server", "--depth", "1"]);
    assert!(
        deep.contains("\n## Layout > Sync server > HTTP routes (http-routes)\nserver/src/routes/"),
        "{deep}"
    );
    assert!(deep.contains("- /v1/feeds (v1-feeds)\n"), "{deep}");
    assert_eq!(deep.matches("## ").count(), 5);
    // A branch shows its question and the config as its source.
    assert!(q(&e, &["show", "services"]).starts_with(
        "## Services (services)\nWhich services run in production?\nsources: config:rrecall.toml\n"
    ));
}

#[test]
fn show_max_tokens_cuts_at_a_node_boundary() {
    let e = fixture_env();
    let full = q(&e, &["show", "layout", "--depth", "3"]);
    assert_eq!(full.matches("## ").count(), 53);
    let cut = q(
        &e,
        &["show", "layout", "--depth", "3", "--max-tokens", "300"],
    );
    assert!(tokens(&cut) <= 300, "{} tokens:\n{cut}", tokens(&cut));
    let shown = cut.matches("## ").count();
    assert!(shown > 1 && shown < 53, "{cut}");
    let footer = cut.lines().last().unwrap();
    assert!(
        footer.starts_with(&format!(
            "[{} nodes left out; see them with: rrecall show ",
            53 - shown
        )),
        "{footer}"
    );
    // Each block printed is whole: the cut output is a prefix of the full one.
    assert!(
        full.starts_with(cut.trim_end().trim_end_matches(footer).trim_end()),
        "{cut}"
    );
    // Even a tiny budget prints the first node.
    let tiny = q(&e, &["show", "layout", "--depth", "1", "--max-tokens", "1"]);
    assert!(tiny.starts_with("## Layout (layout)\n"), "{tiny}");
    assert!(tiny.ends_with("[9 nodes left out; see them with: rrecall show sync-server, rrecall show fetcher, rrecall show web-client, rrecall show mobile-app, rrecall show queue, rrecall show infrastructure-code, rrecall show docs-site, rrecall show test-suites, rrecall show release-tooling]\n"), "{tiny}");
}

#[test]
fn show_json_has_nodes_paths_children_and_left_out() {
    let e = fixture_env();
    let v: serde_json::Value =
        serde_json::from_str(&q(&e, &["show", "fetcher", "--json"])).unwrap();
    let n = &v["nodes"][0];
    assert_eq!(n["id"], "fetcher");
    assert_eq!(n["parent"], "layout");
    assert_eq!(n["path"], "Layout > Fetcher");
    assert_eq!(
        n["sources"],
        serde_json::json!(["import:lanternfish-outline.txt"])
    );
    assert_eq!(
        n["children"],
        serde_json::json!(["scheduler", "parser", "politeness", "dead-feed-detector"])
    );
    assert!(v.get("left_out").is_none());
    let cut: serde_json::Value = serde_json::from_str(&q(
        &e,
        &[
            "show",
            "fetcher",
            "--depth",
            "1",
            "--max-tokens",
            "1",
            "--json",
        ],
    ))
    .unwrap();
    assert_eq!(cut["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(
        cut["left_out"],
        serde_json::json!(["scheduler", "parser", "politeness", "dead-feed-detector"])
    );
}

#[test]
fn find_matches_title_or_body_with_paths() {
    let e = fixture_env();
    let f = q(&e, &["find", "RETRY-after"]);
    assert_eq!(f.matches("## ").count(), 3, "{f}");
    assert!(
        f.contains("## Layout > Fetcher > Politeness (politeness)\n"),
        "{f}"
    );
    assert!(
        f.contains("## Services > caddy > Rate limiting > Response (response)\n"),
        "{f}"
    );
    let v: serde_json::Value =
        serde_json::from_str(&q(&e, &["find", "Politeness", "--json"])).unwrap();
    let ids: Vec<&str> = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["politeness", "politeness-2"]);
    let o = e.run(&["--project", "lanternfish", "find", "kubernetes"]);
    assert_eq!(o.code, 1, "{}", o.stderr);
}

#[test]
fn rewrite_of_imported_300_node_store_keeps_bytes() {
    let e = fixture_env();
    let store = e.store("lanternfish");
    let read_map = || {
        let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(store.join("map"))
            .unwrap()
            .map(|f| f.unwrap().path())
            .map(|p| (p.display().to_string(), std::fs::read(&p).unwrap()))
            .collect();
        files.sort();
        files
    };
    let before = read_map();
    rrecall::store::Store::load(&store).unwrap().save().unwrap();
    assert_eq!(read_map(), before);
    assert_eq!(git(&store, &["status", "--porcelain"]), "");
}

// ---- note, rm, why, recent, status, agent-instructions ----

/// `kiln` seeded from the small outline.
fn small_env() -> Env {
    let e = Env::new();
    e.ok(&["init", "kiln"]);
    let file = e.write("outline.txt", SMALL_OUTLINE);
    e.ok(&["--project", "kiln", "import", file.to_str().unwrap()]);
    e
}

fn k(e: &Env, args: &[&str]) -> Out {
    let mut all = vec!["--project", "kiln"];
    all.extend_from_slice(args);
    e.run(&all)
}

#[test]
fn note_writes_only_the_note_field_in_its_own_commit() {
    let e = small_env();
    let store = e.store("kiln");
    let before = std::fs::read_to_string(store.join("map/layout.jsonl")).unwrap();
    assert_eq!(
        k(
            &e,
            &["note", "api-gateway", "ask Tomasz before changing routes"]
        )
        .code,
        0
    );
    assert_eq!(
        git(&store, &["log", "-1", "--format=%s"]),
        "note layout/api-gateway\n"
    );
    let after = std::fs::read_to_string(store.join("map/layout.jsonl")).unwrap();
    let changed: Vec<(&str, &str)> = before
        .lines()
        .zip(after.lines())
        .filter(|(a, b)| a != b)
        .collect();
    assert_eq!(changed.len(), 1, "only the noted node's line changes");
    assert_eq!(
        changed[0].1,
        changed[0]
            .0
            .replace("}", r#","note":"ask Tomasz before changing routes"}"#)
    );
    assert!(
        k(&e, &["show", "api-gateway"])
            .stdout
            .contains("note: ask Tomasz before changing routes\n")
    );
    // The same note again: no commit.
    assert_eq!(
        k(
            &e,
            &["note", "api-gateway", "ask Tomasz before changing routes"]
        )
        .code,
        0
    );
    assert_eq!(git(&store, &["rev-list", "--count", "HEAD"]), "3\n");
    // An empty text clears it, back to the original bytes.
    assert_eq!(k(&e, &["note", "api-gateway", ""]).code, 0);
    assert_eq!(
        git(&store, &["log", "-1", "--format=%s"]),
        "note layout/api-gateway: cleared\n"
    );
    assert_eq!(
        std::fs::read_to_string(store.join("map/layout.jsonl")).unwrap(),
        before
    );
    assert_eq!(
        k(&e, &["note", "layout", "x"]).code,
        2,
        "branches belong to the config"
    );
    assert_eq!(k(&e, &["note", "nope", "x"]).code, 1);
}

#[test]
fn rm_removes_by_hand_children_only_when_stated() {
    let e = small_env();
    let store = e.store("kiln");
    let o = k(&e, &["rm", "api-gateway"]);
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stderr.contains("--recursive"), "{}", o.stderr);
    assert_eq!(git(&store, &["status", "--porcelain"]), "");
    assert_eq!(
        k(
            &e,
            &[
                "rm",
                "api-gateway",
                "--recursive",
                "--reason",
                "moved to the services branch"
            ]
        )
        .code,
        0
    );
    assert_eq!(
        git(&store, &["log", "-1", "--format=%B"]),
        "rm layout/api-gateway: moved to the services branch\n\nremoved layout/api-gateway\nremoved layout/rate-limits\n\n"
    );
    assert_eq!(k(&e, &["show", "rate-limits"]).code, 1);
    assert_eq!(k(&e, &["rm", "web-client"]).code, 0);
    assert_eq!(
        git(&store, &["log", "-1", "--format=%s"]),
        "rm layout/web-client: removed by hand\n"
    );
    assert!(
        !store.join("map/layout.jsonl").exists(),
        "an empty branch has no file"
    );
    assert_eq!(
        k(&e, &["rm", "layout"]).code,
        2,
        "branches belong to the config"
    );
    assert_eq!(k(&e, &["rm", "nope"]).code, 1);
}

#[test]
fn why_lists_the_commits_that_touched_a_node() {
    let e = small_env();
    k(&e, &["note", "fetcher", "polls too often?"]);
    let why = k(&e, &["why", "fetcher"]).stdout;
    let lines: Vec<&str> = why.lines().collect();
    assert_eq!(lines[0], "## Services > Fetcher (fetcher)");
    assert_eq!(lines[1], "sources: import:outline.txt");
    assert_eq!(lines[2], "history:");
    assert!(
        lines[3].starts_with("- ")
            && lines[3].ends_with(" import outline.txt: +6 nodes, +1 branches"),
        "{why}"
    );
    assert!(lines[4].ends_with(" note services/fetcher"), "{why}");
    assert_eq!(lines.len(), 5, "{why}");
    // A branch from the config dates from the commit that added it; a default one from init.
    assert!(
        k(&e, &["why", "services"])
            .stdout
            .contains(" import outline.txt: +6 nodes")
    );
    assert!(k(&e, &["why", "layout"]).stdout.contains(" init kiln\n"));
    let v: serde_json::Value =
        serde_json::from_str(&k(&e, &["why", "fetcher", "--json"]).stdout).unwrap();
    assert_eq!(v["node"]["id"], "fetcher");
    assert_eq!(v["history"][1]["subject"], "note services/fetcher");
    assert_eq!(k(&e, &["why", "nope"]).code, 1);
}

fn today() -> String {
    let o = Command::new("date")
        .args(["-u", "+%Y-%m-%d"])
        .output()
        .unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

#[test]
fn recent_lists_map_commits_and_dated_nodes_in_the_window() {
    let e = small_env();
    let store = e.store("kiln");
    // Date two nodes by hand: one today, one long ago.
    let path = store.join("map/decisions.jsonl");
    let mut text = std::fs::read_to_string(&path).unwrap();
    let dated = format!(
        r#"{{"id":"old-call","parent":"decisions","title":"Old call","body":"","sources":["import:outline.txt"],"date":"2001-01-01","order":1}}
{{"id":"today-call","parent":"decisions","title":"Today call","body":"","sources":["import:outline.txt"],"date":"{}","order":2}}
"#,
        today()
    );
    text.push_str(&dated);
    std::fs::write(&path, text).unwrap();
    let r = k(&e, &["recent"]).stdout;
    assert!(
        r.contains(" import outline.txt: +6 nodes, +1 branches\n"),
        "{r}"
    );
    assert!(r.contains(" init kiln\n"), "{r}");
    assert!(
        r.contains(&format!(
            "- {} Decisions > Today call (today-call)\n",
            today()
        )),
        "{r}"
    );
    assert!(!r.contains("old-call"), "{r}");
    let v: serde_json::Value =
        serde_json::from_str(&k(&e, &["recent", "--since", "2w", "--json"]).stdout).unwrap();
    assert_eq!(v["commits"].as_array().unwrap().len(), 2);
    assert_eq!(v["nodes"][0]["id"], "today-call");
    assert_eq!(k(&e, &["recent", "--since", "7x"]).code, 2);
}

#[test]
fn status_prints_the_last_report_or_says_there_is_none() {
    let e = small_env();
    let s = k(&e, &["status"]).stdout;
    assert!(s.contains("No run report yet"), "{s}");
    let runs = e.store("kiln").join("runs");
    for (id, text) in [
        ("2026-11-01T0900Z", "# Run one\n"),
        ("2026-11-08T0900Z", "# Run two\nadded 3\n"),
    ] {
        std::fs::create_dir_all(runs.join(id)).unwrap();
        std::fs::write(runs.join(id).join("report.md"), text).unwrap();
    }
    assert_eq!(
        k(&e, &["status"]).stdout,
        "Last run: 2026-11-08T0900Z\n\n# Run two\nadded 3\n"
    );
    assert_eq!(
        git(&e.store("kiln"), &["status", "--porcelain"]),
        "",
        "runs/ is untracked"
    );
}

#[test]
fn agent_instructions_name_the_query_commands() {
    let e = Env::new();
    let text = e.ok(&["agent-instructions"]);
    for c in [
        "rrecall ls",
        "rrecall show <id>",
        "rrecall find <text>",
        "rrecall recent",
        "rrecall why <id>",
        "--json",
    ] {
        assert!(text.contains(c), "{c}: {text}");
    }
    assert!(tokens(&text) < 250, "a short paragraph: {}", tokens(&text));
}

#[test]
fn import_appends_when_two_lines_fill_one_branch() {
    let e = Env::new();
    e.ok(&["init", "kiln"]);
    let file = e.write("twice.txt", "Layout\n  Zed\nlayout\n  Alpha\n");
    e.ok(&["--project", "kiln", "import", file.to_str().unwrap()]);
    let show = e.ok(&["--project", "kiln", "show", "layout"]);
    assert!(
        show.ends_with("- Zed (zed)\n- Alpha (alpha)\n"),
        "new nodes go last:\n{show}"
    );
}

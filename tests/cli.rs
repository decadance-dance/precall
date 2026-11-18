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
        assert_eq!(o.code, 0, "rrecall {args:?}\nstdout: {}\nstderr: {}", o.stdout, o.stderr);
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
    let o = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
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
    assert!(cfg.contains("id = \"repo\"") && cfg.contains("use = \"files\""), "{cfg}");
    assert!(cfg.contains(&format!("path = \"{}\"", repo.canonicalize().unwrap().display())), "{cfg}");
    assert!(cfg.contains("repo = \"lanternfish-dev/lanternfish\""), "{cfg}");
    assert_eq!(std::fs::read_to_string(store.join(".gitignore")).unwrap(), "cache/\nruns/\nviews/\n");
    assert_eq!(git(&store, &["remote"]), "", "no remote is ever added");
    assert_eq!(git(&store, &["log", "--format=%s"]), "init lanternfish\n");
    assert_eq!(git(&store, &["status", "--porcelain"]), "");
    let user = std::fs::read_to_string(e.user_config()).unwrap();
    assert!(user.contains("name = \"lanternfish\"") && user.contains(ORIGIN), "{user}");
    // The origin now resolves the project from inside the repo.
    let ls = e.run_in(&repo, &["ls"]);
    assert_eq!(ls.code, 0, "{}", ls.stderr);
    for b in ["Layout (layout)", "How it works (how-it-works)", "People (people)", "Done (done)", "Blocked (blocked)", "Decisions (decisions)"] {
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
    std::fs::write(e.user_config(), format!("[[projects]]\nname = \"kiln\"\nstore = \"{}\"\n", custom.display())).unwrap();
    e.ok(&["init", "kiln"]);
    assert!(custom.join("rrecall.toml").exists());
    assert!(!e.store("kiln").exists());
    let cfg = std::fs::read_to_string(custom.join("rrecall.toml")).unwrap();
    assert!(!cfg.contains("[[plugins]]"), "no sources found outside a repo: {cfg}");
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
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
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
    assert!(o.stderr.contains("https://example.org/stray.git"), "{}", o.stderr);
    // 1: not found, 0: done.
    e.ok(&["init", "kiln"]);
    assert_eq!(e.run(&["--project", "kiln", "show", "nothing-here"]).code, 1);
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
    for c in ["init", "import", "ls", "show", "find", "why", "recent", "status", "note", "rm", "agent-instructions"] {
        assert!(help.contains(&format!("  {c} ")) || help.contains(&format!("  {c}\n")), "{c} missing:\n{help}");
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
    let ls: serde_json::Value = serde_json::from_str(&e.ok(&["--project", project, "ls", "--depth", "1", "--json"])).unwrap();
    let mut out = String::new();
    for b in ls["nodes"].as_array().unwrap() {
        let id = b["id"].as_str().unwrap();
        let show: serde_json::Value =
            serde_json::from_str(&e.ok(&["--project", project, "show", id, "--depth", "99", "--json"])).unwrap();
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
    assert_eq!(git(&store, &["log", "--format=%s"]), "import outline.txt: +6 nodes, +1 branches\ninit kiln\n");
    let files = git(&store, &["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(files, "map/decisions.jsonl\nmap/layout.jsonl\nmap/services.jsonl\nrrecall.toml\n");
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

//! End-to-end tests of `precall doctor` and `precall plugin list | add | trust` (spec §2, §6.7):
//! plugin sources are directories and local git repos built in temp dirs, binaries are fakes on
//! `PATH`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

/// An isolated world: HOME, XDG dirs, the store `kettle` and its repo under `src/kettle`.
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

fn git_in(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

/// A plugin directory at `dir`: a check `id` of `version` whose command is `./check.sh`.
fn check_plugin(dir: &Path, id: &str, version: &str, requires: &[&str]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("precall-plugin.toml"),
        format!(
            "id = \"{id}\"\nname = \"Kettle check\"\nversion = \"{version}\"\nmin_precall = \"0.1.0\"\nkind = \"check\"\nrequires = {requires:?}\n\n[check]\ncommand = [\"./check.sh\"]\n"
        ),
    )
    .unwrap();
    let script = dir.join("check.sh");
    std::fs::write(&script, "#!/bin/sh\necho '{\"verdicts\": []}'\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl W {
    fn new() -> W {
        static N: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "precall-plugins-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["home/agent", "data", "config", "work", "rec", "src/kettle"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        W {
            root: root.canonicalize().unwrap(),
        }
    }

    /// `new` plus the store `kettle` and a git repo for it.
    fn with_store() -> W {
        let w = W::new();
        let o = w.rr(&["init", "kettle"], "");
        assert_eq!(o.code, 0, "{}", o.stderr);
        git_in(&w.repo(), &["init", "-q", "-b", "main"]);
        w
    }

    fn repo(&self) -> PathBuf {
        self.root.join("src/kettle")
    }

    fn store(&self) -> PathBuf {
        self.root.join("data/precall/kettle")
    }

    fn user_plugins(&self) -> PathBuf {
        self.root.join("config/precall/plugins")
    }

    fn user_config(&self) -> String {
        std::fs::read_to_string(self.root.join("config/precall/config.toml")).unwrap_or_default()
    }

    /// Runs precall in the work dir with `stdin`.
    fn rr(&self, args: &[&str], stdin: &str) -> Out {
        self.rr_env(args, stdin, &[])
    }

    /// `rr` with extra environment, set last.
    fn rr_env(&self, args: &[&str], stdin: &str, env: &[(&str, String)]) -> Out {
        let mut c = Command::new(env!("CARGO_BIN_EXE_precall"));
        c.args(args)
            .current_dir(self.root.join("work"))
            .env("HOME", self.root.join("home"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env(
                "PATH",
                format!(
                    "{}/tests/fixtures/bin:{}",
                    env!("CARGO_MANIFEST_DIR"),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("FAKE_GH_DIR", self.root.join("rec"))
            .env_remove("PRECALL_PROJECT")
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
            si.write_all(stdin.as_bytes()).unwrap();
        }
        let o = child.wait_with_output().unwrap();
        Out {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8(o.stdout).unwrap(),
            stderr: String::from_utf8(o.stderr).unwrap(),
        }
    }

    /// Starts precall in the work dir with stdin closed; the caller waits.
    fn spawn(&self, args: &[&str]) -> std::process::Child {
        Command::new(env!("CARGO_BIN_EXE_precall"))
            .args(args)
            .current_dir(self.root.join("work"))
            .env("HOME", self.root.join("home"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env(
                "PATH",
                format!(
                    "{}/tests/fixtures/bin:{}",
                    env!("CARGO_MANIFEST_DIR"),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("PRECALL_PROJECT")
            .env_remove("GIT_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    /// Replaces the store config with `repo` and `plugins`, and commits it.
    fn config(&self, plugins: &str) {
        let text = format!("repo = {}\n{plugins}", q(&self.repo().to_string_lossy()));
        std::fs::write(self.store().join("precall.toml"), text).unwrap();
        git_in(&self.store(), &["add", "precall.toml"]);
        git_in(&self.store(), &["commit", "-q", "-m", "config"]);
    }
}

#[test]
fn plugin_add_from_a_path_shows_the_manifest_asks_and_records_source_and_hash() {
    let w = W::new();
    let src = w.root.join("src/steam-check");
    check_plugin(&src, "steam-check", "1.2.0", &["jq"]);
    git_in(&src, &["init", "-q"]);
    let o = w.rr(&["plugin", "add", src.to_str().unwrap()], "y\n");
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(o.stdout.contains("id = \"steam-check\""), "{}", o.stdout);
    assert!(o.stdout.contains("command: ./check.sh"), "{}", o.stdout);
    assert!(o.stdout.contains("requires: jq"), "{}", o.stdout);
    assert!(
        o.stderr.contains("Install steam-check into"),
        "{}",
        o.stderr
    );
    let dir = w.user_plugins().join("steam-check");
    assert!(dir.join("check.sh").is_file());
    assert!(!dir.join(".git").exists(), ".git is not copied");
    let hash = precall::plugin::content_hash(&dir).unwrap();
    let cfg = w.user_config();
    assert!(cfg.contains("[[installed]]"), "{cfg}");
    assert!(
        cfg.contains(&format!("source = {}", q(src.to_str().unwrap()))),
        "{cfg}"
    );
    assert!(cfg.contains(&format!("hash = \"{hash}\"")), "{cfg}");

    let l = w.rr(&["plugin", "list"], "");
    assert_eq!(l.code, 0, "{}", l.stderr);
    let line = l
        .stdout
        .lines()
        .find(|l| l.contains("steam-check"))
        .unwrap();
    assert!(
        line.contains("check") && line.contains("1.2.0") && line.contains(&hash),
        "{line}"
    );
    assert!(line.contains(&format!("from {}", src.display())), "{line}");
    assert!(l.stdout.contains("no project"), "{}", l.stdout);
    assert!(
        l.stdout.contains("no-op"),
        "built-ins are listed: {}",
        l.stdout
    );
    assert!(!l.stdout.contains("changed since add"), "{}", l.stdout);
    std::fs::write(dir.join("check.sh"), "#!/bin/sh\necho edited\n").unwrap();
    let l = w.rr(&["plugin", "list"], "");
    assert!(l.stdout.contains("changed since add"), "{}", l.stdout);
}

#[test]
fn plugin_add_declined_or_unanswered_installs_nothing() {
    let w = W::new();
    let src = w.root.join("src/steam-check");
    check_plugin(&src, "steam-check", "1.0.0", &[]);
    for answer in ["n\n", "", "\n"] {
        let o = w.rr(&["plugin", "add", src.to_str().unwrap()], answer);
        assert_eq!(o.code, 1, "{answer:?}: {}{}", o.stdout, o.stderr);
        assert!(!w.user_plugins().join("steam-check").exists());
        assert!(!w.user_config().contains("installed"));
    }
    let o = w.rr(&["plugin", "add", src.to_str().unwrap()], "YES\n");
    assert_eq!(o.code, 0, "{}", o.stderr);
}

#[test]
fn plugin_add_from_git_pins_the_ref() {
    let w = W::new();
    let src = w.root.join("src/steam-check");
    check_plugin(&src, "steam-check", "1.0.0", &[]);
    git_in(&src, &["init", "-q", "-b", "main"]);
    git_in(&src, &["add", "."]);
    git_in(&src, &["commit", "-q", "-m", "v1"]);
    git_in(&src, &["tag", "v1"]);
    let v1 = git_in(&src, &["rev-parse", "HEAD"]);
    check_plugin(&src, "steam-check", "2.0.0", &[]);
    git_in(&src, &["commit", "-q", "-am", "v2"]);
    let url = format!("file://{}", src.display());
    let o = w.rr(&["plugin", "add", &url, "--ref", "v1"], "y\n");
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(o.stdout.contains("version = \"1.0.0\""), "{}", o.stdout);
    let dir = w.user_plugins().join("steam-check");
    assert!(!dir.join(".git").exists());
    let cfg = w.user_config();
    assert!(cfg.contains(&format!("source = {}", q(&url))), "{cfg}");
    assert!(cfg.contains("ref = \"v1\""), "{cfg}");
    assert!(cfg.contains(&format!("commit = \"{v1}\"")), "{cfg}");
    let l = w.rr(&["plugin", "list"], "");
    assert!(l.stdout.contains(&format!("from {url}@v1")), "{}", l.stdout);
}

#[test]
fn plugin_add_refuses_an_installed_id_and_a_bad_manifest_before_asking() {
    let w = W::new();
    let src = w.root.join("src/steam-check");
    check_plugin(&src, "steam-check", "1.0.0", &[]);
    assert_eq!(
        w.rr(&["plugin", "add", src.to_str().unwrap()], "y\n").code,
        0
    );
    let o = w.rr(&["plugin", "add", src.to_str().unwrap()], "y\n");
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stderr.contains("already installed"), "{}", o.stderr);
    assert_eq!(w.user_config().matches("[[installed]]").count(), 1);

    let bad = w.root.join("src/bad");
    check_plugin(&bad, "bad-check", "1.0.0", &[]);
    let m = bad.join("precall-plugin.toml");
    let text = std::fs::read_to_string(&m).unwrap();
    std::fs::write(&m, format!("color = \"red\"\n{text}")).unwrap();
    let o = w.rr(&["plugin", "add", bad.to_str().unwrap()], "y\n");
    assert_ne!(o.code, 0);
    assert!(o.stderr.contains("color"), "{}", o.stderr);
    assert!(!o.stderr.contains("Install"), "no prompt: {}", o.stderr);
    assert!(!w.user_plugins().join("bad-check").exists());
}

#[test]
fn plugin_trust_records_the_directory_and_a_changed_hash_needs_trust_again() {
    let w = W::with_store();
    let dir = w.repo().join(".precall/plugins");
    check_plugin(&dir.join("lid-check"), "lid-check", "1.0.0", &[]);
    w.config("\n[[plugins]]\nid = \"lid-check\"\n");
    let l = w.rr(&["--project", "kettle", "plugin", "list"], "");
    assert!(l.stdout.contains("not trusted"), "{}", l.stdout);

    let o = w.rr(
        &[
            "--project",
            "kettle",
            "plugin",
            "trust",
            dir.to_str().unwrap(),
        ],
        "y\n",
    );
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(o.stdout.contains("id = \"lid-check\""), "{}", o.stdout);
    assert!(o.stderr.contains("Trust "), "{}", o.stderr);
    let hash = precall::plugin::content_hash(&dir).unwrap();
    let cfg = w.user_config();
    assert!(
        cfg.contains(&format!("path = {}", q(dir.to_str().unwrap()))),
        "{cfg}"
    );
    assert!(cfg.contains(&format!("hash = \"{hash}\"")), "{cfg}");
    let l = w.rr(&["--project", "kettle", "plugin", "list"], "");
    let line = l.stdout.lines().find(|l| l.contains("lid-check")).unwrap();
    assert!(line.contains("instances: lid-check"), "{}", l.stdout);
    assert!(l.stdout.contains("project"), "{}", l.stdout);

    let again = w.rr(
        &[
            "--project",
            "kettle",
            "plugin",
            "trust",
            dir.to_str().unwrap(),
        ],
        "",
    );
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert!(again.stdout.contains("already trusted"), "{}", again.stdout);
    assert_eq!(w.user_config().matches("[[trusted]]").count(), 1);

    std::fs::write(dir.join("lid-check/check.sh"), "#!/bin/sh\necho changed\n").unwrap();
    let run = w.rr(&["--project", "kettle", "run"], "");
    assert_eq!(run.code, 4, "{}", run.stderr);
    assert!(
        run.stderr.contains(&format!(
            "precall --project kettle plugin trust '{}'",
            dir.display()
        )),
        "{}",
        run.stderr
    );
    let declined = w.rr(
        &[
            "--project",
            "kettle",
            "plugin",
            "trust",
            dir.to_str().unwrap(),
        ],
        "n\n",
    );
    assert_eq!(declined.code, 1);
    assert_eq!(w.user_config().matches("[[trusted]]").count(), 1);
    assert_eq!(
        w.rr(
            &[
                "--project",
                "kettle",
                "plugin",
                "trust",
                dir.to_str().unwrap()
            ],
            "y\n"
        )
        .code,
        0
    );
    assert_eq!(w.user_config().matches("[[trusted]]").count(), 2);
}

#[test]
fn plugin_list_marks_a_replaced_plugin_and_names_instances() {
    let w = W::with_store();
    check_plugin(&w.store().join("plugins/secrets"), "secrets", "9.0.0", &[]);
    w.config("\n[[plugins]]\nid = \"secrets\"\n\n[[plugins]]\nid = \"no-op\"\n\n[[plugins]]\nid = \"tidy\"\nuse = \"no-op\"\n");
    let l = w.rr(&["--project", "kettle", "plugin", "list"], "");
    assert_eq!(l.code, 0, "{}", l.stderr);
    let lines: Vec<&str> = l.stdout.lines().collect();
    let builtin_secrets = lines
        .iter()
        .find(|l| l.contains("secrets") && l.contains("built-in"))
        .unwrap();
    assert!(
        builtin_secrets.contains("replaced by store"),
        "{}",
        l.stdout
    );
    let store_secrets = lines
        .iter()
        .find(|l| l.contains("secrets") && l.contains("9.0.0"))
        .unwrap();
    assert!(store_secrets.contains("store") && store_secrets.contains("instances: secrets"));
    let noop = lines.iter().find(|l| l.contains("no-op ")).unwrap();
    assert!(noop.contains("instances: no-op, tidy"), "{noop}");
    assert!(noop.contains("sha256:"), "{noop}");
}

fn github_ok(w: &W) {
    std::fs::write(
        w.root.join("rec/gh.api.issues"),
        r#"[{"number": 3, "updated_at": "2026-11-01T00:00:00Z"}]"#,
    )
    .unwrap();
    std::fs::write(w.root.join("rec/gh.api.releases"), "[]").unwrap();
}

const DOCTOR_CONFIG: &str = "\n[[plugins]]\nid = \"github\"\nparams = { repo = \"acme/kettle\" }\n\n[[plugins]]\nid = \"no-op\"\n\n[[plugins]]\nid = \"support\"\n\n[[plugins]]\nid = \"claude\"\n";

#[test]
fn doctor_passes_when_every_enabled_plugin_can_run() {
    let w = W::with_store();
    w.config(DOCTOR_CONFIG);
    github_ok(&w);
    let o = w.rr(&["--project", "kettle", "doctor"], "");
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    for want in [
        "ok    requires gh",
        "ok    fingerprint",
        "ok    login",
        "ok    runner claude",
    ] {
        assert!(o.stdout.contains(want), "{want}: {}", o.stdout);
    }
    assert!(!o.stdout.contains("fail"), "{}", o.stdout);
    assert!(w.store().join("cache/doctor").is_dir());
    assert!(!w.store().join("runs").exists(), "doctor makes no run");
}

#[test]
fn doctor_fails_and_names_what_a_run_would_skip() {
    let w = W::with_store();
    check_plugin(
        &w.store().join("plugins/kettle-check"),
        "kettle-check",
        "1.0.0",
        &["no-such-tool-for-precall"],
    );
    w.config(&format!(
        "{DOCTOR_CONFIG}\n[[plugins]]\nid = \"kettle-check\"\n"
    ));
    std::fs::write(w.root.join("home/agent/logged-out"), "").unwrap();
    // No canned `gh api` answers: the github fingerprint fails like an unreachable source.
    let o = w.rr(&["--project", "kettle", "doctor"], "");
    assert_eq!(o.code, 1, "{}{}", o.stdout, o.stderr);
    for want in [
        "fail  requires no-such-tool-for-precall",
        "fail  fingerprint",
        "fail  login",
        "fail  runner claude",
        "ok    program ./check.sh",
    ] {
        assert!(o.stdout.contains(want), "{want}: {}", o.stdout);
    }
    assert!(
        o.stderr
            .contains("a run would skip: github, support, claude, kettle-check"),
        "{}",
        o.stderr
    );
    assert!(o.stdout.contains("no-op\n  ok"), "{}", o.stdout);
}

#[test]
fn doctor_reports_an_untrusted_project_directory_as_the_run_would() {
    let w = W::with_store();
    check_plugin(
        &w.repo().join(".precall/plugins/lid-check"),
        "lid-check",
        "1.0.0",
        &[],
    );
    w.config("\n[[plugins]]\nid = \"lid-check\"\n");
    let o = w.rr(&["--project", "kettle", "doctor"], "");
    assert_eq!(o.code, 4, "{}", o.stderr);
    let dir = w.repo().join(".precall/plugins");
    let hint = format!("precall --project kettle plugin trust '{}'", dir.display());
    assert!(o.stderr.contains(&hint), "{}", o.stderr);
    let l = w.rr(&["--project", "kettle", "plugin", "list"], "");
    assert!(l.stdout.contains(&hint), "{}", l.stdout);
}

#[test]
fn doctor_rejects_an_allowed_runners_list_a_run_would_reject() {
    let w = W::with_store();
    github_ok(&w);
    w.config("\n[[plugins]]\nid = \"github\"\nparams = { repo = \"acme/kettle\" }\nallowed_runners = [\"codex\"]\n\n[[plugins]]\nid = \"claude\"\n\n[[plugins]]\nid = \"codex\"\n");
    let o = w.rr(&["--project", "kettle", "doctor"], "");
    assert_eq!(o.code, 4, "{}{}", o.stdout, o.stderr);
    assert!(o.stderr.contains("allowed_runners"), "{}", o.stderr);
}

// ---- review round 1 ----

/// A git repo at `dir` holding the check `steam-check` 1.0.0, tagged `v1`, then 2.0.0 on top.
fn tagged_source(dir: &Path) -> String {
    check_plugin(dir, "steam-check", "1.0.0", &[]);
    git_in(dir, &["init", "-q", "-b", "main"]);
    git_in(dir, &["add", "."]);
    git_in(dir, &["commit", "-q", "-m", "v1"]);
    git_in(dir, &["tag", "v1"]);
    let v1 = git_in(dir, &["rev-parse", "HEAD"]);
    check_plugin(dir, "steam-check", "2.0.0", &[]);
    git_in(dir, &["commit", "-q", "-am", "v2"]);
    v1
}

#[test]
fn plugin_add_takes_a_relative_path_with_ref_from_the_working_directory() {
    let w = W::new();
    let v1 = tagged_source(&w.root.join("work/steam"));
    let o = w.rr(&["plugin", "add", "./steam", "--ref", "v1"], "y\n");
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let cfg = w.user_config();
    let abs = w.root.join("work/steam");
    assert!(
        cfg.contains(&format!("source = {}", q(abs.to_str().unwrap()))),
        "{cfg}"
    );
    assert!(cfg.contains(&format!("commit = \"{v1}\"")), "{cfg}");
}

#[test]
fn plugin_add_refuses_an_id_installed_under_another_directory_name() {
    let w = W::new();
    check_plugin(
        &w.user_plugins().join("other-folder"),
        "steam-check",
        "1.0.0",
        &[],
    );
    let src = w.root.join("src/steam-check");
    check_plugin(&src, "steam-check", "2.0.0", &[]);
    let o = w.rr(&["plugin", "add", src.to_str().unwrap()], "y\n");
    assert_eq!(o.code, 2, "{}{}", o.stdout, o.stderr);
    assert!(o.stderr.contains("other-folder"), "{}", o.stderr);
    assert!(!o.stderr.contains("Install"), "no prompt: {}", o.stderr);
    assert!(!w.user_plugins().join("steam-check").exists());
}

#[test]
fn plugin_add_appends_to_an_empty_inline_array() {
    let w = W::new();
    std::fs::create_dir_all(w.root.join("config/precall")).unwrap();
    std::fs::write(
        w.root.join("config/precall/config.toml"),
        "# mine\nrunner = \"claude\"\ninstalled = []\ntrusted = []\n",
    )
    .unwrap();
    let src = w.root.join("src/steam-check");
    check_plugin(&src, "steam-check", "1.0.0", &[]);
    let o = w.rr(&["plugin", "add", src.to_str().unwrap()], "y\n");
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let l = w.rr(&["plugin", "list"], "");
    assert_eq!(l.code, 0, "{}", l.stderr);
    assert!(
        l.stdout.contains(&format!("from {}", src.display())),
        "{}",
        l.stdout
    );
    assert!(
        w.user_config().contains("runner = \"claude\""),
        "{}",
        w.user_config()
    );
}

#[test]
fn plugin_add_runs_git_without_hooks_checkout_or_other_transports_before_asking() {
    let w = W::new();
    tagged_source(&w.root.join("src/steam"));
    let real = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let log = w.root.join("rec/git.log");
    let path = format!(
        "{}/tests/fixtures/git-bin:{}/tests/fixtures/bin:{}",
        env!("CARGO_MANIFEST_DIR"),
        env!("CARGO_MANIFEST_DIR"),
        std::env::var("PATH").unwrap_or_default()
    );
    let url = format!("file://{}", w.root.join("src/steam").display());
    let o = w.rr_env(
        &["plugin", "add", &url, "--ref", "v1"],
        "n\n",
        &[
            ("PATH", path),
            ("FAKE_GIT_LOG", log.to_string_lossy().into_owned()),
            ("FAKE_GIT_REAL", real),
        ],
    );
    assert_eq!(o.code, 1, "{}{}", o.stdout, o.stderr);
    let calls = std::fs::read_to_string(&log).unwrap();
    let clone = calls
        .lines()
        .find(|l| l.split(' ').any(|a| a == "clone"))
        .unwrap();
    for want in [
        "protocol.allow=never",
        "core.hooksPath=",
        "--no-checkout",
        "GIT_ALLOW_PROTOCOL=https:ssh:git:file",
    ] {
        assert!(clone.contains(want), "{want}: {clone}");
    }
    for l in calls.lines() {
        assert!(!l.split(' ').any(|a| a == "checkout"), "no checkout: {l}");
        assert!(
            l.contains("core.hooksPath="),
            "hooks off in every call: {l}"
        );
    }
}

#[test]
fn plugin_add_refuses_the_ext_transport_even_when_git_config_allows_it() {
    let w = W::new();
    let marker = w.root.join("rec/ext-ran");
    let url = format!("ext::sh -c touch% {}", marker.display());
    let o = w.rr_env(
        &["plugin", "add", &url],
        "y\n",
        &[
            ("GIT_CONFIG_COUNT", "1".into()),
            ("GIT_CONFIG_KEY_0", "protocol.ext.allow".into()),
            ("GIT_CONFIG_VALUE_0", "always".into()),
        ],
    );
    assert_ne!(o.code, 0, "{}", o.stderr);
    assert!(!marker.exists(), "the ext transport ran a command");
}

#[test]
fn plugin_add_from_git_runs_no_smudge_filter() {
    let w = W::new();
    let src = w.root.join("src/steam");
    check_plugin(&src, "steam-check", "1.0.0", &[]);
    std::fs::write(src.join(".gitattributes"), "check.sh filter=evil\n").unwrap();
    git_in(&src, &["init", "-q", "-b", "main"]);
    git_in(&src, &["add", "."]);
    git_in(&src, &["commit", "-q", "-m", "v1"]);
    let marker = w.root.join("rec/smudge-ran");
    let o = w.rr_env(
        &["plugin", "add", &format!("file://{}", src.display())],
        "y\n",
        &[
            ("GIT_CONFIG_COUNT", "1".into()),
            ("GIT_CONFIG_KEY_0", "filter.evil.smudge".into()),
            (
                "GIT_CONFIG_VALUE_0",
                format!("touch {}; cat", marker.display()),
            ),
        ],
    );
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(!marker.exists(), "a smudge filter ran");
    let dir = w.user_plugins().join("steam-check");
    let mode = std::fs::metadata(dir.join("check.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0, "the exec bit survives");
}

#[test]
fn plugin_trust_refuses_a_directory_the_loader_never_reads() {
    let w = W::with_store();
    w.config("");
    check_plugin(
        &w.repo().join(".precall/plugins/lid-check"),
        "lid-check",
        "1.0.0",
        &[],
    );
    let other = w.root.join("work/arbitrary");
    check_plugin(&other.join("lid-check"), "lid-check", "1.0.0", &[]);
    let o = w.rr(
        &["--project", "kettle", "plugin", "trust", "./arbitrary"],
        "y\n",
    );
    assert_eq!(o.code, 2, "{}{}", o.stdout, o.stderr);
    let want = w.repo().join(".precall/plugins");
    assert!(o.stderr.contains(want.to_str().unwrap()), "{}", o.stderr);
    assert!(!w.user_config().contains("[[trusted]]"));
    // A path that resolves to the right directory is fine.
    let roundabout = w.repo().join(".precall/../.precall/plugins");
    let o = w.rr(
        &[
            "--project",
            "kettle",
            "plugin",
            "trust",
            roundabout.to_str().unwrap(),
        ],
        "y\n",
    );
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
}

#[test]
fn doctor_fails_a_program_without_the_execute_bit() {
    let w = W::with_store();
    let dir = w.store().join("plugins/kettle-check");
    check_plugin(&dir, "kettle-check", "1.0.0", &[]);
    std::fs::set_permissions(dir.join("check.sh"), std::fs::Permissions::from_mode(0o644)).unwrap();
    w.config("\n[[plugins]]\nid = \"kettle-check\"\n");
    let o = w.rr(&["--project", "kettle", "doctor"], "");
    assert_eq!(o.code, 1, "{}{}", o.stdout, o.stderr);
    assert!(
        o.stdout.contains("fail  program ./check.sh"),
        "{}",
        o.stdout
    );
}

#[test]
fn doctor_exits_4_on_a_user_config_a_run_cannot_load() {
    let w = W::with_store();
    std::fs::write(
        w.root.join("config/precall/config.toml"),
        "unknown = true\n",
    )
    .unwrap();
    let o = w.rr(&["--project", "kettle", "doctor"], "");
    assert_eq!(o.code, 4, "{}{}", o.stdout, o.stderr);
    assert!(o.stderr.contains("unknown"), "{}", o.stderr);
    let none = w.rr(&["doctor"], "");
    assert_eq!(
        none.code, 4,
        "the config is read before the project: {}",
        none.stderr
    );
}

#[test]
fn doctor_runs_fingerprints_in_a_scratch_state_dir() {
    let w = W::with_store();
    let dir = w.store().join("plugins/kettle-notes");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("precall-plugin.toml"),
        "id = \"kettle-notes\"\nname = \"N\"\nversion = \"1.0.0\"\nmin_precall = \"0.1.0\"\nkind = \"collector\"\n\n[collector]\nscope = [\"decisions\"]\nsource_kinds = [\"kettle-note\"]\nfingerprint = [\"./fp.sh\"]\ncommand = [\"./fp.sh\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("fp.sh"),
        "#!/bin/sh\necho \"$PRECALL_STATE_DIR\" > \"$PRECALL_STATE_DIR/seen\"\nls \"$PRECALL_STATE_DIR\" | wc -l\n",
    )
    .unwrap();
    std::fs::set_permissions(dir.join("fp.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    w.config("\n[[plugins]]\nid = \"kettle-notes\"\n");
    for _ in 0..2 {
        let o = w.rr(&["--project", "kettle", "doctor"], "");
        assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    }
    assert!(
        !w.store().join("state").exists(),
        "the run state is untouched"
    );
    let seen = w.store().join("cache/doctor/state/kettle-notes/seen");
    let text = std::fs::read_to_string(&seen).unwrap();
    assert!(text.contains("cache/doctor/state/kettle-notes"), "{text}");
    let out = std::fs::read_to_string(
        w.store()
            .join("cache/doctor/kettle-notes.fingerprint.stdout"),
    )
    .unwrap();
    assert_eq!(out.trim(), "1", "a fresh dir on each doctor run");
}

// ---- review round 2 ----

/// Adds `blobs` (path, mode, content) to the index of the git repo at `dir` and commits them;
/// returns the commit. Plumbing, so paths the working tree could not hold are possible.
fn commit_blobs(dir: &Path, blobs: &[(&str, &str, &str)]) -> String {
    for (path, mode, text) in blobs {
        let o = Command::new("git")
            .args(["hash-object", "-w", "--stdin"])
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write;
                c.stdin.take().unwrap().write_all(text.as_bytes())?;
                c.wait_with_output()
            })
            .unwrap();
        let sha = String::from_utf8(o.stdout).unwrap().trim().to_string();
        git_in(
            dir,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("{mode},{sha},{path}"),
            ],
        );
    }
    git_in(dir, &["commit", "-q", "-m", "blobs"]);
    git_in(dir, &["rev-parse", "HEAD"])
}

fn committed_plugin(dir: &Path) {
    check_plugin(dir, "steam-check", "1.0.0", &[]);
    git_in(dir, &["init", "-q", "-b", "main"]);
    git_in(dir, &["add", "."]);
    git_in(dir, &["commit", "-q", "-m", "v1"]);
}

#[test]
fn plugin_add_refuses_tree_paths_that_collide_by_case() {
    let w = W::new();
    let src = w.root.join("src/steam");
    committed_plugin(&src);
    let head = commit_blobs(
        &src,
        &[
            ("ReadMe.txt", "100644", "first\n"),
            ("readme.txt", "100644", "second\n"),
        ],
    );
    let o = w.rr(
        &["plugin", "add", src.to_str().unwrap(), "--ref", &head],
        "y\n",
    );
    assert_ne!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(o.stderr.contains("readme.txt"), "{}", o.stderr);
    assert!(!w.user_plugins().join("steam-check").exists());
}

#[test]
fn plugin_add_never_writes_through_a_link_from_the_tree() {
    let w = W::new();
    let outside = w.root.join("rec/outside");
    std::fs::create_dir_all(&outside).unwrap();
    let src = w.root.join("src/steam");
    committed_plugin(&src);
    let head = commit_blobs(
        &src,
        &[
            ("Out", "120000", outside.to_str().unwrap()),
            ("out/pwn", "100644", "pwned\n"),
        ],
    );
    let o = w.rr(
        &["plugin", "add", src.to_str().unwrap(), "--ref", &head],
        "y\n",
    );
    assert_ne!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(!outside.join("pwn").exists(), "wrote through a link");
    assert!(!w.user_plugins().join("steam-check").exists());
}

#[test]
fn plugin_trust_rejects_a_relative_repo_as_the_loader_does() {
    let w = W::with_store();
    check_plugin(
        &w.root.join("work/relative/.precall/plugins/lid-check"),
        "lid-check",
        "1.0.0",
        &[],
    );
    std::fs::write(w.store().join("precall.toml"), "repo = \"relative\"\n").unwrap();
    let o = w.rr(
        &[
            "--project",
            "kettle",
            "plugin",
            "trust",
            "./relative/.precall/plugins",
        ],
        "y\n",
    );
    assert_ne!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(o.stderr.contains("is not an absolute path"), "{}", o.stderr);
    assert!(!w.user_config().contains("[[trusted]]"));
}

#[test]
fn concurrent_doctors_keep_their_own_scratch_state() {
    let w = W::with_store();
    let dir = w.store().join("plugins/kettle-notes");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("precall-plugin.toml"),
        "id = \"kettle-notes\"\nname = \"N\"\nversion = \"1.0.0\"\nmin_precall = \"0.1.0\"\nkind = \"collector\"\n\n[collector]\nscope = [\"decisions\"]\nsource_kinds = [\"kettle-note\"]\nfingerprint = [\"./fp.sh\"]\ncommand = [\"./fp.sh\"]\n",
    )
    .unwrap();
    // The first call parks until `go` exists, then checks that its scratch file survived.
    std::fs::write(
        dir.join("fp.sh"),
        "#!/bin/sh\ntouch \"$PRECALL_STATE_DIR/mine.$$\"\ntouch \"$HOME/started\"\nwhile [ ! -e \"$HOME/go\" ]; do sleep 0.05; done\n[ -e \"$PRECALL_STATE_DIR/mine.$$\" ] || { echo 'my scratch file disappeared' >&2; exit 1; }\necho ok\n",
    )
    .unwrap();
    std::fs::set_permissions(dir.join("fp.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    w.config("\n[[plugins]]\nid = \"kettle-notes\"\n");
    let home = w.root.join("home");
    let first = w.spawn(&["--project", "kettle", "doctor"]);
    while !home.join("started").exists() {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let second = w.spawn(&["--project", "kettle", "doctor"]);
    std::thread::sleep(std::time::Duration::from_millis(500));
    std::fs::write(home.join("go"), "").unwrap();
    for c in [first, second] {
        let o = c.wait_with_output().unwrap();
        assert!(
            o.status.success(),
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

#[test]
fn init_waits_for_the_user_config_lock() {
    let w = W::new();
    let cfg_dir = w.root.join("config/precall");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let held = precall::lock::lock_file(&cfg_dir.join("config.toml.lock"), true).unwrap();
    let mut init = w.spawn(&["init", "kettle"]);
    // Unlocked, init finishes well within this window; locked out, it must still be waiting
    // at its end, having written neither the user config nor the store.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < until {
        if let Some(status) = init.try_wait().unwrap() {
            let mut err = String::new();
            use std::io::Read;
            init.stderr
                .take()
                .unwrap()
                .read_to_string(&mut err)
                .unwrap();
            panic!(
                "init finished ({status}) while another command held the user config lock: {err}"
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        !cfg_dir.join("config.toml").exists(),
        "init wrote the user config"
    );
    assert!(!w.store().exists(), "init created the store");
    drop(held);
    let o = init.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(
        w.user_config().contains("name = \"kettle\""),
        "{}",
        w.user_config()
    );
}

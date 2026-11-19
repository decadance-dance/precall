//! The built-in runners (spec §6.8) against fake `claude` and `codex` executables on `PATH`:
//! the argv and stdin they get, what they print, a missing cost, malformed output, the
//! wrap-up turn and the kill at the timeout.

use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

struct T {
    root: PathBuf,
}

impl Drop for T {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Out {
    ok: bool,
    stdout: String,
    stderr: String,
}

impl T {
    fn new(scenario: &str, out: &str) -> T {
        static N: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "rrecall-runner-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["home/agent", "home/.codex", "state", "tmp"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("home/agent/scenario"), scenario).unwrap();
        std::fs::write(root.join("home/agent/out"), out).unwrap();
        std::fs::write(root.join("home/.codex/auth.json"), "{}").unwrap();
        T { root }
    }

    fn call(&self, id: &str, model: &str, timeout: u64, prices: Option<&str>) -> Out {
        self.call_with(id, model, timeout, prices, &[])
    }

    /// `call` with extra environment variables (`PATH` among them replaces the fake one).
    fn call_with(
        &self,
        id: &str,
        model: &str,
        timeout: u64,
        prices: Option<&str>,
        env: &[(&str, String)],
    ) -> Out {
        let o = self
            .spawn_with(id, model, timeout, prices, env)
            .wait_with_output()
            .unwrap();
        Out {
            ok: o.status.success(),
            stdout: String::from_utf8(o.stdout).unwrap(),
            stderr: String::from_utf8(o.stderr).unwrap(),
        }
    }

    /// `call_with` without waiting for the runner, which is the returned child itself.
    fn spawn_with(
        &self,
        id: &str,
        model: &str,
        timeout: u64,
        prices: Option<&str>,
        env: &[(&str, String)],
    ) -> std::process::Child {
        let path = format!(
            "{}/tests/fixtures/bin:{}",
            env!("CARGO_MANIFEST_DIR"),
            std::env::var("PATH").unwrap_or_default()
        );
        // A umask that lets other users read what the runner creates unless it says otherwise.
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "umask 022 && exec \"$0\" builtin \"$1\""])
            .args([env!("CARGO_BIN_EXE_rrecall"), id])
            .env("PATH", path)
            .env("HOME", self.root.join("home"))
            .env("RRECALL_STATE_DIR", self.root.join("state"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("RRECALL_RUN_ID", "r1")
            .env("GH_TOKEN", "must-not-reach-the-agent")
            .env_remove("CODEX_HOME")
            .env_remove("RRECALL_PARAM_PRICES")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(p) = prices {
            cmd.env("RRECALL_PARAM_PRICES", p);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let input = json!({
            "prompt": "PROMPT TEXT, never on the command line",
            "input": {"refs": ["note:a"]},
            "output_schema": {"type": "object", "required": ["texts"]},
            "model": model,
            "timeout": timeout,
            "read_only": true,
        });
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .unwrap();
        child
    }

    /// Gives the temp dir an ACL that every dir and file created in it inherits, readable by
    /// everyone (macOS ACLs are independent of the mode).
    #[cfg(target_os = "macos")]
    fn inheritable_acl(&self) {
        let ok = Command::new("/bin/chmod")
            .args([
                "+a",
                "group:everyone allow list,search,readattr,read,file_inherit,directory_inherit",
            ])
            .arg(self.root.join("tmp"))
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }

    fn rec(&self, name: &str) -> String {
        std::fs::read_to_string(self.root.join("home/agent").join(name))
            .unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    fn argv(&self, name: &str) -> Vec<String> {
        self.rec(name).lines().map(String::from).collect()
    }

    /// The runners' private dir in `TMPDIR`.
    fn private(&self) -> PathBuf {
        let uid = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(&self.root).unwrap());
        self.root.join(format!("tmp/rrecall-{uid}"))
    }

    /// The scratch dir of each call (in the private dir in `TMPDIR`) is gone after it.
    fn no_scratch_left(&self) {
        let names = |p: PathBuf| -> Vec<_> {
            let names = std::fs::read_dir(p).unwrap().flatten();
            names.map(|e| e.file_name()).collect()
        };
        let private = self.private();
        assert_eq!(names(self.root.join("tmp")), [private.file_name().unwrap()]);
        let left = names(private);
        assert!(left.is_empty(), "{left:?}");
    }
}

/// A runner output's `usage`: input, output, cache read, cache write.
fn usage(input: u64, output: u64, read: u64, write: u64) -> Value {
    json!({"input_tokens": input, "output_tokens": output,
        "cache_read_tokens": read, "cache_write_tokens": write})
}

fn json_of(o: &Out) -> Value {
    assert!(o.ok, "stderr: {}", o.stderr);
    serde_json::from_str(&o.stdout).unwrap()
}

fn after<'a>(argv: &'a [String], flag: &str) -> &'a str {
    let i = argv
        .iter()
        .position(|a| a == flag)
        .unwrap_or_else(|| panic!("no {flag} in {argv:?}"));
    &argv[i + 1]
}

fn dead(pid_file: &str) -> bool {
    let pid = pid_file.trim();
    !Command::new("kill")
        .args(["-0", pid])
        .status()
        .unwrap()
        .success()
}

#[test]
fn claude_runs_read_only_with_the_prompt_on_stdin_and_reports_cost_and_model() {
    let t = T::new("ok", r#"{"texts":{"note:a":"A"}}"#);
    let out = json_of(&t.call("claude", "claude-fake-1", 60, None));
    assert_eq!(
        out,
        json!({"output": {"texts": {"note:a": "A"}}, "cost_usd": 0.25, "model": "claude-fake-1",
            "usage": usage(1, 2, 4, 3)})
    );
    let argv = t.argv("claude.argv.1");
    for want in ["-p", "--safe-mode", "--session-id"] {
        assert!(argv.iter().any(|a| a == want), "{want}: {argv:?}");
    }
    assert_eq!(after(&argv, "--output-format"), "stream-json");
    assert!(argv.iter().any(|a| a == "--verbose"));
    assert_eq!(after(&argv, "--tools"), "Read,Grep,Glob");
    assert_eq!(after(&argv, "--allowedTools"), "Read,Grep,Glob");
    assert_eq!(after(&argv, "--permission-mode"), "dontAsk");
    assert_eq!(after(&argv, "--model"), "claude-fake-1");
    assert!(
        !argv.iter().any(|a| a == "--effort"),
        "no effort unless set"
    );
    assert_eq!(
        after(&argv, "--json-schema"),
        r#"{"required":["texts"],"type":"object"}"#
    );
    assert!(!argv.iter().any(|a| a.contains("PROMPT TEXT")));
    assert_eq!(
        t.rec("claude.stdin.1"),
        "PROMPT TEXT, never on the command line"
    );
    let env = t.rec("claude.env.1");
    assert!(
        !env.contains("GH_TOKEN") && !env.contains("RRECALL_"),
        "{env}"
    );
    assert!(t.rec("claude.cwd.1").contains("/rrecall-runner-"));
    t.no_scratch_left();
}

#[test]
fn claude_without_a_cost_is_estimated_from_the_price_table_or_fails() {
    let t = T::new("nocost", r#"{"texts":{}}"#);
    let prices = r#"{"claude-fake-1":{"input":2,"cached_input":0,"output":0}}"#;
    let out = json_of(&t.call("claude", "default", 60, Some(prices)));
    assert_eq!(out["cost_usd"], 2.0);
    assert_eq!(out["model"], "claude-fake-1");
    assert!(!t.argv("claude.argv.1").iter().any(|a| a == "--model"));
    let o = t.call("claude", "default", 60, None);
    assert!(!o.ok);
    assert!(
        o.stderr.contains("`claude-fake-1`") && o.stderr.contains("`prices`"),
        "{}",
        o.stderr
    );
}

#[test]
fn claude_malformed_output_or_an_error_fails_the_call() {
    let t = T::new("malformed", "{}");
    let o = t.call("claude", "m", 60, None);
    assert!(
        !o.ok && o.stderr.contains("not one JSON document"),
        "{}",
        o.stderr
    );
    std::fs::write(t.root.join("home/agent/scenario"), "error").unwrap();
    let o = t.call("claude", "m", 60, None);
    assert!(!o.ok && o.stderr.contains("Not logged in"), "{}", o.stderr);
    t.no_scratch_left();
}

/// A timeout with a wrap-up turn: a main turn of 12s and a wrap-up of 1s (a fifth of it, minus
/// 2s), long enough for the fake agent to start and answer under a loaded test machine.
const WRAP_TIMEOUT: u64 = 15;

#[test]
fn claude_near_the_timeout_gets_a_wrap_up_turn_in_the_same_session() {
    let t = T::new("sleep", r#"{"texts":{}}"#);
    let free = r#"{"claude-fake-1":{"input":0,"cached_input":0,"output":0}}"#;
    let out = json_of(&t.call("claude", "m", WRAP_TIMEOUT, Some(free)));
    assert_eq!(out["cost_usd"], 0.1);
    let (first, second) = (t.argv("claude.argv.1"), t.argv("claude.argv.2"));
    assert_eq!(after(&second, "--resume"), after(&first, "--session-id"));
    assert!(t.rec("claude.stdin.2").starts_with("Time is up."));
    assert!(
        dead(&t.rec("claude.pid")),
        "the first turn's process tree was killed"
    );
}

#[test]
fn claude_effort_goes_after_the_model_on_both_turns_and_empty_means_unset() {
    let t = T::new("sleep", r#"{"texts":{}}"#);
    let free = r#"{"claude-fake-1":{"input":0,"cached_input":0,"output":0}}"#;
    let o = t.call_with(
        "claude",
        "m",
        WRAP_TIMEOUT,
        Some(free),
        &[("RRECALL_PARAM_EFFORT", "high".into())],
    );
    assert!(o.ok, "{}", o.stderr);
    for n in [1, 2] {
        let argv = t.argv(&format!("claude.argv.{n}"));
        let at = argv.iter().position(|a| a == "--effort").expect("--effort");
        assert_eq!(
            argv[at - 2..at + 2],
            ["--model", "m", "--effort", "high"],
            "turn {n}"
        );
    }
    let t = T::new("ok", r#"{"texts":{}}"#);
    let o = t.call_with(
        "claude",
        "m",
        60,
        None,
        &[("RRECALL_PARAM_EFFORT", "".into())],
    );
    assert!(o.ok, "{}", o.stderr);
    assert!(!t.argv("claude.argv.1").iter().any(|a| a == "--effort"));
}

#[test]
fn claude_that_never_answers_is_killed_at_the_timeout() {
    let t = T::new("sleep-always", "{}");
    let started = std::time::Instant::now();
    let o = t.call("claude", "m", WRAP_TIMEOUT, None);
    assert!(!o.ok && o.stderr.contains("timed out"), "{}", o.stderr);
    // Killed, not left to finish: each fake turn would sleep 30s.
    let took = started.elapsed();
    assert!(took < std::time::Duration::from_secs(25), "{took:?}");
    assert!(dead(&t.rec("claude.pid.1")));
    // A wrap-up turn killed before it started its sleep wrote no pid and left nothing behind.
    let second = std::fs::read_to_string(t.root.join("home/agent/claude.pid.2"));
    assert!(second.is_err() || dead(&second.unwrap()));
    t.no_scratch_left();
}

#[test]
fn codex_runs_read_only_in_a_clean_home_and_prices_its_tokens() {
    let t = T::new("ok", r#"{"texts":{"note:a":null}}"#);
    let prices = r#"{"codex-fake-1":{"input":1,"cached_input":0.1,"output":10}}"#;
    let out = json_of(&t.call("codex", "default", 60, Some(prices)));
    // From the session log: 0.3M uncached input, 1M cache reads, 0.2M cache writes (priced as
    // input), 0.1M output.
    assert_eq!(out["model"], "codex-fake-1");
    assert!(
        (out["cost_usd"].as_f64().unwrap() - 1.6).abs() < 1e-9,
        "{out}"
    );
    assert_eq!(out["usage"], usage(300_000, 100_000, 1_000_000, 200_000));
    assert_eq!(out["output"], json!({"texts": {"note:a": null}}));
    let argv = t.argv("codex.argv.1");
    assert_eq!(
        &argv[..5],
        ["exec", "--json", "-s", "read-only", "--skip-git-repo-check"]
    );
    let dir = after(&argv, "-C");
    assert!(dir.contains("/rrecall-runner-"), "{dir}");
    assert_eq!(after(&argv, "-o"), format!("{dir}/reply.txt"));
    assert_eq!(argv.last().map(String::as_str), Some("-"));
    assert!(!argv.iter().any(|a| a == "-m" || a.contains("PROMPT TEXT")));
    assert_eq!(
        t.rec("codex.stdin.1"),
        "PROMPT TEXT, never on the command line"
    );
    let env = t.rec("codex.env.1");
    assert!(
        env.contains(&format!("CODEX_HOME={dir}/codex-home")),
        "{env}"
    );
    assert!(
        !env.contains("GH_TOKEN") && !env.contains("RRECALL_"),
        "{env}"
    );
    t.no_scratch_left();
}

#[test]
fn codex_without_a_session_log_counts_its_turn_events_and_the_requested_model() {
    let t = T::new("nolog", r#"{"texts":{}}"#);
    let prices = r#"{"m7":{"input":1,"cached_input":0,"output":0}}"#;
    let out = json_of(&t.call("codex", "m7", 60, Some(prices)));
    assert_eq!(
        (out["model"].as_str(), out["cost_usd"].as_f64()),
        (Some("m7"), Some(1.0))
    );
    assert_eq!(out["usage"], usage(1_000_000, 0, 0, 0));
    assert_eq!(after(&t.argv("codex.argv.1"), "-m"), "m7");
    let o = t.call("codex", "m7", 60, None);
    assert!(!o.ok && o.stderr.contains("`m7`"), "{}", o.stderr);
}

#[test]
fn codex_without_a_session_log_counts_a_resumed_threads_last_running_total_once() {
    let t = T::new("nolog-sleep", r#"{"texts":{}}"#);
    let prices = r#"{"m":{"input":1,"cached_input":1,"output":1}}"#;
    let out = json_of(&t.call("codex", "m", WRAP_TIMEOUT, Some(prices)));
    // The resumed turn's total already holds the first turn's 100 input and 20 output.
    assert_eq!(out["usage"], usage(21, 21, 84, 0));
    assert!(
        (out["cost_usd"].as_f64().unwrap() - 0.000126).abs() < 1e-12,
        "{out}"
    );
}

#[test]
fn codex_near_the_timeout_resumes_its_thread_with_a_wrap_up() {
    let t = T::new("sleep", r#"{"texts":{}}"#);
    let prices = r#"{"codex-fake-1":{"input":0,"cached_input":0,"output":0}}"#;
    let out = json_of(&t.call("codex", "default", WRAP_TIMEOUT, Some(prices)));
    assert_eq!(out["model"], "codex-fake-1");
    let second = t.argv("codex.argv.2");
    assert_eq!(&second[second.len() - 3..], ["resume", "th1", "-"]);
    assert!(t.rec("codex.stdin.2").starts_with("Time is up."));
    assert!(dead(&t.rec("codex.pid")));
}

#[test]
fn codex_fails_without_a_login_or_when_it_crashes() {
    let t = T::new("crash", "{}");
    let o = t.call("codex", "m", 60, None);
    assert!(
        !o.ok && o.stderr.contains("codex exit status: 2"),
        "{}",
        o.stderr
    );
    std::fs::remove_file(t.root.join("home/.codex/auth.json")).unwrap();
    let o = t.call("codex", "m", 60, None);
    assert!(
        !o.ok && o.stderr.contains("Codex login not found"),
        "{}",
        o.stderr
    );
    t.no_scratch_left();
}

/// The agent's working dir: outside the store and outside any git repository, so it can
/// neither find the map by a relative path nor load a project's instruction files.
fn isolated(cwd: &str, root: &std::path::Path) {
    let cwd = std::path::Path::new(cwd.trim());
    let state = root.join("state").canonicalize().unwrap();
    assert!(
        !cwd.starts_with(&state),
        "{} is in the store",
        cwd.display()
    );
    for a in cwd.ancestors() {
        assert!(
            !a.join(".git").exists(),
            "{} is inside the git repository {}",
            cwd.display(),
            a.display()
        );
    }
}

#[test]
fn the_agent_runs_outside_the_store_and_any_git_repository() {
    let t = T::new("ok", r#"{"texts":{}}"#);
    json_of(&t.call("claude", "m", 60, None));
    isolated(&t.rec("claude.cwd.1"), &t.root);
    let prices = r#"{"codex-fake-1":{"input":0,"cached_input":0,"output":0}}"#;
    json_of(&t.call("codex", "default", 60, Some(prices)));
    let argv = t.argv("codex.argv.1");
    isolated(after(&argv, "-C"), &t.root);
    // A temp dir inside a git repository is refused rather than used.
    let repo = t.root.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let o = t.call_with(
        "claude",
        "m",
        60,
        None,
        &[("TMPDIR", repo.to_string_lossy().into())],
    );
    assert!(!o.ok && o.stderr.contains("git repository"), "{}", o.stderr);
    assert_eq!(
        t.rec("claude.calls").lines().count(),
        1,
        "no agent was started"
    );
}

#[test]
fn claude_wrap_up_charges_the_killed_turn_too() {
    let t = T::new("sleep", r#"{"texts":{}}"#);
    // The killed turn streamed 1M input tokens of one message (twice, one per content block).
    let prices = r#"{"claude-fake-1":{"input":0.5,"cached_input":0,"output":0}}"#;
    let out = json_of(&t.call("claude", "m", WRAP_TIMEOUT, Some(prices)));
    assert!(
        (out["cost_usd"].as_f64().unwrap() - 0.6).abs() < 1e-9,
        "{out}"
    );
    // The killed turn's streamed usage plus the wrap-up's result.
    assert_eq!(out["usage"], usage(1_000_001, 2, 4, 3));
    let o = t.call("claude", "m", WRAP_TIMEOUT, None);
    assert!(
        !o.ok && o.stderr.contains("`claude-fake-1`") && o.stderr.contains("`prices`"),
        "{}",
        o.stderr
    );
}

#[test]
fn an_agent_that_reports_neither_cost_nor_usage_fails_the_call() {
    let t = T::new("nousage", r#"{"texts":{}}"#);
    let prices = r#"{"claude-fake-1":{"input":1,"cached_input":1,"output":1}}"#;
    let o = t.call("claude", "m", 60, Some(prices));
    assert!(!o.ok && o.stderr.contains("usage"), "{}", o.stderr);
}

#[test]
fn a_key_twice_in_an_agent_reply_fails_the_call() {
    let t = T::new("ok", r#"{"texts":false,"texts":{}}"#);
    let o = t.call("claude", "m", 60, None);
    assert!(!o.ok && o.stderr.contains("duplicate key"), "{}", o.stderr);
    let prices = r#"{"codex-fake-1":{"input":0,"cached_input":0,"output":0}}"#;
    let o = t.call("codex", "default", 60, Some(prices));
    assert!(!o.ok && o.stderr.contains("duplicate key"), "{}", o.stderr);
}

#[test]
fn an_agent_is_never_started_through_a_shell() {
    let t = T::new("ok", "{}");
    let bin = t.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(&claude, "printf '%s\\n' '{\"type\":\"result\",\"is_error\":false,\"structured_output\":{},\"total_cost_usd\":1}'\n").unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let o = t.call_with("claude", "m", 60, None, &[("PATH", path)]);
    assert!(
        !o.ok && o.stderr.contains("through a shell"),
        "{}",
        o.stderr
    );
}

#[test]
fn a_timeout_too_short_for_a_wrap_up_says_so() {
    let t = T::new("sleep", "{}");
    let o = t.call("claude", "m", 1, None);
    assert!(!o.ok && o.stderr.contains("no wrap-up"), "{}", o.stderr);
}

#[test]
fn a_killed_turn_that_reported_its_cost_is_charged_that_cost() {
    let t = T::new("result-sleep", r#"{"texts":{}}"#);
    let out = json_of(&t.call("claude", "m", 15, None));
    assert!(
        (out["cost_usd"].as_f64().unwrap() - 0.75).abs() < 1e-9,
        "{out}"
    );
    assert_eq!(out["usage"], usage(2, 4, 8, 6));
}

#[test]
fn a_record_torn_by_the_kill_does_not_lose_the_wrap_up() {
    let t = T::new("torn", r#"{"texts":{}}"#);
    let prices = r#"{"claude-fake-1":{"input":0.5,"cached_input":0,"output":0}}"#;
    let out = json_of(&t.call("claude", "m", WRAP_TIMEOUT, Some(prices)));
    assert!(
        (out["cost_usd"].as_f64().unwrap() - 0.6).abs() < 1e-9,
        "{out}"
    );
}

#[test]
fn the_scratch_dir_and_its_files_are_owner_only() {
    let t = T::new("perms", r#"{"texts":{}}"#);
    #[cfg(target_os = "macos")]
    t.inheritable_acl();
    json_of(&t.call("claude", "m", 60, None));
    // The private dir and the scratch dir in it: no ACL flag and no ACL entry, also under a temp
    // dir whose ACL everything inherits.
    assert_eq!(
        t.rec("claude.perms"),
        "drwx------\ndrwx------\n-rw-------\nacl=0\n"
    );
}

#[test]
fn a_call_removes_only_the_scratch_dirs_of_calls_whose_runner_is_gone() {
    // Call 1 hangs in its agent; calls 2 and 3 share its TMPDIR.
    let t = T::new("sleep-always", "{}");
    let mut first = t.spawn_with("claude", "m", 60, None, &[]);
    let cwd = t.root.join("home/agent/claude.cwd.1");
    for _ in 0..200 {
        if cwd.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let running = PathBuf::from(std::fs::read_to_string(&cwd).unwrap().trim());
    let private = std::fs::canonicalize(t.private()).unwrap();
    assert_eq!(running.parent(), Some(private.as_path()));
    // Look-alikes with a dead pid, in TMPDIR and in the private dir, and a link with the exact
    // name form.
    let mut done = Command::new("true").spawn().unwrap();
    done.wait().unwrap();
    let dead = done.id();
    let tmp = t.root.join("tmp");
    let looks = [
        tmp.join(format!("rrecall-agent-{dead}-project-notes")),
        private.join(format!("agent-{dead}-project-notes")),
        private.join(format!("agent-{dead}-0a1b2c3d-0000-4000-8000-00000000000g")),
    ];
    for d in &looks {
        std::fs::create_dir_all(d).unwrap();
        std::fs::write(d.join("note"), "keep").unwrap();
    }
    let target = t.root.join("keep");
    std::fs::create_dir_all(&target).unwrap();
    let link = private.join(format!("agent-{dead}-0a1b2c3d-0000-4000-8000-000000000000"));
    std::os::unix::fs::symlink(&target, link).unwrap();
    let other = T::new("ok", r#"{"texts":{}}"#);
    let shared = [("TMPDIR", tmp.to_string_lossy().into())];
    assert!(other.call_with("claude", "m", 60, None, &shared).ok);
    assert!(running.exists());
    // Killed, the runner leaves its dir behind; the next call removes it, and nothing else.
    first.kill().unwrap();
    first.wait().unwrap();
    if let Ok(p) = std::fs::read_to_string(t.root.join("home/agent/claude.pid.1")) {
        let _ = Command::new("kill").arg(p.trim()).status();
    }
    assert!(running.exists());
    assert!(other.call_with("claude", "m", 60, None, &shared).ok);
    assert!(!running.exists());
    for d in &looks {
        assert!(d.join("note").exists(), "{}", d.display());
    }
    assert!(target.exists());
}

#[test]
fn a_private_runner_dir_that_others_could_reach_is_refused() {
    let t = T::new("ok", r#"{"texts":{}}"#);
    let private = t.private();
    std::fs::create_dir(&private).unwrap();
    std::fs::set_permissions(
        &private,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let o = t.call("claude", "m", 60, None);
    assert!(
        !o.ok && o.stderr.contains(&private.display().to_string()),
        "{}",
        o.stderr
    );
    assert!(!t.root.join("home/agent/claude.calls").exists());
    // Nor a link in its place.
    std::fs::remove_dir(&private).unwrap();
    std::os::unix::fs::symlink(t.root.join("home"), &private).unwrap();
    let o = t.call("claude", "m", 60, None);
    assert!(
        !o.ok && o.stderr.contains(&private.display().to_string()),
        "{}",
        o.stderr
    );
    assert!(!t.root.join("home/agent/claude.calls").exists());
}

#[test]
fn a_key_twice_in_codex_usage_fails_the_call() {
    let t = T::new("dupusage", r#"{"texts":{}}"#);
    let prices = r#"{"m":{"input":1,"cached_input":0,"output":0}}"#;
    let o = t.call("codex", "m", 60, Some(prices));
    assert!(!o.ok && o.stderr.contains("duplicate key"), "{}", o.stderr);
}

#[test]
fn claude_with_connectors_runs_in_a_fresh_config_dir_without_hooks_and_allows_only_named_tools() {
    let t = T::new("config", r#"{"texts":{"note:a":"A"}}"#);
    let tools =
        r#"["mcp__claude_ai_Slack__slack_read_channel","mcp__claude_ai_Slack__slack_read_thread"]"#;
    // The user's own config dir, with a login file and a CLAUDE.md that must not load.
    let real = t.root.join("home/real-config");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join(".credentials.json"), r#"{"token":"t1"}"#).unwrap();
    std::fs::write(real.join("CLAUDE.md"), "user instructions").unwrap();
    #[cfg(target_os = "macos")]
    t.inheritable_acl();
    let o = t.call_with(
        "claude",
        "default",
        60,
        None,
        &[
            ("RRECALL_PARAM_CONNECTORS", "true".into()),
            ("RRECALL_PARAM_TOOLS", tools.into()),
            ("CLAUDE_CONFIG_DIR", real.to_string_lossy().into()),
        ],
    );
    assert!(o.ok, "{}", o.stderr);
    let argv = t.argv("claude.argv.1");
    assert!(!argv.iter().any(|a| a == "--safe-mode"), "{argv:?}");
    assert_eq!(after(&argv, "--setting-sources"), "user");
    assert!(argv.iter().any(|a| a == "--disable-slash-commands"));
    assert_eq!(after(&argv, "--settings"), r#"{"disableAllHooks":true}"#);
    // ToolSearch loads the deferred schemas of the connectors' tools.
    assert_eq!(after(&argv, "--tools"), "Read,Grep,Glob,ToolSearch");
    assert_eq!(
        after(&argv, "--allowedTools"),
        "Read,Grep,Glob,ToolSearch,mcp__claude_ai_Slack__slack_read_channel,mcp__claude_ai_Slack__slack_read_thread"
    );
    assert_eq!(after(&argv, "--permission-mode"), "dontAsk");
    let env = t.rec("claude.env.1");
    for want in [
        "ENABLE_TOOL_SEARCH=true",
        "MCP_CONNECTION_NONBLOCKING=false",
        "CLAUDE_CODE_MCP_STARTUP_WAIT_MS=30000",
        "CLAUDE_CODE_DISABLE_CLAUDE_MDS=1",
    ] {
        assert!(env.lines().any(|l| l == want), "{want}: {env}");
    }
    // A fresh owner-only config dir in the call's scratch dir; the login stays the user's.
    let cwd = t.rec("claude.cwd.1");
    let config = env
        .lines()
        .find_map(|l| l.strip_prefix("CLAUDE_CONFIG_DIR="))
        .unwrap();
    assert_eq!(config, format!("{}/claude-config", cwd.trim()), "{env}");
    let secure = format!("CLAUDE_SECURESTORAGE_CONFIG_DIR={}", real.display());
    assert!(env.lines().any(|l| l == secure), "{env}");
    // Owner-only with no ACL, and empty: Claude Code reads (and refreshes) the login in
    // CLAUDE_SECURESTORAGE_CONFIG_DIR, so nothing is copied or linked into it.
    assert_eq!(t.rec("claude.config"), "drwx------\nacl=0\n");
    t.no_scratch_left();
    assert_eq!(
        std::fs::read_to_string(real.join(".credentials.json")).unwrap(),
        r#"{"token":"t1"}"#
    );
}

#[test]
fn claude_without_connectors_keeps_safe_mode_and_no_tool_search() {
    let t = T::new("ok", r#"{"texts":{"note:a":"A"}}"#);
    let o = t.call_with(
        "claude",
        "default",
        60,
        None,
        &[
            ("RRECALL_PARAM_CONNECTORS", "false".into()),
            ("ENABLE_TOOL_SEARCH", "true".into()),
        ],
    );
    assert!(o.ok, "{}", o.stderr);
    let argv = t.argv("claude.argv.1");
    assert!(argv.iter().any(|a| a == "--safe-mode"), "{argv:?}");
    assert!(
        !argv
            .iter()
            .any(|a| a == "--setting-sources" || a == "--settings")
    );
    assert!(!t.rec("claude.env.1").contains("ENABLE_TOOL_SEARCH"));
}

#[test]
fn claude_tools_must_be_exact_mcp_tool_names_and_need_connectors() {
    for (connectors, tools) in [
        ("true", r#"["mcp__claude_ai_Slack"]"#),
        ("true", r#"["mcp__claude_ai_Slack__*"]"#),
        ("true", r#"["Bash"]"#),
        ("true", r#"["mcp__a__b,Bash"]"#),
        ("true", r#"["mcp__a__b(x)"]"#),
        ("true", r#"["mcp____b"]"#),
        ("true", r#""mcp__a__b""#),
        ("false", r#"["mcp__claude_ai_Slack__slack_read_channel"]"#),
    ] {
        let t = T::new("ok", "{}");
        let o = t.call_with(
            "claude",
            "default",
            60,
            None,
            &[
                ("RRECALL_PARAM_CONNECTORS", connectors.into()),
                ("RRECALL_PARAM_TOOLS", tools.into()),
            ],
        );
        assert!(!o.ok, "{connectors} {tools}");
        assert!(o.stderr.contains("param `"), "{tools}: {}", o.stderr);
        assert!(!t.root.join("home/agent/claude.calls").exists(), "{tools}");
    }
}

#[test]
fn a_relative_claude_config_dir_reaches_the_agent_as_an_absolute_path() {
    let t = T::new("ok", r#"{"texts":{}}"#);
    let o = t.call_with(
        "claude",
        "m",
        60,
        None,
        &[
            ("RRECALL_PARAM_CONNECTORS", "true".into()),
            ("CLAUDE_CONFIG_DIR", "rel/config".into()),
        ],
    );
    assert!(o.ok, "{}", o.stderr);
    // The runner runs in the test's working dir, the crate root.
    let want = format!(
        "CLAUDE_SECURESTORAGE_CONFIG_DIR={}/rel/config",
        env!("CARGO_MANIFEST_DIR")
    );
    assert!(t.rec("claude.env.1").lines().any(|l| l == want));
}

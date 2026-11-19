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
        for d in ["home/agent", "home/.codex", "state"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("home/agent/scenario"), scenario).unwrap();
        std::fs::write(root.join("home/agent/out"), out).unwrap();
        std::fs::write(root.join("home/.codex/auth.json"), "{}").unwrap();
        T { root }
    }

    fn call(&self, id: &str, model: &str, timeout: u64, prices: Option<&str>) -> Out {
        let path = format!(
            "{}/tests/fixtures/bin:{}",
            env!("CARGO_MANIFEST_DIR"),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rrecall"));
        cmd.args(["builtin", id])
            .env("PATH", path)
            .env("HOME", self.root.join("home"))
            .env("RRECALL_STATE_DIR", self.root.join("state"))
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
        let o = child.wait_with_output().unwrap();
        Out {
            ok: o.status.success(),
            stdout: String::from_utf8(o.stdout).unwrap(),
            stderr: String::from_utf8(o.stderr).unwrap(),
        }
    }

    fn rec(&self, name: &str) -> String {
        std::fs::read_to_string(self.root.join("home/agent").join(name))
            .unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    fn argv(&self, name: &str) -> Vec<String> {
        self.rec(name).lines().map(String::from).collect()
    }

    /// The scratch dir of each call is gone after it.
    fn no_scratch_left(&self) {
        let left: Vec<_> = std::fs::read_dir(self.root.join("state"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }
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
        json!({"output": {"texts": {"note:a": "A"}}, "cost_usd": 0.25, "model": "claude-fake-1"})
    );
    let argv = t.argv("claude.argv.1");
    for want in ["-p", "--safe-mode", "--session-id"] {
        assert!(argv.iter().any(|a| a == want), "{want}: {argv:?}");
    }
    assert_eq!(after(&argv, "--output-format"), "json");
    assert_eq!(after(&argv, "--tools"), "Read,Grep,Glob");
    assert_eq!(after(&argv, "--allowedTools"), "Read,Grep,Glob");
    assert_eq!(after(&argv, "--permission-mode"), "dontAsk");
    assert_eq!(after(&argv, "--model"), "claude-fake-1");
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
    assert!(t.rec("claude.cwd.1").contains("/state/call-"));
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
    assert!(!o.ok && o.stderr.contains("no JSON result"), "{}", o.stderr);
    std::fs::write(t.root.join("home/agent/scenario"), "error").unwrap();
    let o = t.call("claude", "m", 60, None);
    assert!(!o.ok && o.stderr.contains("Not logged in"), "{}", o.stderr);
    t.no_scratch_left();
}

// A timeout of 11s: a main turn of 8.8s and a wrap-up of 0.2s (a fifth of it, minus 2s).
#[test]
fn claude_near_the_timeout_gets_a_wrap_up_turn_in_the_same_session() {
    let t = T::new("sleep", r#"{"texts":{}}"#);
    let out = json_of(&t.call("claude", "m", 11, None));
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
fn claude_that_never_answers_is_killed_at_the_timeout() {
    let t = T::new("sleep-always", "{}");
    let started = std::time::Instant::now();
    let o = t.call("claude", "m", 11, None);
    assert!(!o.ok && o.stderr.contains("timed out"), "{}", o.stderr);
    assert!(started.elapsed() < std::time::Duration::from_secs(11));
    assert!(dead(&t.rec("claude.pid.1")) && dead(&t.rec("claude.pid.2")));
    t.no_scratch_left();
}

#[test]
fn codex_runs_read_only_in_a_clean_home_and_prices_its_tokens() {
    let t = T::new("ok", r#"{"texts":{"note:a":null}}"#);
    let prices = r#"{"codex-fake-1":{"input":1,"cached_input":0.1,"output":10}}"#;
    let out = json_of(&t.call("codex", "default", 60, Some(prices)));
    // From the session log: 0.5M uncached input, 1M cached, 0.1M output.
    assert_eq!(out["model"], "codex-fake-1");
    assert!(
        (out["cost_usd"].as_f64().unwrap() - 1.6).abs() < 1e-9,
        "{out}"
    );
    assert_eq!(out["output"], json!({"texts": {"note:a": null}}));
    let argv = t.argv("codex.argv.1");
    assert_eq!(
        &argv[..5],
        ["exec", "--json", "-s", "read-only", "--skip-git-repo-check"]
    );
    let dir = after(&argv, "-C");
    assert!(dir.contains("/state/call-"), "{dir}");
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
    assert_eq!(after(&t.argv("codex.argv.1"), "-m"), "m7");
    let o = t.call("codex", "m7", 60, None);
    assert!(!o.ok && o.stderr.contains("`m7`"), "{}", o.stderr);
}

#[test]
fn codex_near_the_timeout_resumes_its_thread_with_a_wrap_up() {
    let t = T::new("sleep", r#"{"texts":{}}"#);
    let prices = r#"{"codex-fake-1":{"input":0,"cached_input":0,"output":0}}"#;
    let out = json_of(&t.call("codex", "default", 11, Some(prices)));
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

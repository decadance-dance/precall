//! The built-in runners of spec §10, `claude` and `codex` (§6.8). Each ships as a manifest
//! inside the binary and runs as `rrecall builtin <id>`: it reads the runner input on stdin,
//! runs `claude -p` or `codex exec` read-only with the prompt on stdin, and prints
//! `{ "output", "cost_usd", "model" }`. Adapted from the headless runner of rreview (same
//! author, same license).

use crate::error;
use crate::process::{self, TimedOut};
use crate::wire::RunnerInput;
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

/// The built-in manifests. `rrecall` as the program stands for the running binary.
pub const MANIFESTS: [&str; 2] = [
    r#"id = "claude"
name = "Claude Code, headless and read-only (claude -p)"
version = "0.1.0"
min_rrecall = "0.1.0"
kind = "runner"
requires = ["claude"]

[runner]
command = ["rrecall", "builtin", "claude"]

[params]
prices = {}
"#,
    r#"id = "codex"
name = "Codex, headless and read-only (codex exec)"
version = "0.1.0"
min_rrecall = "0.1.0"
kind = "runner"
requires = ["codex"]

[runner]
command = ["rrecall", "builtin", "codex"]

[params]
prices = {}
"#,
];

/// What an agent inherits from the runner's environment; every other variable, such as the
/// `RRECALL_*` ones or a `GH_TOKEN`, is dropped.
const INHERIT: &[&str] = &[
    // Run at all, find the user's login (the macOS keychain).
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    // Reach the API from behind a proxy or a private CA.
    "HTTPS_PROXY",
    "https_proxy",
    "NO_PROXY",
    "no_proxy",
    "NODE_EXTRA_CA_CERTS",
    "SSL_CERT_FILE",
];

/// Claude's own login: its config dir or a `claude setup-token` OAuth token. Codex needs none:
/// its login is the `auth.json` its run-scoped home links to.
const CLAUDE_INHERIT: &[&str] = &["CLAUDE_CONFIG_DIR", "CLAUDE_CODE_OAUTH_TOKEN"];

/// The only tools a Claude runner call has: it reads, it does not run commands or write.
const TOOLS: &str = "Read,Grep,Glob";

const WRAP_UP: &str = "Time is up. Stop now and reply with only the JSON object described in the \
first message, covering what you have so far.";
/// The wrap-up turn gets a fifth of the caller's timeout, at most this.
const WRAP_UP_MAX: Duration = Duration::from_secs(180);
/// Kept free at the end, so the runner answers before the core kills it at the timeout.
const MARGIN: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Agent {
    Claude,
    Codex,
}

/// Runs built-in runner `id` on the runner input `stdin` and returns its output document.
pub fn run(id: &str, stdin: &str) -> Result<String> {
    let agent = match id {
        "claude" => Agent::Claude,
        "codex" => Agent::Codex,
        _ => return Err(error::usage(format!("no built-in runner `{id}`"))),
    };
    // serde's message can quote the input, which holds source text: say where only.
    let input: RunnerInput = serde_json::from_str(stdin).map_err(|e| {
        anyhow!(
            "the runner input does not decode at line {} column {}",
            e.line(),
            e.column()
        )
    })?;
    if !input.read_only {
        bail!("the built-in runners run read-only only");
    }
    let prices = prices_from_env()?;
    let state = std::env::var_os("RRECALL_STATE_DIR").context("RRECALL_STATE_DIR is not set")?;
    let scratch = Scratch::create(Path::new(&state))?;
    let dir = &scratch.0;
    let launch = match agent {
        Agent::Claude => claude_launch(&input, dir)?,
        Agent::Codex => codex_launch(&input, dir)?,
    };
    let session = uuid_v4();
    let (main, wrap) = split(Duration::from_secs(input.timeout));
    let mut turns = vec![dir.join("turn1.stdout")];
    let start = match agent {
        Agent::Claude => vec!["--session-id".into(), session.clone()],
        Agent::Codex => vec!["-".into()],
    };
    let status = match launch.turn(&start, &input.prompt, &turns[0], main) {
        Err(e) if e.is::<TimedOut>() && !wrap.is_zero() => {
            let resume = match agent {
                Agent::Claude => vec!["--resume".into(), session],
                Agent::Codex => {
                    let events = fs::read_to_string(&turns[0]).unwrap_or_default();
                    let thread = codex_thread(&events)
                        .with_context(|| format!("{e}; no codex thread to resume"))?;
                    vec!["resume".into(), thread, "-".into()]
                }
            };
            turns.push(dir.join("turn2.stdout"));
            launch
                .turn(&resume, WRAP_UP, &turns[1], wrap)
                .with_context(|| format!("{e}; wrap-up turn"))?
        }
        r => r?,
    };
    let last = turns.last().map(fs::read_to_string).transpose()?;
    let (output, cost, model) = match agent {
        Agent::Claude => {
            claude_reply(status, last.as_deref().unwrap_or(""), &input.model, &prices)?
        }
        Agent::Codex => {
            if !status.success() {
                bail!("codex {status}");
            }
            let reply = fs::read_to_string(dir.join("reply.txt"))
                .context("codex wrote no final message")?;
            let events: Vec<String> = turns
                .iter()
                .map(|t| fs::read_to_string(t).unwrap_or_default())
                .collect();
            let (tokens, model) = codex_usage(&dir.join("codex-home"), &events, &input.model)?;
            (
                parse_reply(&reply)?,
                estimate(&prices, &model, tokens)?,
                model,
            )
        }
    };
    Ok(json!({"output": output, "cost_usd": cost, "model": model}).to_string() + "\n")
}

/// The main turn's deadline and the wrap-up turn's.
fn split(timeout: Duration) -> (Duration, Duration) {
    let w = (timeout / 5).min(WRAP_UP_MAX);
    (timeout - w, w.saturating_sub(MARGIN))
}

/// One agent process: program, argument vector (without the per-turn session args), the
/// variables it inherits, extra env and working directory.
#[derive(Debug)]
struct Launch {
    program: &'static str,
    args: Vec<String>,
    inherit: Vec<&'static str>,
    env: Vec<(String, String)>,
    cwd: PathBuf,
}

impl Launch {
    /// Runs one turn: `stdin` as the prompt, stdout into `out`, stderr into the runner's own
    /// (which the core keeps, redacted). The agent leads its own process group, so a deadline
    /// kills everything it started.
    // limit: the core kills the runner's group at the caller's timeout, not the agent's; the
    // runner's own deadlines end before it (MARGIN), so only a runner killed early leaves one.
    fn turn(
        &self,
        extra: &[String],
        stdin: &str,
        out: &Path,
        timeout: Duration,
    ) -> Result<ExitStatus> {
        let mut cmd = Command::new(self.program);
        cmd.args(&self.args)
            .args(extra)
            .env_clear()
            .envs(
                std::env::vars_os()
                    .filter(|(k, _)| k.to_str().is_some_and(|k| self.inherit.contains(&k))),
            )
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(File::create(out).with_context(|| format!("{}", out.display()))?)
            .stderr(Stdio::inherit())
            .process_group(0);
        process::wait(&mut cmd, self.program, stdin.as_bytes().to_vec(), timeout)
    }
}

/// A model name goes on the command line; one that starts with `-` would read as a flag.
fn model_args(flag: &str, model: &str) -> Result<Vec<String>> {
    if model == "default" {
        return Ok(vec![]);
    }
    if model.is_empty() || model.starts_with('-') {
        bail!("model `{model}` is not a model name");
    }
    Ok(vec![flag.into(), model.into()])
}

/// `claude -p --safe-mode --output-format json`, tools limited to reading, every other
/// permission refused without a prompt nobody would answer, and the output schema enforced.
/// `--safe-mode` drops the user's CLAUDE.md, skills, plugins, hooks and MCP servers.
fn claude_launch(input: &RunnerInput, dir: &Path) -> Result<Launch> {
    let mut args: Vec<String> = [
        "-p",
        "--safe-mode",
        "--output-format",
        "json",
        "--tools",
        TOOLS,
        "--allowedTools",
        TOOLS,
        "--permission-mode",
        "dontAsk",
        "--json-schema",
    ]
    .map(String::from)
    .into();
    args.push(input.output_schema.to_string());
    args.extend(model_args("--model", &input.model)?);
    Ok(Launch {
        program: "claude",
        args,
        inherit: [INHERIT, CLAUDE_INHERIT].concat(),
        env: vec![],
        cwd: dir.to_path_buf(),
    })
}

/// `codex exec --json -s read-only` in the scratch dir, the last message to `reply.txt`, with
/// `CODEX_HOME` a fresh dir holding only a link to the user's login, so Codex loads none of the
/// user's config, MCP servers, plugins or AGENTS.md. The session lives in that home, so a
/// wrap-up can resume it and its log gives the model and the tokens.
// limit: no `--output-schema`: OpenAI's strict schemas need every field required (optional ones
// as `null`, which the decoders reject) and cannot express the resolve map; the schema is in
// the prompt and the core decodes the reply strictly.
fn codex_launch(input: &RunnerInput, dir: &Path) -> Result<Launch> {
    let auth = match std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        Some(home) => PathBuf::from(home),
        None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".codex"),
    }
    .join("auth.json");
    let auth = fs::canonicalize(&auth)
        .with_context(|| format!("Codex login not found at {}", auth.display()))?;
    let home = dir.join("codex-home");
    fs::create_dir_all(&home).with_context(|| format!("{}", home.display()))?;
    std::os::unix::fs::symlink(&auth, home.join("auth.json"))
        .with_context(|| format!("{}", home.display()))?;
    let mut args: Vec<String> = vec![
        "exec".into(),
        "--json".into(),
        "-s".into(),
        "read-only".into(),
        "--skip-git-repo-check".into(),
        "-C".into(),
        utf8(dir)?.into(),
        "-o".into(),
        utf8(&dir.join("reply.txt"))?.into(),
    ];
    args.extend(model_args("-m", &input.model)?);
    Ok(Launch {
        program: "codex",
        args,
        inherit: INHERIT.into(),
        env: vec![("CODEX_HOME".into(), utf8(&home)?.into())],
        cwd: dir.to_path_buf(),
    })
}

fn utf8(p: &Path) -> Result<&str> {
    p.to_str()
        .with_context(|| format!("path is not UTF-8: {}", p.display()))
}

/// A call's own working dir under the runner instance's state dir, removed when the call ends:
/// the agent's session log there holds source text.
struct Scratch(PathBuf);

impl Scratch {
    fn create(state: &Path) -> Result<Scratch> {
        let dir = state.join(format!("call-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        // The sandboxes match real paths (macOS `/var` is `/private/var`).
        Ok(Scratch(fs::canonicalize(&dir)?))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Token counts: input not read from a cache, input read from a cache, output.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Tokens {
    input: u64,
    cached: u64,
    output: u64,
}

/// USD per 1M tokens of one model (the runner's param `prices`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Price {
    input: f64,
    cached_input: f64,
    output: f64,
}

type Prices = BTreeMap<String, Price>;

fn prices_from_env() -> Result<Prices> {
    let Some(text) = std::env::var_os("RRECALL_PARAM_PRICES") else {
        return Ok(Prices::new());
    };
    let prices: Prices = serde_json::from_str(&text.to_string_lossy()).map_err(|e| {
        anyhow!("param `prices`: {e} (want {{ \"<model>\" = {{ input, cached_input, output }} }})")
    })?;
    for (m, p) in &prices {
        if [p.input, p.cached_input, p.output]
            .iter()
            .any(|v| !v.is_finite() || *v < 0.0)
        {
            bail!("param `prices`: model `{m}` has a price that is not a cost");
        }
    }
    Ok(prices)
}

/// The cost of `t` tokens of `model` from the price table: the agent reported none.
fn estimate(prices: &Prices, model: &str, t: Tokens) -> Result<f64> {
    let p = prices.get(model).with_context(|| {
        format!(
            "the agent reported no cost and the runner's param `prices` has no price for the model \
             `{model}` (USD per 1M tokens: input, cached_input, output)"
        )
    })?;
    Ok(
        (t.input as f64 * p.input + t.cached as f64 * p.cached_input + t.output as f64 * p.output)
            / 1e6,
    )
}

/// The model a call ran on, else the requested one when it names one.
fn model_or(found: Option<String>, requested: &str) -> Result<String> {
    match found {
        Some(m) if !m.trim().is_empty() => Ok(m),
        _ if requested != "default" => Ok(requested.to_string()),
        _ => bail!("the agent did not report which model it ran on"),
    }
}

/// The JSON object of a final message, inside a Markdown fence or not.
fn parse_reply(text: &str) -> Result<Value> {
    let t = text.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .and_then(|r| r.trim_end().strip_suffix("```"))
        .unwrap_or(t);
    serde_json::from_str(t.trim()).map_err(|e| {
        anyhow!(
            "the agent's final message is not JSON (line {} column {})",
            e.line(),
            e.column()
        )
    })
}

/// Claude's `--output-format json` result: the structured output (else the result text as
/// JSON), `total_cost_usd` (else an estimate), and the model with the most output tokens.
// limit: a turn killed at its deadline prints no result, so the cost counted after a wrap-up is
// what the wrap-up turn reports.
fn claude_reply(
    status: ExitStatus,
    stdout: &str,
    requested: &str,
    prices: &Prices,
) -> Result<(Value, f64, String)> {
    let out: Value = serde_json::from_str(stdout.trim())
        .map_err(|_| anyhow!("claude {status}, no JSON result"))?;
    if !status.success() || out["is_error"] != false {
        let first = out["result"]
            .as_str()
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or("");
        bail!("claude {status}, reported an error: {first}");
    }
    let output = match out.get("structured_output") {
        Some(v) if !v.is_null() => v.clone(),
        _ => parse_reply(out["result"].as_str().unwrap_or_default())?,
    };
    let found = out["modelUsage"].as_object().and_then(|m| {
        m.iter()
            .max_by_key(|(name, u)| {
                (
                    u["outputTokens"].as_u64().unwrap_or(0),
                    std::cmp::Reverse(*name),
                )
            })
            .map(|(name, _)| name.clone())
    });
    let model = model_or(found, requested)?;
    let cost = match out["total_cost_usd"].as_f64() {
        Some(c) => c,
        None => {
            let u = &out["usage"];
            let n = |k: &str| u[k].as_u64().unwrap_or(0);
            let t = Tokens {
                input: n("input_tokens") + n("cache_creation_input_tokens"),
                cached: n("cache_read_input_tokens"),
                output: n("output_tokens"),
            };
            estimate(prices, &model, t)?
        }
    };
    Ok((output, cost, model))
}

/// The thread id from Codex's `thread.started` event.
fn codex_thread(events: &str) -> Option<String> {
    events
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|e| e["type"] == "thread.started")
        .and_then(|e| e["thread_id"].as_str().map(String::from))
}

/// The tokens and the model of a Codex call: from its session log in `home` (the whole
/// session, a killed turn included), else the `turn.completed` events of the turns' stdout.
fn codex_usage(home: &Path, turns: &[String], requested: &str) -> Result<(Tokens, String)> {
    let (mut tokens, mut model) = (None, None);
    for log in session_logs(&home.join("sessions")) {
        for e in fs::read_to_string(&log)?
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        {
            let p = &e["payload"];
            if e["type"] == "turn_context" && model.is_none() {
                model = p["model"].as_str().map(String::from);
            }
            if p["type"] == "token_count" && p["info"]["total_token_usage"].is_object() {
                tokens = Some(codex_tokens(&p["info"]["total_token_usage"]));
            }
        }
    }
    let tokens = match tokens {
        Some(t) => t,
        None => {
            let done: Vec<Tokens> = turns
                .iter()
                .flat_map(|t| t.lines())
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .filter(|e| e["type"] == "turn.completed")
                .map(|e| codex_tokens(&e["usage"]))
                .collect();
            if done.is_empty() {
                bail!("codex reported no token counts to estimate its cost from");
            }
            done.iter().fold(Tokens::default(), |a, t| Tokens {
                input: a.input + t.input,
                cached: a.cached + t.cached,
                output: a.output + t.output,
            })
        }
    };
    Ok((tokens, model_or(model, requested)?))
}

/// Codex counts cached input inside `input_tokens`.
fn codex_tokens(u: &Value) -> Tokens {
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    Tokens {
        input: n("input_tokens").saturating_sub(n("cached_input_tokens")),
        cached: n("cached_input_tokens"),
        output: n("output_tokens"),
    }
}

/// Every `*.jsonl` under `dir`, sorted: a run-scoped home holds one session.
fn session_logs(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(session_logs(&p));
        } else if p.extension().is_some_and(|x| x == "jsonl") {
            out.push(p);
        }
    }
    out.sort();
    out
}

fn uuid_v4() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let random = || RandomState::new().build_hasher().finish();
    let (a, b) = (
        random() & !0xf000 | 0x4000,
        random() & !(0xc << 60) | (0x8 << 60),
    );
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0xffff,
        b >> 48,
        b & 0xffff_ffff_ffff
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn ok() -> ExitStatus {
        ExitStatus::from_raw(0)
    }

    fn prices() -> Prices {
        serde_json::from_str(r#"{"m1": {"input": 1, "cached_input": 0.1, "output": 10}}"#).unwrap()
    }

    #[test]
    fn the_wrap_up_gets_a_fifth_of_the_timeout_at_most_three_minutes() {
        let s = Duration::from_secs;
        assert_eq!(split(s(600)), (s(480), s(118)));
        assert_eq!(split(s(3600)), (s(3420), s(178)));
        assert_eq!(split(s(5)), (s(4), s(0)));
    }

    #[test]
    fn a_cost_is_estimated_from_tokens_or_names_the_missing_model() {
        let t = Tokens {
            input: 1_000_000,
            cached: 1_000_000,
            output: 1_000_000,
        };
        assert!((estimate(&prices(), "m1", t).unwrap() - 11.1).abs() < 1e-9);
        let e = estimate(&prices(), "m2", t).unwrap_err().to_string();
        assert!(e.contains("`m2`") && e.contains("`prices`"), "{e}");
    }

    #[test]
    fn a_reply_may_sit_in_a_markdown_fence() {
        for t in [
            "{\"a\":1}",
            " ```json\n{\"a\":1}\n``` ",
            "```\n{\"a\":1}\n```",
        ] {
            assert_eq!(parse_reply(t).unwrap(), json!({"a": 1}), "{t}");
        }
        assert!(parse_reply("Here it is: {}").is_err());
    }

    #[test]
    fn a_claude_result_gives_its_output_cost_and_main_model() {
        let out = r#"{"type":"result","is_error":false,"result":"done","structured_output":{"proposals":[]},
            "total_cost_usd":0.5,"modelUsage":{"small":{"outputTokens":3},"big":{"outputTokens":90}}}"#;
        let (o, c, m) = claude_reply(ok(), out, "default", &prices()).unwrap();
        assert_eq!((o, c, m.as_str()), (json!({"proposals": []}), 0.5, "big"));

        let no_cost = r#"{"is_error":false,"result":"```json\n{\"verdicts\":[]}\n```",
            "usage":{"input_tokens":100,"cache_creation_input_tokens":900000,"cache_read_input_tokens":0,"output_tokens":0}}"#;
        let (o, c, m) = claude_reply(ok(), no_cost, "m1", &prices()).unwrap();
        assert_eq!((o, m.as_str()), (json!({"verdicts": []}), "m1"));
        assert!((c - 0.9001).abs() < 1e-9, "{c}");

        let e = claude_reply(
            ok(),
            r#"{"is_error":false,"result":"{}","total_cost_usd":0}"#,
            "default",
            &prices(),
        );
        assert!(e.unwrap_err().to_string().contains("which model"));
        let e = claude_reply(
            ok(),
            r#"{"is_error":true,"result":"Credit balance too low\nmore"}"#,
            "m1",
            &prices(),
        );
        assert!(
            e.unwrap_err()
                .to_string()
                .contains("Credit balance too low")
        );
        assert!(claude_reply(ok(), "not json", "m1", &prices()).is_err());
    }

    #[test]
    fn codex_usage_comes_from_its_session_log_else_its_events() {
        let home = std::env::temp_dir().join(format!("rrecall-codex-usage-{}", std::process::id()));
        let day = home.join("sessions/2026/11/19");
        fs::create_dir_all(&day).unwrap();
        fs::write(
            day.join("rollout-x-th1.jsonl"),
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{}}\n",
                "{\"type\":\"turn_context\",\"payload\":{\"model\":\"m1\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":10,\"cached_input_tokens\":4,\"output_tokens\":2}}}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":30,\"cached_input_tokens\":10,\"output_tokens\":5}}}}\n",
            ),
        )
        .unwrap();
        let (t, m) = codex_usage(&home, &[], "default").unwrap();
        assert_eq!(
            (t, m.as_str()),
            (
                Tokens {
                    input: 20,
                    cached: 10,
                    output: 5
                },
                "m1"
            )
        );
        fs::remove_dir_all(&home).unwrap();

        let ev = "{\"type\":\"thread.started\",\"thread_id\":\"th1\"}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":7,\"cached_input_tokens\":2,\"output_tokens\":1}}\n";
        let (t, m) = codex_usage(&home, &[ev.into(), ev.into()], "m9").unwrap();
        assert_eq!(
            (t, m.as_str()),
            (
                Tokens {
                    input: 10,
                    cached: 4,
                    output: 2
                },
                "m9"
            )
        );
        assert_eq!(codex_thread(ev).as_deref(), Some("th1"));
        assert!(codex_usage(&home, &[], "m9").is_err());
    }

    #[test]
    fn launches_are_read_only_and_never_carry_the_prompt() {
        let input = RunnerInput {
            prompt: "SECRET PROMPT".into(),
            input: json!({}),
            output_schema: json!({"type": "object"}),
            model: "m1".into(),
            timeout: 60,
            read_only: true,
        };
        let dir = Path::new("/tmp/x");
        let c = claude_launch(&input, dir).unwrap();
        let args = c.args.join(" ");
        assert!(args.starts_with("-p --safe-mode --output-format json --tools Read,Grep,Glob --allowedTools Read,Grep,Glob --permission-mode dontAsk --json-schema {\"type\":\"object\"} --model m1"), "{args}");
        assert!(!args.contains("SECRET"));
        assert!(!c.inherit.contains(&"ANTHROPIC_API_KEY") && !c.inherit.contains(&"GH_TOKEN"));
        let bad = RunnerInput {
            model: "--dangerously-skip-permissions".into(),
            ..input
        };
        assert!(claude_launch(&bad, dir).is_err());
    }
}

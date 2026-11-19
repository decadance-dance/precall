//! The built-in runners of spec §10, `claude` and `codex` (§6.8). Each ships as a manifest
//! inside the binary and runs as `rrecall builtin <id>`: it reads the runner input on stdin,
//! runs `claude -p` or `codex exec` read-only with the prompt on stdin, and prints
//! `{ "output", "cost_usd", "model" }`. Adapted from the headless runner of rreview (same
//! author, same license).

use crate::error;
use crate::process::{self, TimedOut};
use crate::wire::{self, RunnerInput};
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
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
connectors = false
tools = []
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
    let access = match agent {
        Agent::Claude => access_from_env()?,
        Agent::Codex => Access::default(),
    };
    let scratch = Scratch::create()?;
    let dir = &scratch.0;
    let launch = match agent {
        Agent::Claude => claude_launch(&input, dir, &access)?,
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
        Err(e) if e.is::<TimedOut>() && wrap.is_zero() => bail!(
            "{e}; no wrap-up turn: a timeout of {}s is too short for one (it needs more than 10s)",
            input.timeout
        ),
        Err(e) if e.is::<TimedOut>() => {
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
    let (output, cost, model) = match agent {
        Agent::Claude => {
            let stdouts = turns
                .iter()
                .map(fs::read_to_string)
                .collect::<Result<Vec<_>, _>>()?;
            claude_reply(status, &stdouts, &input.model, &prices)?
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
    // limit: `allowed_runners` governs what rrecall sends the agent (input, resolved texts,
    // feedback); a read-only agent can still read files on the machine, the store included,
    // the same exposure as any coding-agent session there (spec §6.8).
    fn turn(
        &self,
        extra: &[String],
        stdin: &str,
        out: &Path,
        timeout: Duration,
    ) -> Result<ExitStatus> {
        // The plugin launcher's check (spec §6.3): a native executable or a `#!` script with a
        // native interpreter, never a file the C library would hand to `/bin/sh`.
        let cwd = std::env::current_dir().context("reading the working directory")?;
        let path_var = std::env::var_os("PATH").unwrap_or_default();
        let program = process::resolve_program(self.program, &self.cwd, &path_var, &cwd)?;
        let mut cmd = Command::new(program);
        cmd.arg0(self.program)
            .args(&self.args)
            .args(extra)
            .env_clear()
            .envs(
                std::env::vars_os()
                    .filter(|(k, _)| k.to_str().is_some_and(|k| self.inherit.contains(&k))),
            )
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(out)
                    .with_context(|| format!("{}", out.display()))?,
            )
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

/// The `claude` runner's params `connectors` and `tools` (spec §6.8): whether the call loads
/// the user's claude.ai connectors, and which of their tools, by exact name, it may use.
#[derive(Default, Debug)]
struct Access {
    connectors: bool,
    tools: Vec<String>,
}

fn access_from_env() -> Result<Access> {
    let param = |name: &str| std::env::var(format!("RRECALL_PARAM_{}", name.to_uppercase())).ok();
    let connectors = match param("connectors").as_deref() {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => bail!("param `connectors` must be true or false"),
    };
    let tools: Vec<String> = match param("tools") {
        None => vec![],
        Some(t) => serde_json::from_str(&t)
            .map_err(|_| anyhow!("param `tools` must be a list of tool names"))?,
    };
    for t in &tools {
        if !is_mcp_tool(t) {
            bail!(
                "param `tools`: `{t}` is not one exact MCP tool name `mcp__<server>__<tool>` \
                 (no wildcard, no server-wide name)"
            );
        }
    }
    if !tools.is_empty() && !connectors {
        bail!("param `tools` needs `connectors = true`: without it no MCP tool loads");
    }
    Ok(Access { connectors, tools })
}

/// `mcp__<server>__<tool>`, each part letters, digits, `_` or `-`: one tool, so a server-wide
/// name, a wildcard or a list can never allow a write tool nobody named.
fn is_mcp_tool(name: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    };
    name.strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
        .is_some_and(|(server, tool)| part(server) && part(tool))
}

/// A fresh owner-only config dir in the scratch dir, so none of the user's CLAUDE.md, settings,
/// permission rules, hooks or local plugins and MCP servers load. The login stays the user's:
/// Claude Code reads it (the macOS keychain entry, or `.credentials.json` elsewhere) from
/// `CLAUDE_SECURESTORAGE_CONFIG_DIR`, set to the user's own config dir (empty for the default
/// `~/.claude`), and refreshes it there in place as any session does; nothing is copied or linked.
// limit: `main` makes a relative `CLAUDE_CONFIG_DIR` absolute against the dir rrecall started in;
// if Claude Code names a keychain entry after the relative text, the call fails as not logged in.
fn claude_config(dir: &Path) -> Result<Vec<(String, String)>> {
    let user = match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        Some(d) => utf8(Path::new(&d))?.to_string(),
        None => String::new(),
    };
    let config = dir.join("claude-config");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&config)
        .with_context(|| format!("{}", config.display()))?;
    Ok(vec![
        ("CLAUDE_CONFIG_DIR".into(), utf8(&config)?.into()),
        ("CLAUDE_SECURESTORAGE_CONFIG_DIR".into(), user),
    ])
}

/// `claude -p --safe-mode --output-format stream-json`, tools limited to reading, every other
/// permission refused without a prompt nobody would answer, and the output schema enforced.
/// `--safe-mode` drops the user's CLAUDE.md, skills, plugins, hooks and MCP servers.
/// With `connectors` (spec §12): the claude.ai connectors load only from the user setting
/// source, so that replaces `--safe-mode`, pointed at a fresh config dir ([`claude_config`]),
/// with skills, slash commands, hooks and CLAUDE.md files off and tool search on (else every
/// connector's tool schemas fill each call). What the account syncs still loads.
fn claude_launch(input: &RunnerInput, dir: &Path, access: &Access) -> Result<Launch> {
    let mut args: Vec<String> = vec!["-p".into()];
    let mut env = vec![];
    if access.connectors {
        args.extend(
            [
                "--setting-sources",
                "user",
                "--disable-slash-commands",
                "--settings",
                r#"{"disableAllHooks":true}"#,
            ]
            .map(String::from),
        );
        env.extend(claude_config(dir)?);
        for (k, v) in [
            ("ENABLE_TOOL_SEARCH", "true"),
            // A fresh config dir has no MCP discovery cache: wait for the connectors.
            ("MCP_CONNECTION_NONBLOCKING", "false"),
            ("CLAUDE_CODE_MCP_STARTUP_WAIT_MS", "30000"),
            ("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "1"),
        ] {
            env.push((k.into(), v.into()));
        }
    } else {
        args.push("--safe-mode".into());
    }
    // With tool search the connectors' tool schemas are deferred: ToolSearch loads them.
    let tools = if access.connectors {
        format!("{TOOLS},ToolSearch")
    } else {
        TOOLS.to_string()
    };
    let allowed = [tools.clone()]
        .into_iter()
        .chain(access.tools.iter().cloned())
        .collect::<Vec<_>>()
        .join(",");
    args.extend(
        [
            // Streamed, so a turn killed at its deadline still shows the usage it had.
            "--output-format",
            "stream-json",
            "--verbose",
            "--tools",
            &tools,
            "--allowedTools",
            &allowed,
            "--permission-mode",
            "dontAsk",
            "--json-schema",
        ]
        .map(String::from),
    );
    args.push(input.output_schema.to_string());
    args.extend(model_args("--model", &input.model)?);
    Ok(Launch {
        program: "claude",
        args,
        inherit: [INHERIT, CLAUDE_INHERIT].concat(),
        env,
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

/// A call's own working dir in the runners' private dir in the system temp dir, removed when
/// the call ends: outside the map store, so the agent cannot read the map by a relative path,
/// and outside any git repository, so it loads no project's instruction files. The agent's
/// session log there holds source text, so the dir is the user's alone.
struct Scratch(PathBuf);

/// A scratch dir's name: the prefix, the pid of the runner that made it, and a random part.
const SCRATCH_PREFIX: &str = "agent-";

impl Scratch {
    fn create() -> Result<Scratch> {
        let private = private_dir()?;
        sweep(&private);
        let name = format!("{SCRATCH_PREFIX}{}-{}", std::process::id(), uuid_v4());
        let dir = private.join(name);
        // Created with its mode in one step, and never an existing dir.
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        // Removed again by `Drop` when a step below fails.
        let mut scratch = Scratch(dir);
        // The sandboxes match real paths (macOS `/var` is `/private/var`).
        scratch.0 = fs::canonicalize(&scratch.0)?;
        if let Some(repo) = scratch.0.ancestors().find(|a| a.join(".git").exists()) {
            bail!(
                "the runner's temp dir {} is inside the git repository {}: set TMPDIR outside it",
                scratch.0.display(),
                repo.display()
            );
        }
        Ok(scratch)
    }
}

/// `rrecall-<uid>` in the temp dir, which every scratch dir lives in and the sweep looks in
/// alone: made owner-only, and refused when it is not a dir of this user that only they reach,
/// since the temp dir is shared.
fn private_dir() -> Result<PathBuf> {
    let uid = unsafe { libc::getuid() };
    let dir = std::env::temp_dir().join(format!("rrecall-{uid}"));
    match fs::DirBuilder::new().mode(0o700).create(&dir) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => {
            return Err(e).with_context(|| format!("creating {}", dir.display()));
        }
        _ => {}
    }
    let m = fs::symlink_metadata(&dir).with_context(|| format!("{}", dir.display()))?;
    if !m.is_dir() || m.uid() != uid || m.mode() & 0o077 != 0 {
        bail!(
            "{} must be a dir that only this user owns and reaches (mode 0700): remove it",
            dir.display()
        );
    }
    strip_acl(&dir)?;
    Ok(dir)
}

/// On macOS an ACL the temp dir passes on is independent of the mode and could let other users
/// read the call's files: the private dir drops it before a scratch dir is created inside, so
/// nothing inside inherits one.
#[cfg(target_os = "macos")]
fn strip_acl(dir: &Path) -> Result<()> {
    let ok = Command::new("/bin/chmod")
        .arg("-N")
        .arg(dir)
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("removing the ACL of {}", dir.display()))?
        .success();
    if !ok {
        bail!("could not remove the ACL of {}", dir.display());
    }
    Ok(())
}

// Linux: a default (inherited) POSIX ACL's entries are masked by the mode's group bits, which
// 0700 and 0600 clear.
#[cfg(not(target_os = "macos"))]
fn strip_acl(_: &Path) -> Result<()> {
    Ok(())
}

/// Removes the scratch dirs of runners killed before their own cleanup ran (`Drop` never runs
/// on SIGKILL): only entries with the exact name form whose runner pid no longer exists. Only
/// this user can add one to the private dir, and `remove_dir_all` never follows a link.
// limit: a dead runner's pid reused by a live process keeps its dir until that process ends;
// a dir whose runner was killed while its agent lives on is removed under that agent, whose
// answer nobody reads any more.
fn sweep(private: &Path) {
    let Ok(entries) = fs::read_dir(private) else {
        return;
    };
    for e in entries.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(scratch_pid) else {
            continue;
        };
        // SAFETY: signal 0 only checks that the process exists.
        let gone = unsafe { libc::kill(pid, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if gone {
            let _ = fs::remove_dir_all(e.path());
        }
    }
}

/// The runner pid of a name made by `Scratch::create`, `None` for any other name.
fn scratch_pid(name: &str) -> Option<libc::pid_t> {
    let (pid, id) = name.strip_prefix(SCRATCH_PREFIX)?.split_once('-')?;
    let hex =
        |s: &str, n| s.len() == n && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    let parts: Vec<&str> = id.split('-').collect();
    let uuid = parts.len() == 5 && parts.iter().zip([8, 4, 4, 4, 12]).all(|(p, n)| hex(p, n));
    let pid = pid
        .parse::<libc::pid_t>()
        .ok()
        .filter(|p| *p > 0 && p.to_string() == pid)?;
    uuid.then_some(pid)
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
    wire::parse_strict(t.trim())
        .map_err(|e| anyhow!("the agent's final message is not one JSON document: {e}"))
}

/// Claude's input tokens (cache writes count as input) and output; `None` without the counts.
fn claude_tokens(u: &Value) -> Option<Tokens> {
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    Some(Tokens {
        input: u["input_tokens"].as_u64()? + n("cache_creation_input_tokens"),
        cached: n("cache_read_input_tokens"),
        output: u["output_tokens"].as_u64()?,
    })
}

/// One `--output-format stream-json` turn: its `result` event, if it got that far, and the
/// model and usage of each assistant message by id (claude repeats a message per content block).
type ClaudeTurn = (Option<Value>, BTreeMap<String, (String, Tokens)>);

fn claude_turn(stdout: &str) -> Result<ClaudeTurn> {
    let mut result = None;
    let mut messages = BTreeMap::new();
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    for (i, line) in lines.iter().enumerate() {
        let e = match wire::parse_strict(line) {
            Ok(e) => e,
            // A kill can cut the last record short; every complete record is decoded strictly.
            Err(e) if e.is_eof() && i + 1 == lines.len() && !stdout.ends_with('\n') => break,
            Err(e) => bail!("claude printed a line that is not one JSON document: {e}"),
        };
        if e["type"] == "result" {
            result = Some(e);
        } else if e["type"] == "assistant" {
            let m = &e["message"];
            if let (Some(id), Some(model), Some(t)) = (
                m["id"].as_str(),
                m["model"].as_str(),
                claude_tokens(&m["usage"]),
            ) {
                messages.insert(id.to_string(), (model.to_string(), t));
            }
        }
    }
    Ok((result, messages))
}

/// Claude's answer from the last turn's `result`: the structured output (else the result text
/// as JSON) and the model with the most output tokens. The cost is the result's
/// `total_cost_usd` (else an estimate from its usage) plus, for each killed earlier turn, its
/// own `total_cost_usd` when it printed one, else an estimate from the usage it streamed.
// limit: a request still in flight when a turn is killed streamed no usage and is not counted.
fn claude_reply(
    status: ExitStatus,
    turns: &[String],
    requested: &str,
    prices: &Prices,
) -> Result<(Value, f64, String)> {
    let (last, earlier) = turns.split_last().context("no claude turn")?;
    let out = claude_turn(last)?
        .0
        .with_context(|| format!("claude {status}, no JSON result"))?;
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
    let mut cost = match out["total_cost_usd"].as_f64() {
        Some(c) => c,
        None => {
            let t = claude_tokens(&out["usage"])
                .context("claude reported neither a cost nor its token usage")?;
            estimate(prices, &model, t)?
        }
    };
    for turn in earlier {
        let (result, messages) = claude_turn(turn)?;
        match result.and_then(|r| r["total_cost_usd"].as_f64()) {
            Some(c) => cost += c,
            None => {
                for (m, t) in messages.values() {
                    cost += estimate(prices, m, *t)?;
                }
            }
        }
    }
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
        let used = |e: &Value| e["type"] == "turn_context" || e["payload"]["type"] == "token_count";
        for e in codex_records(&fs::read_to_string(&log)?, used)? {
            let p = &e["payload"];
            if e["type"] == "turn_context" && model.is_none() {
                model = p["model"].as_str().map(String::from);
            }
            if p["type"] == "token_count"
                && let Some(t) = codex_tokens(&p["info"]["total_token_usage"])
            {
                tokens = Some(t);
            }
        }
    }
    let tokens = match tokens {
        Some(t) => t,
        None => {
            let done: Vec<Tokens> = turns
                .iter()
                .map(|t| codex_records(t, |e| e["type"] == "turn.completed"))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .map(|e| {
                    codex_tokens(&e["usage"])
                        .context("codex reported a turn without its token usage")
                })
                .collect::<Result<_>>()?;
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

/// The JSON records of Codex output that `used` picks, decoded strictly (a key twice fails); a
/// line that is not JSON, such as a record cut short by a kill, is skipped.
fn codex_records(text: &str, used: fn(&Value) -> bool) -> Result<Vec<Value>> {
    text.lines()
        .filter(|l| serde_json::from_str::<Value>(l).is_ok_and(|e| used(&e)))
        .map(|l| {
            wire::parse_strict(l)
                .map_err(|e| anyhow!("codex printed a record that is not strict JSON: {e}"))
        })
        .collect()
}

/// Codex counts cached input inside `input_tokens`; `None` without the counts.
fn codex_tokens(u: &Value) -> Option<Tokens> {
    let cached = u["cached_input_tokens"].as_u64().unwrap_or(0);
    Some(Tokens {
        input: u["input_tokens"].as_u64()?.saturating_sub(cached),
        cached,
        output: u["output_tokens"].as_u64()?,
    })
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
        let reply = |turns: &[&str], model| {
            let turns: Vec<String> = turns.iter().map(|t| t.to_string()).collect();
            claude_reply(ok(), &turns, model, &prices())
        };
        let out = concat!(
            r#"{"type":"system","subtype":"init"}"#,
            "\n",
            r#"{"type":"result","is_error":false,"result":"done","structured_output":{"proposals":[]},"total_cost_usd":0.5,"modelUsage":{"small":{"outputTokens":3},"big":{"outputTokens":90}}}"#,
        );
        let (o, c, m) = reply(&[out], "default").unwrap();
        assert_eq!((o, c, m.as_str()), (json!({"proposals": []}), 0.5, "big"));

        let no_cost = r#"{"type":"result","is_error":false,"result":"```json\n{\"verdicts\":[]}\n```","usage":{"input_tokens":100,"cache_creation_input_tokens":900000,"cache_read_input_tokens":0,"output_tokens":0}}"#;
        let (o, c, m) = reply(&[no_cost], "m1").unwrap();
        assert_eq!((o, m.as_str()), (json!({"verdicts": []}), "m1"));
        assert!((c - 0.9001).abs() < 1e-9, "{c}");

        // A killed turn adds what it streamed, each message once.
        let killed = concat!(
            r#"{"type":"assistant","message":{"id":"a","model":"m1","usage":{"input_tokens":1000000,"output_tokens":0}}}"#,
            "\n",
            r#"{"type":"assistant","message":{"id":"a","model":"m1","usage":{"input_tokens":1000000,"output_tokens":0}}}"#,
        );
        let (_, c, _) = reply(&[killed, out], "default").unwrap();
        assert!((c - 1.5).abs() < 1e-9, "{c}");

        let no_model = r#"{"type":"result","is_error":false,"result":"{}","total_cost_usd":0}"#;
        assert!(
            reply(&[no_model], "default")
                .unwrap_err()
                .to_string()
                .contains("which model")
        );
        let no_usage = r#"{"type":"result","is_error":false,"result":"{}"}"#;
        assert!(
            reply(&[no_usage], "m1")
                .unwrap_err()
                .to_string()
                .contains("usage")
        );
        let err = r#"{"type":"result","is_error":true,"result":"Credit balance too low\nmore"}"#;
        assert!(
            reply(&[err], "m1")
                .unwrap_err()
                .to_string()
                .contains("Credit balance too low")
        );
        assert!(reply(&["not json"], "m1").is_err());
        let twice =
            r#"{"type":"result","is_error":false,"result":"{\"a\":1,\"a\":2}","total_cost_usd":0}"#;
        assert!(
            reply(&[twice], "m1")
                .unwrap_err()
                .to_string()
                .contains("duplicate key")
        );
    }

    #[test]
    fn only_a_generated_scratch_name_yields_a_pid() {
        let id = uuid_v4();
        assert_eq!(scratch_pid(&format!("agent-42-{id}")), Some(42));
        for other in [
            "agent-1-project-notes".to_string(),
            format!("agent-042-{id}"),
            format!("agent-0-{id}"),
            format!("agent--1-{id}"),
            format!("agent-1-{id}-x"),
            format!("agent-1-{}", id.to_uppercase()),
            format!("rrecall-agent-1-{id}"),
        ] {
            assert_eq!(scratch_pid(&other), None, "{other}");
        }
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
        let c = claude_launch(&input, dir, &Access::default()).unwrap();
        let args = c.args.join(" ");
        assert!(args.starts_with("-p --safe-mode --output-format stream-json --verbose --tools Read,Grep,Glob --allowedTools Read,Grep,Glob --permission-mode dontAsk --json-schema {\"type\":\"object\"} --model m1"), "{args}");
        assert!(!args.contains("SECRET"));
        assert!(!c.inherit.contains(&"ANTHROPIC_API_KEY") && !c.inherit.contains(&"GH_TOKEN"));
        let bad = RunnerInput {
            model: "--dangerously-skip-permissions".into(),
            ..input
        };
        assert!(claude_launch(&bad, dir, &Access::default()).is_err());
    }
}

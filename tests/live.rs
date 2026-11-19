//! Live tests of the built-in runners against the real `claude` and `codex` with the user's
//! logins. They cost money: ignored, and run only with
//! `RRECALL_LIVE=1 cargo test --test live -- --ignored --nocapture`. Codex reports no cost, so
//! its test needs a price table: `RRECALL_LIVE_CODEX_PRICES='{"<model>":{"input":N,
//! "cached_input":N,"output":N}}'` (USD per 1M tokens). `RRECALL_LIVE_CLAUDE_MODEL` picks the
//! Claude model (default `haiku`).

use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};

fn live() -> bool {
    let on = std::env::var("RRECALL_LIVE").as_deref() == Ok("1");
    if !on {
        eprintln!("skipped: set RRECALL_LIVE=1 to run live tests");
    }
    on
}

/// Runs built-in runner `id` on a tiny prompt and returns its output document.
fn call(id: &str, model: &str, prices: Option<String>) -> Value {
    let state = std::env::temp_dir().join(format!("rrecall-live-{id}-{}", std::process::id()));
    std::fs::create_dir_all(&state).unwrap();
    let schema = json!({
        "type": "object", "additionalProperties": false, "required": ["answer"],
        "properties": {"answer": {"type": "string"}}
    });
    let input =
        json!({"question": "How many legs does a spider have? Answer with the number only."});
    let runner_input = json!({
        "prompt": rrecall::wire::render_prompt("Answer the question in the input.", &input, &schema),
        "input": input,
        "output_schema": schema,
        "model": model,
        "timeout": 300,
        "read_only": true,
    });
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rrecall"));
    cmd.args(["builtin", id])
        .env("RRECALL_STATE_DIR", &state)
        .env("RRECALL_RUN_ID", "live")
        .env_remove("RRECALL_PARAM_PRICES")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(p) = prices {
        cmd.env("RRECALL_PARAM_PRICES", p);
    }
    let started = std::time::Instant::now();
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(runner_input.to_string().as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(&state);
    let stdout = String::from_utf8_lossy(&o.stdout);
    println!(
        "{id}: {:.1}s, stdout: {stdout}",
        started.elapsed().as_secs_f64()
    );
    println!("{id}: stderr: {}", String::from_utf8_lossy(&o.stderr));
    assert!(o.status.success(), "{id} failed");
    let out: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        out["output"]["answer"].as_str().map(str::trim),
        Some("8"),
        "{out}"
    );
    assert!(out["cost_usd"].as_f64().is_some_and(|c| c > 0.0), "{out}");
    assert!(
        out["model"].as_str().is_some_and(|m| !m.is_empty()),
        "{out}"
    );
    out
}

#[test]
#[ignore = "live: costs money; RRECALL_LIVE=1"]
fn live_claude_runner_answers_with_cost_and_model() {
    if live() {
        let model = std::env::var("RRECALL_LIVE_CLAUDE_MODEL").unwrap_or_else(|_| "haiku".into());
        call("claude", &model, None);
    }
}

#[test]
#[ignore = "live: costs money; RRECALL_LIVE=1"]
fn live_codex_runner_answers_with_cost_and_model() {
    if live() {
        let prices = std::env::var("RRECALL_LIVE_CODEX_PRICES")
            .expect("set RRECALL_LIVE_CODEX_PRICES to the price table of the model Codex uses");
        call("codex", "default", Some(prices));
    }
}

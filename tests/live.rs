//! Live tests of the built-in runners against the real `claude` and `codex` with the user's
//! logins. They cost money: ignored, and run only with
//! `PRECALL_LIVE=1 cargo test --test live -- --ignored --nocapture`. Codex reports no cost, so
//! its test needs a price table: `PRECALL_LIVE_CODEX_PRICES='{"<model>":{"input":N,
//! "cached_input":N,"output":N}}'` (USD per 1M tokens). `PRECALL_LIVE_CLAUDE_MODEL` picks the
//! Claude model (default `haiku`).

use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};

fn live() -> bool {
    let on = std::env::var("PRECALL_LIVE").as_deref() == Ok("1");
    if !on {
        eprintln!("skipped: set PRECALL_LIVE=1 to run live tests");
    }
    on
}

/// Runs built-in runner `id` on a tiny prompt and returns its output document.
fn call(id: &str, model: &str, prices: Option<String>) -> Value {
    let schema = json!({
        "type": "object", "additionalProperties": false, "required": ["answer"],
        "properties": {"answer": {"type": "string"}}
    });
    let input =
        json!({"question": "How many legs does a spider have? Answer with the number only."});
    let runner_input = json!({
        "prompt": precall::wire::render_prompt("Answer the question in the input.", &input, &schema),
        "input": input,
        "output_schema": schema,
        "model": model,
        "timeout": 300,
        "read_only": true,
    });
    let out = runner(id, prices, &runner_input);
    assert_eq!(
        out["output"]["answer"].as_str().map(str::trim),
        Some("8"),
        "{out}"
    );
    out
}

/// Runs built-in runner `id` on `runner_input` (spec §6.8) and returns its output document,
/// which must carry a cost and a model.
fn runner(id: &str, prices: Option<String>, runner_input: &Value) -> Value {
    let state = std::env::temp_dir().join(format!("precall-live-{id}-{}", std::process::id()));
    std::fs::create_dir_all(&state).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_precall"));
    cmd.args(["builtin", id])
        .env("PRECALL_STATE_DIR", &state)
        .env("PRECALL_RUN_ID", "live")
        .env_remove("PRECALL_PARAM_PRICES")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(p) = prices {
        cmd.env("PRECALL_PARAM_PRICES", p);
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
    assert!(out["cost_usd"].as_f64().is_some_and(|c| c > 0.0), "{out}");
    assert!(
        out["model"].as_str().is_some_and(|m| !m.is_empty()),
        "{out}"
    );
    out
}

#[test]
#[ignore = "live: costs money; PRECALL_LIVE=1"]
fn live_claude_runner_answers_with_cost_and_model() {
    if live() {
        let model = std::env::var("PRECALL_LIVE_CLAUDE_MODEL").unwrap_or_else(|_| "haiku".into());
        call("claude", &model, None);
    }
}

#[test]
#[ignore = "live: costs money; PRECALL_LIVE=1"]
fn live_codex_runner_answers_with_cost_and_model() {
    if live() {
        let prices = std::env::var("PRECALL_LIVE_CODEX_PRICES")
            .expect("set PRECALL_LIVE_CODEX_PRICES to the price table of the model Codex uses");
        call("codex", "default", Some(prices));
    }
}

/// Replays each built-in judge's labeled set (`tests/fixtures/judges/`) through the real
/// `claude` with the prompt a run renders, and reports how many verdicts match the labels
/// (a `revise` counts as not accepted).
#[test]
#[ignore = "live: costs money; PRECALL_LIVE=1"]
fn live_builtin_judges_agree_with_their_labeled_sets() {
    if !live() {
        return;
    }
    let model = std::env::var("PRECALL_LIVE_CLAUDE_MODEL").unwrap_or_else(|_| "haiku".into());
    for judge in ["support", "removal", "duplicate"] {
        let path = format!(
            "{}/tests/fixtures/judges/{judge}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let set: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let input = set["input"].clone();
        let schema = precall::wire::judge_schema();
        let prompt = precall::plugin::builtin_prompt(judge, &format!("{judge}.md")).unwrap();
        let out = runner(
            "claude",
            None,
            &json!({
                "prompt": precall::wire::render_prompt(&prompt, &input, &schema),
                "input": input,
                "output_schema": schema,
                "model": model,
                "timeout": 600,
                "read_only": true,
            }),
        );
        let expected = set["expected"].as_object().unwrap();
        let keys: Vec<String> = expected.keys().cloned().collect();
        let verdicts =
            precall::wire::decode_verdicts(judge, &out["output"].to_string(), &keys).unwrap();
        let mut agree = 0;
        for v in &verdicts {
            let accepted = v.verdict == precall::wire::VerdictKind::Accept;
            if accepted == (expected[&v.key] == "accept") {
                agree += 1;
            } else {
                println!(
                    "{judge}: {} expected {}, got {:?}: {}",
                    v.key, expected[&v.key], v.verdict, v.reason
                );
            }
        }
        println!("{judge}: {agree}/{} agree with the labels", keys.len());
        // limit: the bar the M3 live run (#11) settles; a judge below it needs its prompt fixed.
        assert!(
            agree * 5 >= keys.len() * 4,
            "{judge}: {agree}/{}",
            keys.len()
        );
    }
}

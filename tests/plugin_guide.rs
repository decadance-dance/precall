//! The plugin author guide (`docs/plugins.md`) cannot drift from the code: every JSON example
//! in it goes through the real wire types (`rrecall::wire`), its copy of the example manifest
//! matches the file, and its steps install and run the example plugin with a fake runner.

use rrecall::config::BranchDef;
use rrecall::merge::Effect;
use rrecall::node::Node;
use rrecall::wire::{self, Proposal, VerdictKind};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const GUIDE: &str = "docs/plugins.md";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn guide() -> String {
    std::fs::read_to_string(root().join(GUIDE)).expect("docs/plugins.md")
}

/// The fenced blocks of `lang` in the guide whose tag line, right before the block, is
/// `<!-- <kind>: <tag> -->`, by tag. Every block of `lang` needs a tag line of some kind, so
/// none escapes the tests by accident.
fn blocks(lang: &str, kind: &str) -> BTreeMap<String, String> {
    let text = guide();
    let lines: Vec<&str> = text.lines().collect();
    let mut out = BTreeMap::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == format!("```{lang}") {
            let (k, tag) = i
                .checked_sub(1)
                .and_then(|p| lines[p].trim().strip_prefix("<!-- "))
                .and_then(|t| t.strip_suffix(" -->"))
                .and_then(|t| t.split_once(": "))
                .unwrap_or_else(|| {
                    panic!(
                        "{GUIDE}:{}: a ```{lang} block without `<!-- <kind>: <tag> -->`",
                        i + 1
                    )
                });
            let end = (i + 1..lines.len())
                .find(|&j| lines[j].trim() == "```")
                .unwrap_or_else(|| panic!("{GUIDE}:{}: unclosed block", i + 1));
            if k == kind {
                let body = lines[i + 1..end].join("\n") + "\n";
                assert!(
                    out.insert(tag.to_string(), body).is_none(),
                    "{GUIDE}: tag `{kind}: {tag}` twice"
                );
            }
            i = end;
        }
        i += 1;
    }
    out
}

/// The JSON examples, parsed as strictly as plugin output.
fn examples() -> BTreeMap<String, Value> {
    blocks("json", "wire")
        .into_iter()
        .map(|(tag, body)| {
            let v = wire::parse_strict(&body).unwrap_or_else(|e| panic!("example `{tag}`: {e}"));
            (tag, v)
        })
        .collect()
}

fn de<T: serde::de::DeserializeOwned>(tag: &str, v: &Value) -> T {
    serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("example `{tag}`: {e}"))
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// The core's own serialization of what the guide shows must be the guide's JSON exactly.
fn same(tag: &str, guide: &Value, real: impl serde::Serialize) {
    let real = serde_json::to_value(real).unwrap();
    assert_eq!(
        guide, &real,
        "example `{tag}` is not what the core sends:\n{real:#}"
    );
}

#[test]
fn every_json_example_in_the_guide_is_what_the_wire_types_read_or_write() {
    let ex = examples();
    let want = [
        "fingerprint-input",
        "fetch-input",
        "fetch-output",
        "collect-input",
        "collect-output",
        "resolve-input",
        "resolve-output",
        "judge-input",
        "judge-output",
        "runner-input",
        "runner-output",
        "hook-input",
        "hook-output",
        "view-input",
    ];
    assert_eq!(
        ex.keys().map(String::as_str).collect::<Vec<_>>(),
        {
            let mut w = want.to_vec();
            w.sort();
            w
        },
        "the guide has one example per I/O"
    );
    assert_eq!(
        guide().matches("```json").count(),
        ex.len(),
        "every JSON block in the guide is a `wire:` example"
    );
    let e = |t: &str| &ex[t];

    // Inputs: rebuilt as the core builds them, from the guide's own values.
    for tag in ["fingerprint-input", "view-input"] {
        let v = e(tag);
        same(
            tag,
            v,
            wire::PluginInput {
                project: s(&v["project"]),
                run_id: s(&v["run_id"]),
                params: de(tag, &v["params"]),
            },
        );
    }
    let v = e("fetch-input");
    same(
        "fetch-input",
        v,
        wire::FetchInput {
            project: s(&v["project"]),
            run_id: s(&v["run_id"]),
            params: de("fetch-input", &v["params"]),
            since: v["since"].clone(),
        },
    );
    let v = e("collect-input");
    let rejections = v["rejections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| wire::Rejection {
            proposal: de("collect-input", &r["proposal"]),
            verdicts: r["verdicts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| wire::RoundVerdict {
                    by: s(&x["by"]),
                    verdict: de("collect-input", &x["verdict"]),
                    reason: s(&x["reason"]),
                    instruction: x.get("instruction").map(s),
                    conflict_with: x.get("conflict_with").map(|c| de("collect-input", c)),
                    saw: Vec::new(),
                })
                .collect(),
        })
        .collect();
    same(
        "collect-input",
        v,
        wire::CollectorInput {
            project: s(&v["project"]),
            run_id: s(&v["run_id"]),
            round: v["round"].as_u64().unwrap() as u32,
            scope: de::<Vec<BranchDef>>("collect-input", &v["scope"]),
            nodes: de::<Vec<Node>>("collect-input", &v["nodes"]),
            rejections,
            params: de("collect-input", &v["params"]),
            fetched: v.get("fetched").cloned(),
            since: v["since"].clone(),
        },
    );
    let refs: Vec<String> = de("resolve-input", &e("resolve-input")["refs"]);
    same(
        "resolve-input",
        e("resolve-input"),
        wire::ResolveInput { refs: refs.clone() },
    );
    let judge: wire::JudgeInput = de("judge-input", e("judge-input"));
    same("judge-input", e("judge-input"), &judge);
    let runner: wire::RunnerInput = de("runner-input", e("runner-input"));
    same("runner-input", e("runner-input"), &runner);
    let v = e("hook-input");
    let diff = v["data"]["diff"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            let leak = |x: &Value| -> &'static str { Box::leak(s(x).into_boxed_str()) };
            Effect {
                change: leak(&d["change"]),
                id: s(&d["id"]),
                parent: s(&d["parent"]),
                fields: d
                    .get("fields")
                    .map(|f| f.as_array().unwrap().iter().map(leak).collect())
                    .unwrap_or_default(),
                by: s(&d["by"]),
                key: s(&d["key"]),
                branch: String::new(),
                from: None,
                reason: String::new(),
                refs: Vec::new(),
            }
        })
        .collect::<Vec<_>>();
    same(
        "hook-input",
        v,
        wire::HookInput {
            event: de("hook-input", &v["event"]),
            project: s(&v["project"]),
            run_id: s(&v["run_id"]),
            data: json!({ "diff": diff, "message": v["data"]["message"] }),
        },
    );

    // Outputs: through the decoders the core runs on plugin output.
    let text = |t: &str| e(t).to_string();
    let fetched = e("fetch-output");
    if let Some(c) = fetched.get("cursor") {
        wire::check_cursor("guide", c).unwrap();
    }
    let (props, cursor) = wire::decode_collect("guide", &text("collect-output")).unwrap();
    assert!(!props.is_empty() && cursor.is_some());
    let texts = wire::decode_texts("guide", &text("resolve-output"), &refs).unwrap();
    assert!(texts.values().any(Option::is_some) && texts.values().any(Option::is_none));
    let keys: Vec<String> = judge.proposals.iter().map(|p| p.key.clone()).collect();
    let verdicts = wire::decode_verdicts("guide", &text("judge-output"), &keys).unwrap();
    assert!(verdicts.iter().any(|v| v.verdict == VerdictKind::Revise));
    let out = wire::decode_runner("guide", &text("runner-output")).unwrap();
    let props: Vec<Proposal> = wire::decode_proposals("guide", &out.output.to_string()).unwrap();
    assert!(!props.is_empty());
    assert!(
        wire::decode_hook("guide", &text("hook-output"))
            .unwrap()
            .is_some()
    );
}

#[test]
fn the_guides_copy_of_the_example_manifest_is_the_file() {
    let b = blocks("toml", "file");
    let file = "examples/meeting-notes/rrecall-plugin.toml";
    let real = std::fs::read_to_string(root().join(file)).unwrap();
    assert_eq!(b[file], real, "{GUIDE} shows {file} as it is");
}

#[test]
fn every_manifest_sketch_in_the_guide_loads() {
    let sketches = blocks("toml", "manifest");
    assert!(!sketches.is_empty());
    for (tag, text) in sketches {
        let dir = std::env::temp_dir().join(format!("rrecall-guide-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("rrecall-plugin.toml"), &text).unwrap();
        // The files it names: prompts, and programs relative to the plugin directory.
        let raw: toml::Table = toml::from_str(&text).unwrap();
        for v in raw
            .values()
            .filter_map(|v| v.as_table())
            .flat_map(|t| t.values())
        {
            let names: Vec<&str> = match v {
                toml::Value::String(s) if s.ends_with(".md") => vec![s],
                toml::Value::Array(a) => a.first().and_then(|p| p.as_str()).into_iter().collect(),
                _ => vec![],
            };
            for n in names
                .into_iter()
                .filter(|n| n.ends_with(".md") || n.starts_with("./"))
            {
                let f = dir.join(n);
                std::fs::write(&f, "#!/bin/sh\n").unwrap();
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let r = rrecall::plugin::load_manifest(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        r.unwrap_or_else(|e| panic!("manifest sketch `{tag}`: {e:#}"));
    }
}

// The guide's steps, run as written.

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

struct W {
    root: PathBuf,
}

impl Drop for W {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl W {
    fn new() -> W {
        let root = std::env::temp_dir().join(format!("rrecall-guide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["home/agent", "data", "config", "work"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        W {
            root: root.canonicalize().unwrap(),
        }
    }

    fn agent(&self) -> PathBuf {
        self.root.join("home/agent")
    }

    fn rr(&self, args: &[&str], stdin: &str) -> Out {
        let mut c = Command::new(env!("CARGO_BIN_EXE_rrecall"));
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
            .env_remove("RRECALL_PROJECT")
            .env_remove("GIT_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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
}

fn calls(agent: &Path) -> usize {
    std::fs::read_to_string(agent.join("claude.calls"))
        .unwrap_or_default()
        .lines()
        .count()
}

#[test]
fn the_guides_steps_install_enable_and_run_the_example_plugin() {
    let w = W::new();
    let example = root().join("examples/meeting-notes");
    let notes = root().join("examples/meeting-notes-data");
    let steps = blocks("sh", "step");
    assert!(
        steps["install"].contains("rrecall plugin add examples/meeting-notes"),
        "{}",
        steps["install"]
    );

    // Install: the preview, the question, `y`.
    let o = w.rr(&["plugin", "add", example.to_str().unwrap()], "y\n");
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(
        o.stderr.contains("Install meeting-notes into"),
        "{}",
        o.stderr
    );
    assert!(
        o.stdout.contains("prompt.md"),
        "the preview shows the prompts: {}",
        o.stdout
    );

    // A store, and the guide's instance appended to its config.
    let o = w.rr(&["init", "lantern"], "");
    assert_eq!(o.code, 0, "{}", o.stderr);
    let store = w.root.join("data/rrecall/lantern");
    let enable = blocks("toml", "step")["enable"].clone();
    assert!(enable.contains("/home/me/meetings"), "{enable}");
    let enable = enable.replace("/home/me/meetings", notes.to_str().unwrap());
    let cfg = store.join("rrecall.toml");
    let text = std::fs::read_to_string(&cfg).unwrap() + "\n" + &enable;
    std::fs::write(&cfg, text).unwrap();

    let o = w.rr(&["doctor", "--project", "lantern"], "");
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(o.stdout.contains("meeting-notes"), "{}", o.stdout);

    // The fake `claude` answers each call in turn: the collector, its resolve_prompt, then
    // the three judges `init` enabled.
    let src = format!("meeting:{}/2026-10-06-weekly-sync.md#L6", notes.display());
    let src = src.as_str();
    let write = |n: usize, v: Value| {
        std::fs::write(w.agent().join(format!("out.{n}")), v.to_string()).unwrap();
    };
    std::fs::write(w.agent().join("scenario"), "ok").unwrap();
    write(
        1,
        json!({"proposals": [{
            "key": "exporter", "op": "add", "parent": "decisions",
            "title": "Lantern exporter ships as its own binary",
            "body": "The weekly sync decided to ship the exporter as a separate binary so the core stays small.",
            "sources": [src], "date": "2026-10-06", "reason": "decided in the weekly sync"
        }]}),
    );
    write(
        2,
        json!({"texts": {src: "- Ship the lantern exporter as a separate binary."}}),
    );
    for n in 3..=5 {
        write(
            n,
            json!({"verdicts": [{"key": "meeting-notes/exporter", "verdict": "accept", "reason": "backed"}]}),
        );
    }
    let o = w.rr(&["run", "--project", "lantern"], "");
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert_eq!(calls(&w.agent()), 5);
    let prompt = std::fs::read_to_string(w.agent().join("claude.stdin.1")).unwrap();
    assert!(
        prompt.contains("# Collector: meeting-notes") && prompt.contains(notes.to_str().unwrap()),
        "the collector's prompt and its params reach the runner"
    );
    let map = std::fs::read_to_string(store.join("map/decisions.jsonl")).unwrap();
    assert!(
        map.contains("Lantern exporter ships as its own binary") && map.contains(src),
        "{map}"
    );

    // `min_interval = "1d"`: the next run the same day finds nothing due and calls no runner.
    let o = w.rr(&["run", "--project", "lantern"], "");
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert_eq!(calls(&w.agent()), 5);
}

//! The plugin author guide (`docs/plugins.md`) cannot drift from the code: every JSON example
//! in it goes through the real wire types (`rrecall::wire`), its copy of the example manifest
//! matches the file, and its steps install and run the example plugin with a fake runner.

use rrecall::config::BranchDef;
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
        "check-input",
        "judge-input",
        "verdict-output",
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
    assert!(judge.proposals.iter().all(|p| p.sources.is_some()));
    let check: wire::JudgeInput = de("check-input", e("check-input"));
    same("check-input", e("check-input"), &check);
    assert!(
        check.proposals.iter().all(|p| p.sources.is_none()),
        "a check gets no resolved sources"
    );
    let runner: wire::RunnerInput = de("runner-input", e("runner-input"));
    same("runner-input", e("runner-input"), &runner);
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
    let verdicts = wire::decode_verdicts("guide", &text("verdict-output"), &keys).unwrap();
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

// The guide's steps, run as written, and the payloads the pipeline really sends.

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

/// An isolated world: only `HOME` (so the XDG directories default under it) and `PATH` (the
/// rrecall under test and the fake agents first) are set; the work dir is outside any git
/// repository and holds a copy of `examples/`.
struct W {
    root: PathBuf,
}

impl Drop for W {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl W {
    fn new(name: &str) -> W {
        let root = std::env::temp_dir().join(format!("rrecall-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["home/agent", "work"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let root = root.canonicalize().unwrap();
        let o = Command::new("cp")
            .arg("-R")
            .arg(super_root().join("examples"))
            .arg(root.join("work"))
            .output()
            .unwrap();
        assert!(o.status.success());
        W { root }
    }

    fn agent(&self) -> PathBuf {
        self.root.join("home/agent")
    }

    fn store(&self) -> PathBuf {
        self.root.join("home/.local/share/rrecall/lantern")
    }

    fn notes(&self) -> PathBuf {
        self.root.join("work/examples/meeting-notes-data")
    }

    /// Runs `program args` in the work dir with `y` on stdin.
    fn exec(&self, program: &str, args: &[&str]) -> Out {
        let bin = PathBuf::from(env!("CARGO_BIN_EXE_rrecall"));
        let mut c = Command::new(program);
        c.args(args)
            .current_dir(self.root.join("work"))
            .env("HOME", self.root.join("home"))
            .env(
                "PATH",
                format!(
                    "{}:{}/tests/fixtures/bin:{}",
                    bin.parent().unwrap().display(),
                    env!("CARGO_MANIFEST_DIR"),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("RRECALL_PROJECT")
            .env_remove("GIT_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        {
            use std::io::Write;
            let mut si = child.stdin.take().unwrap();
            let _ = si.write_all(b"y\n");
        }
        let o = child.wait_with_output().unwrap();
        Out {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8(o.stdout).unwrap(),
            stderr: String::from_utf8(o.stderr).unwrap(),
        }
    }

    /// One of the guide's shell blocks, as a script that stops at the first failure.
    fn step(&self, script: &str) -> Out {
        let o = self.exec("sh", &["-ec", script]);
        assert_eq!(o.code, 0, "{script}\n{}{}", o.stdout, o.stderr);
        o
    }

    fn rr(&self, args: &[&str]) -> Out {
        let o = self.exec(env!("CARGO_BIN_EXE_rrecall"), args);
        assert_eq!(o.code, 0, "rrecall {args:?}\n{}{}", o.stdout, o.stderr);
        o
    }

    /// Appends the guide's `enable` block, with the example's notes as the folder.
    fn enable(&self) {
        let enable = blocks("toml", "step")["enable"].clone();
        assert!(enable.contains("/home/me/meetings"), "{enable}");
        let enable = enable.replace("/home/me/meetings", self.notes().to_str().unwrap());
        let cfg = self.store().join("rrecall.toml");
        let text = std::fs::read_to_string(&cfg).unwrap() + "\n" + &enable;
        std::fs::write(&cfg, text).unwrap();
    }
}

fn super_root() -> PathBuf {
    root()
}

fn calls(agent: &Path) -> usize {
    std::fs::read_to_string(agent.join("claude.calls"))
        .unwrap_or_default()
        .lines()
        .count()
}

#[test]
fn the_guides_steps_install_enable_and_run_the_example_plugin() {
    let w = W::new("guide-steps");
    let steps = blocks("sh", "step");

    let o = w.step(&steps["install"]);
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
    assert!(w.store().join("rrecall.toml").is_file());
    w.enable();

    // The fake `claude` answers each call in turn: the collector, its resolve_prompt, then
    // the three judges `init` enabled.
    let src = format!(
        "meeting:{}/2026-10-06-weekly-sync.md#L6",
        w.notes().display()
    );
    let write = |n: usize, v: Value| {
        std::fs::write(w.agent().join(format!("out.{n}")), v.to_string()).unwrap();
    };
    std::fs::write(w.agent().join("scenario"), "ok").unwrap();
    write(
        1,
        json!({"proposals": [{
            "key": "exporter", "op": "add", "parent": "decisions",
            "title": "Lantern exporter ships as its own binary",
            "body": "The weekly sync decided to ship the exporter as a separate binary.",
            "sources": [src], "date": "2026-10-06", "reason": "decided in the weekly sync"
        }]}),
    );
    write(
        2,
        json!({"texts": {&src: "- Ship the lantern exporter as a separate binary."}}),
    );
    for n in 3..=5 {
        write(
            n,
            json!({"verdicts": [{"key": "meeting-notes/exporter", "verdict": "accept", "reason": "backed"}]}),
        );
    }
    let o = w.step(&steps["run"]);
    assert!(o.stdout.contains("meeting-notes"), "doctor: {}", o.stdout);
    assert_eq!(calls(&w.agent()), 5);
    let prompt = std::fs::read_to_string(w.agent().join("claude.stdin.1")).unwrap();
    assert!(
        prompt.contains("# Collector: meeting-notes")
            && prompt.contains(w.notes().to_str().unwrap()),
        "the collector's prompt and its params reach the runner"
    );
    assert!(
        o.stdout
            .contains("Lantern exporter ships as its own binary")
            && o.stdout.contains(&src),
        "`show` prints the decision with its source: {}",
        o.stdout
    );

    // `min_interval = "1d"`: the same steps again the same day find nothing due.
    w.step(&steps["run"]);
    assert_eq!(calls(&w.agent()), 5);
}

/// A plugin directory under the store's `plugins/` with `manifest` and one script.
fn store_plugin(w: &W, id: &str, manifest: &str, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    let dir = w.store().join("plugins").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("rrecall-plugin.toml"), manifest).unwrap();
    let f = dir.join("run.sh");
    std::fs::write(&f, script).unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn keys(v: &Value) -> Vec<String> {
    v.as_object()
        .unwrap_or_else(|| panic!("not an object: {v}"))
        .keys()
        .cloned()
        .collect()
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// `+<n> ~<n> -<n>` of a commit subject `run <time>: +1 ~0 -0`.
fn counts(message: &str) -> Vec<char> {
    let subject = message.lines().next().unwrap_or_default();
    let (_, c) = subject.split_once(": ").expect(subject);
    c.split(' ')
        .take(3)
        .map(|t| {
            assert!(t[1..].parse::<u32>().is_ok(), "{subject}");
            t.chars().next().unwrap()
        })
        .collect()
}

#[test]
fn the_guides_runner_and_hook_examples_have_the_keys_the_pipeline_sends() {
    let ex = examples();
    let w = W::new("guide-pipeline");
    let rec = w.root.join("rec");
    std::fs::create_dir_all(&rec).unwrap();
    w.rr(&["plugin", "add", "examples/meeting-notes"]);
    w.rr(&["init", "lantern"]);
    // A runner and a `commit.before` hook that record their stdin and answer from files.
    let record = |what: &str, answer: &str| {
        format!(
            "#!/bin/sh\nn=1\nwhile [ -e \"$RRECALL_PARAM_REC/{what}.in.$n\" ]; do n=$((n + 1)); done\ncat > \"$RRECALL_PARAM_REC/{what}.in.$n\"\n{answer}\n"
        )
    };
    let head = |id: &str, kind: &str| {
        format!(
            "id = \"{id}\"\nname = \"Recorder\"\nversion = \"0.1.0\"\nmin_rrecall = \"0.1.0\"\nkind = \"{kind}\"\n\n"
        )
    };
    store_plugin(
        &w,
        "recorder",
        &(head("recorder", "runner")
            + "[runner]\ncommand = [\"./run.sh\"]\n\n[params]\nrec = \"\"\n"),
        &record("runner", "cat \"$RRECALL_PARAM_REC/runner.out.$n\""),
    );
    store_plugin(
        &w,
        "recorder-hook",
        &(head("recorder-hook", "hook")
            + "[hook]\nevents = [\"commit.before\"]\ncommand = [\"./run.sh\"]\n\n[params]\nrec = \"\"\n"),
        &record("hook", "echo '{}'"),
    );
    let rec_s = rec.to_str().unwrap();
    std::fs::write(
        w.store().join("rrecall.toml"),
        format!(
            "[[plugins]]\nid = \"no-op\"\n\n[[plugins]]\nid = \"recorder\"\nparams = {{ rec = {rec_s:?} }}\n\n[[plugins]]\nid = \"meeting-notes\"\nrunner = \"recorder\"\nparams = {{ path = {:?} }}\n\n[[plugins]]\nid = \"recorder-hook\"\nparams = {{ rec = {rec_s:?} }}\n",
            w.notes().to_str().unwrap()
        ),
    )
    .unwrap();

    // Run 1 adds a node, a forced run 2 updates it: the hook sees an `added` and an `updated`.
    let src = format!(
        "meeting:{}/2026-10-06-weekly-sync.md#L6",
        w.notes().display()
    );
    let answer = |n: usize, p: Value| {
        let out = json!({"output": {"proposals": [p]}, "cost_usd": 0, "model": "fake-1"});
        std::fs::write(rec.join(format!("runner.out.{n}")), out.to_string()).unwrap();
    };
    answer(
        1,
        json!({"key": "exporter", "op": "add", "parent": "decisions", "title": "Exporter ships alone", "date": "2026-10-06",
               "body": "The exporter ships as a separate binary.", "sources": [&src], "reason": "sync"}),
    );
    w.rr(&["run", "--project", "lantern"]);
    let map = std::fs::read_to_string(w.store().join("map/decisions.jsonl")).unwrap();
    let node: Value = serde_json::from_str(map.lines().next().unwrap()).unwrap();
    answer(
        2,
        json!({"key": "exporter", "op": "update", "id": node["id"], "body": "The exporter ships as its own binary.",
               "sources": [&src], "reason": "sync"}),
    );
    w.rr(&["run", "--project", "lantern", "--force"]);

    let read = |f: &str| -> Value {
        let text = std::fs::read_to_string(rec.join(f)).unwrap_or_else(|e| panic!("{f}: {e}"));
        serde_json::from_str(&text).unwrap()
    };

    // The runner envelope: the guide's keys and value types; its `input` is the collect input.
    let real = read("runner.in.2");
    let guide = &ex["runner-input"];
    assert_eq!(keys(guide), keys(&real), "runner-input");
    for k in keys(guide) {
        assert_eq!(kind(&guide[&k]), kind(&real[&k]), "runner-input `{k}`");
    }
    let collect = &ex["collect-input"];
    assert_eq!(
        keys(collect),
        keys(&real["input"]),
        "collect-input, as the runner gets it"
    );
    assert_eq!(keys(&collect["scope"][0]), keys(&real["input"]["scope"][0]));
    assert_eq!(keys(&collect["nodes"][0]), keys(&real["input"]["nodes"][0]));

    // The hook input: the guide's keys at every level, diff entry by kind of change.
    let guide = &ex["hook-input"];
    let runs = [read("hook.in.1"), read("hook.in.2")];
    for real in &runs {
        assert_eq!(keys(guide), keys(real), "hook-input");
        assert_eq!(real["event"], guide["event"]);
        assert_eq!(
            keys(&guide["data"]),
            keys(&real["data"]),
            "hook-input `data`"
        );
        assert_eq!(
            counts(guide["data"]["message"].as_str().unwrap()),
            counts(real["data"]["message"].as_str().unwrap()),
            "the commit subject's counts"
        );
    }
    for g in guide["data"]["diff"].as_array().unwrap() {
        let real = runs
            .iter()
            .flat_map(|r| r["data"]["diff"].as_array().unwrap())
            .find(|d| d["change"] == g["change"])
            .unwrap_or_else(|| panic!("no `{}` entry captured", g["change"]));
        assert_eq!(
            keys(g),
            keys(real),
            "hook-input diff entry `{}`",
            g["change"]
        );
    }
}

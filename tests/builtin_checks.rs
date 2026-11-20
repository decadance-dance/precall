//! The built-in checks run as plugins: discovered from the binary, started by the process
//! runner, speaking the check protocol of spec §6.5.

use precall::config::UserConfig;
use precall::plugin::{self, Spec};
use precall::process::{self, Ctx};
use precall::wire::{self, JudgeInput, VerdictKind};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_precall");

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn each_builtin_check_runs_through_the_process_runner() {
    let root = Dir(std::env::temp_dir().join(format!("precall-builtin-{}", std::process::id())));
    let store = root.0.join("store");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        store.join("precall.toml"),
        "[[plugins]]\nid = \"no-op\"\n\n[[plugins]]\nid = \"size-limits\"\nparams = { body_max = 20 }\n\n[[plugins]]\nid = \"secrets\"\n",
    )
    .unwrap();
    let ins = plugin::load_with(&store, &UserConfig::default(), &root.0.join("user")).unwrap();
    let ctx = Ctx {
        project: "lanternfish".into(),
        store: store.clone(),
        run_id: "r1".into(),
        run_dir: store.join("runs/r1"),
    };
    let secret = ["xo", "xb-1234567890-", "Zq3vK9mW2xR7tY5pL8nB"].concat();
    let input: JudgeInput = serde_json::from_value(serde_json::json!({
        "proposals": [
            { "key": "repo/same", "proposal": { "key": "same", "op": "update", "id": "fetcher",
                "title": "fetcher", "sources": ["file:src/fetch.rs@a1"], "reason": "r" },
              "current": { "id": "fetcher", "parent": "layout", "title": "Fetcher",
                "body": "Polls feeds.", "sources": ["file:src/fetch.rs@a1"], "order": 1 } },
            { "key": "repo/new", "proposal": { "key": "new", "op": "add", "parent": "layout",
                "title": "Web client", "body": "Svelte app under web/ with a long body.",
                "sources": ["file:web/app.js@a1"], "reason": format!("found {secret}") } }
        ],
        "map_ids": ["layout", "fetcher"],
        "params": {}
    }))
    .unwrap();
    let keys = ["repo/same".to_string(), "repo/new".to_string()];
    let mut got = Vec::new();
    for mut inst in ins {
        assert!(inst.plugin.builtin, "{}", inst.id);
        let Spec::Check(c) = &mut inst.plugin.manifest.spec else {
            panic!("{} is not a check", inst.id)
        };
        // In a test the running binary is the harness; point the program at precall.
        c.command[0] = BIN.to_string();
        let command = c.command.clone();
        let input = JudgeInput {
            params: inst.params_json(),
            ..serde_json::from_value(serde_json::to_value(&input).unwrap()).unwrap()
        };
        let out = process::call(&ctx, &inst, "check", &command, &input)
            // The error carries the check's stderr; a fixed message keeps a leaked secret out.
            .unwrap_or_else(|_| panic!("check `{}` failed to run", inst.id));
        let v = wire::decode_verdicts(&inst.id, &out, &keys).unwrap();
        got.push((
            inst.id.clone(),
            v[0].verdict,
            v[1].verdict,
            v[1].reason.clone(),
        ));
    }
    use VerdictKind::{Accept, Reject};
    assert_eq!(got[0].0, "no-op");
    assert_eq!((got[0].1, got[0].2), (Reject, Accept));
    assert_eq!(got[1].0, "size-limits");
    assert_eq!((got[1].1, got[1].2), (Accept, Reject), "{}", got[1].3);
    assert!(got[1].3.contains("over the limit of 20"), "{}", got[1].3);
    assert_eq!(got[2].0, "secrets");
    assert_eq!((got[2].1, got[2].2), (Accept, Reject));
    assert!(got[2].3.contains("slack-token") && !got[2].3.contains(&secret[5..]));
    // The secret reached no log either: stdout and stderr of the check hold only the verdicts.
    for ext in ["stdout", "stderr"] {
        let log =
            std::fs::read_to_string(store.join(format!("runs/r1/secrets.check.{ext}"))).unwrap();
        assert!(
            !log.contains(&secret[5..]),
            "secrets.check.{ext} echoes the secret"
        );
    }
}

#[test]
fn the_builtin_subcommand_is_hidden_and_refuses_unknown_ids() {
    let help = Command::new(BIN).arg("--help").output().unwrap();
    assert!(!String::from_utf8_lossy(&help.stdout).contains("builtin"));
    let mut child = Command::new(BIN)
        .args(["builtin", "no-such-check"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no-such-check"));
    assert!(out.stdout.is_empty());
}

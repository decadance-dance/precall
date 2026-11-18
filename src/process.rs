//! The process contract of spec §6.3 and the hooks of §6.6.

use crate::error;
use crate::plugin::{Instance, Spec};
use crate::wire::{self, Event, HookInput};
use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use std::fs::File;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// What every plugin process of one run shares.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub project: String,
    pub store: PathBuf,
    pub run_id: String,
    /// `runs/<run-id>/` in the store: each call's stdout and stderr land here.
    pub run_dir: PathBuf,
}

/// More stdout than this is not a plugin answer; the call fails instead of filling memory.
const MAX_STDOUT: u64 = 64 << 20;

/// Runs `template` (an argv of `inst`'s manifest) as a child process (spec §6.3): in the plugin
/// directory, never through a shell, `input` as JSON on stdin, the `RRECALL_*` environment, and
/// killed with everything it started at the instance's timeout. stdout and stderr go to
/// `<run_dir>/<instance>.<entry>.stdout|stderr` (a later call with the same `entry` replaces
/// them). Returns stdout of a run that exited 0.
pub fn call(
    ctx: &Ctx,
    inst: &Instance,
    entry: &str,
    template: &[String],
    input: &impl Serialize,
) -> Result<String> {
    run(ctx, inst, entry, template, input)
        .with_context(|| format!("plugin `{}` ({entry})", inst.id))
}

fn run(
    ctx: &Ctx,
    inst: &Instance,
    entry: &str,
    template: &[String],
    input: &impl Serialize,
) -> Result<String> {
    let argv = inst.argv(template);
    let (program, args) = argv.split_first().context("empty argv")?;
    let stdin = serde_json::to_vec(input)?;
    let state = ctx.store.join("state").join(&inst.id);
    std::fs::create_dir_all(&state).with_context(|| format!("creating {}", state.display()))?;
    std::fs::create_dir_all(&ctx.run_dir)
        .with_context(|| format!("creating {}", ctx.run_dir.display()))?;
    let log = |ext| ctx.run_dir.join(format!("{}.{entry}.{ext}", inst.id));
    let (out_path, err_path) = (log("stdout"), log("stderr"));
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(&inst.plugin.dir)
        .stdin(Stdio::piped())
        .stdout(
            File::create(&out_path).with_context(|| format!("creating {}", out_path.display()))?,
        )
        .stderr(
            File::create(&err_path).with_context(|| format!("creating {}", err_path.display()))?,
        )
        .process_group(0);
    // A param the instance does not set must not leak in from rrecall's own environment.
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("RRECALL_PARAM_") {
            cmd.env_remove(k);
        }
    }
    cmd.env(
        "RRECALL_BIN",
        std::env::current_exe().context("locating the rrecall binary")?,
    )
    .env("RRECALL_PROJECT", &ctx.project)
    .env("RRECALL_STORE", &ctx.store)
    .env("RRECALL_PLUGIN_DIR", &inst.plugin.dir)
    .env("RRECALL_STATE_DIR", &state)
    .env("RRECALL_RUN_ID", &ctx.run_id)
    .envs(inst.env_params());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("cannot start `{program}`"))?;
    // A writer thread, so a child that never reads stdin cannot block the timeout below.
    if let Some(mut pipe) = child.stdin.take() {
        std::thread::spawn(move || pipe.write_all(&stdin));
    }
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= inst.timeout {
            kill_group(&mut child);
            bail!(
                "timed out after {:?}; stderr in {}",
                inst.timeout,
                err_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if !status.success() {
        let stderr = std::fs::read_to_string(&err_path).unwrap_or_default();
        let last = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        bail!("{status}: {last} (stderr in {})", err_path.display());
    }
    let size = std::fs::metadata(&out_path)?.len();
    if size > MAX_STDOUT {
        bail!("printed {size} bytes, more than {MAX_STDOUT}");
    }
    let bytes =
        std::fs::read(&out_path).with_context(|| format!("reading {}", out_path.display()))?;
    String::from_utf8(bytes).map_err(|_| anyhow!("output is not UTF-8"))
}

/// Kills `child`, which leads its own process group, and everything it started.
fn kill_group(child: &mut Child) {
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{}", child.id())])
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

/// Runs the hook instances subscribed to `event`, in config order (spec §6.6). On a `*.before`
/// event the first hook that denies, fails, times out or prints anything but `{}` or
/// `{"deny": "<reason>"}` aborts the run (exit 4). On other events a failure is only returned,
/// one line per hook, for the report.
pub fn run_hooks(
    ctx: &Ctx,
    instances: &[Instance],
    event: Event,
    data: &serde_json::Value,
) -> Result<Vec<String>> {
    let input = HookInput {
        event,
        project: ctx.project.clone(),
        run_id: ctx.run_id.clone(),
        data: data.clone(),
    };
    let name = event.name();
    let mut warnings = Vec::new();
    for inst in instances {
        let Spec::Hook(h) = &inst.plugin.manifest.spec else {
            continue;
        };
        if !h.events.contains(&event) {
            continue;
        }
        let out = call(ctx, inst, &name, &h.command, &input);
        if event.can_deny() {
            let deny = out
                .and_then(|o| wire::decode_hook(&inst.id, &o))
                .map_err(|e| {
                    error::aborted(format!(
                        "hook `{}` failed on `{name}`, which denies: {e:#}",
                        inst.id
                    ))
                })?;
            if let Some(reason) = deny {
                return Err(error::aborted(format!(
                    "hook `{}` denied `{name}`: {reason}",
                    inst.id
                )));
            }
        } else if let Err(e) = out {
            warnings.push(format!("hook `{}` failed on `{name}`: {e:#}", inst.id));
        }
    }
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{self, Instance};
    use crate::testutil::TempDir;
    use crate::wire;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn fake() -> String {
        format!(
            "{}/tests/fixtures/bin/fake-plugin",
            env!("CARGO_MANIFEST_DIR")
        )
    }

    struct World {
        t: TempDir,
    }

    impl World {
        fn new() -> World {
            let t = TempDir::new();
            std::fs::create_dir_all(t.0.join("store/plugins")).unwrap();
            World { t }
        }
        fn store(&self) -> PathBuf {
            self.t.0.join("store")
        }
        fn run_dir(&self) -> PathBuf {
            self.store().join("runs/r1")
        }
        fn record(&self, inst: &str) -> PathBuf {
            self.t.0.join(format!("{inst}.rec"))
        }
        /// A plugin `id` of `kind` whose entrypoint is the fake, with `extra` manifest lines.
        fn plugin(&self, id: &str, table: &str) {
            let d = self.store().join("plugins").join(id);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join(plugin::MANIFEST),
                format!(
                    "id = \"{id}\"\nname = \"N\"\nversion = \"0.1.0\"\nmin_rrecall = \"0.1.0\"\nkind = \"{}\"\n{table}\n[params]\nscenario = \"ok\"\nout = \"\"\nrecord = \"\"\nlabel-x = \"\"\n",
                    table.trim_start_matches('[').split(']').next().unwrap()
                ),
            )
            .unwrap();
        }
        /// The instance `id` of plugin `uses` with the scenario and canned output.
        fn instance(&self, id: &str, uses: &str, scenario: &str, out: &str) -> Instance {
            let cfg: crate::config::StoreConfig = toml::from_str(&format!(
                "[[plugins]]\nid = \"{id}\"\nuse = \"{uses}\"\nparams = {{ scenario = \"{scenario}\", out = {}, record = {} }}\n",
                crate::config::toml_str(out),
                crate::config::toml_str(&self.record(id).to_string_lossy())
            ))
            .unwrap();
            let found = plugin::discover(&[self.store().join("plugins")]).unwrap();
            plugin::instances(&cfg.plugins, &found, &cfg.branches().unwrap())
                .unwrap()
                .remove(0)
        }
        fn ctx(&self) -> Ctx {
            Ctx {
                project: "kiln".into(),
                store: self.store(),
                run_id: "r1".into(),
                run_dir: self.run_dir(),
            }
        }
    }

    fn collector(w: &World) {
        w.plugin(
            "gh",
            &format!(
                "[collector]\nscope = [\"done\"]\nsource_kinds = [\"gh-pr\"]\nfingerprint = [\"git\"]\ncommand = [{}, \"--repo\", \"{{label-x}}\"]\n",
                crate::config::toml_str(&fake())
            ),
        );
    }

    #[test]
    fn valid_output_with_argv_env_stdin_and_stderr_recorded() {
        let w = World::new();
        collector(&w);
        let inst = w.instance("prs", "gh", "ok", r#"{"proposals":[]}"#);
        let input = wire::ResolveInput {
            refs: vec!["gh-pr:o/r#1".into()],
        };
        let out = call(&w.ctx(), &inst, "collect", &command(&inst), &input).unwrap();
        assert!(wire::decode_proposals(&inst.id, &out).unwrap().is_empty());

        let rec = std::fs::read_to_string(w.record("prs")).unwrap();
        assert!(
            rec.contains(&format!("argv: [{}] [--repo] []\n", fake())),
            "{rec}"
        );
        let dir = inst.plugin.dir.canonicalize().unwrap();
        assert!(rec.contains(&format!("cwd: {}\n", dir.display())), "{rec}");
        let store = w.store();
        for line in [
            format!("RRECALL_BIN={}", std::env::current_exe().unwrap().display()),
            "RRECALL_PROJECT=kiln".into(),
            format!("RRECALL_STORE={}", store.display()),
            format!("RRECALL_PLUGIN_DIR={}", inst.plugin.dir.display()),
            format!("RRECALL_STATE_DIR={}", store.join("state/prs").display()),
            "RRECALL_RUN_ID=r1".into(),
            "RRECALL_PARAM_SCENARIO=ok".into(),
            "RRECALL_PARAM_LABEL_X=".into(),
        ] {
            assert!(rec.lines().any(|l| l == line), "{line} in {rec}");
        }
        assert!(store.join("state/prs").is_dir(), "the state dir is made");
        let stdin = std::fs::read_to_string(w.record("prs").with_extension("rec.stdin")).unwrap();
        assert_eq!(stdin, r#"{"refs":["gh-pr:o/r#1"]}"#);
        assert_eq!(
            std::fs::read_to_string(w.run_dir().join("prs.collect.stderr")).unwrap(),
            "fake plugin: scenario ok\n"
        );
    }

    fn command(inst: &Instance) -> Vec<String> {
        match &inst.plugin.manifest.spec {
            plugin::Spec::Collector(c) => c.command.clone().unwrap(),
            plugin::Spec::Hook(h) => h.command.clone(),
            _ => panic!(),
        }
    }

    #[test]
    fn malformed_output_and_unknown_fields_are_rejected_by_name() {
        let w = World::new();
        collector(&w);
        for (out, want) in [
            ("{\"proposals\": [", "not one JSON document"),
            (
                r#"{"proposals":[{"key":"a","op":"remove","id":"x","sources":[],"reason":"r","colour":"red"}]}"#,
                "unknown field `colour`",
            ),
        ] {
            let inst = w.instance("prs", "gh", "ok", out);
            let text = call(&w.ctx(), &inst, "collect", &command(&inst), &"{}").unwrap();
            let e = wire::decode_proposals(&inst.id, &text)
                .unwrap_err()
                .to_string();
            assert!(e.contains("plugin `prs`") && e.contains(want), "{e}");
        }
    }

    #[test]
    fn a_crash_names_the_plugin_and_keeps_stderr() {
        let w = World::new();
        collector(&w);
        let inst = w.instance("prs", "gh", "crash", "");
        let e = format!(
            "{:#}",
            call(&w.ctx(), &inst, "collect", &command(&inst), &"{}").unwrap_err()
        );
        assert!(e.contains("plugin `prs`") && e.contains("collect"), "{e}");
        assert!(
            e.contains("3") && e.contains("fake plugin: scenario crash"),
            "{e}"
        );
        assert!(w.run_dir().join("prs.collect.stderr").is_file());
    }

    #[test]
    fn a_missing_program_is_an_error_not_a_panic() {
        let w = World::new();
        w.plugin(
            "gh",
            "[collector]\nscope = [\"done\"]\nsource_kinds = [\"gh-pr\"]\nfingerprint = [\"git\"]\ncommand = [\"./absent.sh\"]\n",
        );
        let inst = w.instance("prs", "gh", "ok", "");
        let e = format!(
            "{:#}",
            call(&w.ctx(), &inst, "collect", &command(&inst), &"{}").unwrap_err()
        );
        assert!(e.contains("plugin `prs`") && e.contains("absent.sh"), "{e}");
    }

    #[test]
    fn a_timeout_kills_the_process_group() {
        let w = World::new();
        collector(&w);
        let mut inst = w.instance("prs", "gh", "sleep", "");
        inst.timeout = Duration::from_millis(300);
        let started = Instant::now();
        let e = format!(
            "{:#}",
            call(&w.ctx(), &inst, "collect", &command(&inst), &"{}").unwrap_err()
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert!(e.contains("plugin `prs`") && e.contains("timed out"), "{e}");
        // What the plugin started is gone too.
        let pid = std::fs::read_to_string(w.record("prs").with_extension("rec.pid")).unwrap();
        let alive = || {
            std::process::Command::new("kill")
                .args(["-0", pid.trim()])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        while alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive(), "the plugin's child {pid} survived the timeout");
    }

    fn hook(w: &World, id: &str, events: &str) {
        w.plugin(
            id,
            &format!(
                "[hook]\nevents = [{events}]\ncommand = [{}]\n",
                crate::config::toml_str(&fake())
            ),
        );
    }

    #[test]
    fn before_hooks_deny_and_fail_closed() {
        let w = World::new();
        hook(&w, "gate", "\"run.before\", \"commit.after\"");
        let data = serde_json::json!({});
        let allow = w.instance("gate", "gate", "ok", "{}");
        assert_eq!(
            run_hooks(&w.ctx(), &[allow], Event::RunBefore, &data).unwrap(),
            Vec::<String>::new()
        );
        assert!(w.run_dir().join("gate.run.before.stderr").is_file());
        let stdin = std::fs::read_to_string(w.record("gate").with_extension("rec.stdin")).unwrap();
        assert_eq!(
            stdin,
            r#"{"event":"run.before","project":"kiln","run_id":"r1","data":{}}"#
        );

        for (scenario, out, want) in [
            ("ok", r#"{"deny":"store frozen"}"#, "store frozen"),
            ("ok", "", "does not decode"),
            ("ok", r#"{"deny":"x","extra":1}"#, "does not decode"),
            ("crash", "", "exit"),
            ("sleep", "", "timed out"),
        ] {
            let mut inst = w.instance("gate", "gate", scenario, out);
            inst.timeout = Duration::from_millis(300);
            let e = run_hooks(&w.ctx(), &[inst], Event::RunBefore, &data).unwrap_err();
            assert_eq!(crate::error::exit_code(&e), 4, "{e:#}");
            let e = format!("{e:#}");
            assert!(
                e.contains("hook `gate`") && e.contains("run.before") && e.contains(want),
                "{e}"
            );
        }
        // A hook not subscribed to the event does not run.
        let crash = w.instance("gate", "gate", "crash", "");
        assert!(run_hooks(&w.ctx(), &[crash], Event::CommitBefore, &data).is_ok());
    }

    #[test]
    fn hooks_run_in_order_and_the_first_deny_stops() {
        let w = World::new();
        hook(&w, "gate", "\"commit.before\"");
        let first = w.instance("first", "gate", "ok", r#"{"deny":"no"}"#);
        let second = w.instance("second", "gate", "ok", "{}");
        let e = format!(
            "{:#}",
            run_hooks(
                &w.ctx(),
                &[first, second],
                Event::CommitBefore,
                &serde_json::json!({})
            )
            .unwrap_err()
        );
        assert!(e.contains("hook `first`"), "{e}");
        assert!(!w.record("second").exists(), "the second hook never ran");
    }

    #[test]
    fn observe_hooks_only_report_failures() {
        let w = World::new();
        hook(&w, "audit", "\"commit.after\"");
        let ok = w.instance("quiet", "audit", "ok", "not json at all");
        let bad = w.instance("loud", "audit", "crash", "");
        let warnings = run_hooks(
            &w.ctx(),
            &[ok, bad],
            Event::CommitAfter,
            &serde_json::json!({"commit": "abc"}),
        )
        .unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("hook `loud`") && warnings[0].contains("commit.after"),
            "{warnings:?}"
        );
    }
}

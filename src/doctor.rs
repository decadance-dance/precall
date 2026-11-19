//! `rrecall doctor` (spec §2): whether each enabled plugin instance can run, and what a run
//! would skip. It runs no model: runner logins are checked with the agents' own status commands,
//! and a collector's access with its fingerprint.

use crate::config;
use crate::error::{self, Fail};
use crate::plugin::{self, Instance, Spec};
use crate::process::{self, Ctx};
use crate::run;
use crate::wire::PluginInput;
use anyhow::{Context, Result};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// One check's line: passed or not, and what it says.
type Line = (bool, String);

pub fn doctor(project: Option<&str>) -> Result<String> {
    // A user config a run cannot load exits 4, as the plugin loader says; project resolution
    // reads it too, and would exit 1 first.
    let user = config::load_user_config().map_err(|e| error::aborted(format!("{e:#}")))?;
    let (name, store) = config::resolve_project(project)?;
    let instances = plugin::load(&store)?;
    // The rest of a run's step 1 that concerns plugins (spec §6.8): a run would abort on it.
    run::check_allowed(&instances, &user).map_err(error::aborted)?;
    let ctx = Ctx {
        project: name,
        store: store.clone(),
        run_id: "doctor".into(),
        // Not under runs/: `status` reads the last run there, and doctor is no run.
        run_dir: store.join("cache/doctor"),
    };
    // One doctor at a time per store: each clears and fills the same scratch and log dir.
    std::fs::create_dir_all(&ctx.run_dir)
        .with_context(|| format!("creating {}", ctx.run_dir.display()))?;
    let _held = crate::lock::lock_file(&store.join("cache/doctor.lock"), true)?;
    // Fingerprints get a fresh state dir each time, so a probe never changes what a run reads.
    let scratch = ctx.run_dir.join("state");
    match std::fs::remove_dir_all(&scratch) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("removing {}", scratch.display()));
        }
        _ => {}
    }
    let cwd = std::env::current_dir().context("reading the working directory")?;
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let mut lines: Vec<Vec<Line>> = instances
        .iter()
        .map(|i| own_checks(&ctx, i, &path_var, &cwd))
        .collect();
    // A declarative plugin runs only if the runner it calls can.
    for (k, inst) in instances.iter().enumerate() {
        if !inst.plugin.manifest.declarative() {
            continue;
        }
        let line = match run::runner_of(&instances, &user, inst) {
            Err(e) => (false, format!("runner: {e}")),
            Ok(r) => {
                let j = instances.iter().position(|x| x.id == r.id).unwrap_or(k);
                if lines[j].iter().all(|l| l.0) {
                    (true, format!("runner {}", r.id))
                } else {
                    (false, format!("runner {}: cannot run", r.id))
                }
            }
        };
        lines[k].push(line);
    }
    let mut out = String::new();
    let mut skipped = Vec::new();
    for (inst, lines) in instances.iter().zip(&lines) {
        out += &inst.id;
        if inst.plugin.manifest.id != inst.id {
            out += &format!(" ({})", inst.plugin.manifest.id);
        }
        out.push('\n');
        if lines.is_empty() {
            out += "  ok    nothing to check\n";
        }
        for (ok, text) in lines {
            out += &format!("  {:<6}{text}\n", if *ok { "ok" } else { "fail" });
        }
        if lines.iter().any(|l| !l.0) {
            skipped.push(inst.id.clone());
        }
    }
    if skipped.is_empty() {
        return Ok(out + "Every enabled plugin can run.\n");
    }
    print!("{out}");
    Err(Fail {
        code: 1,
        msg: format!("a run would skip: {}", skipped.join(", ")),
    }
    .into())
}

/// The checks that need no other instance: required binaries, programs, a built-in runner's
/// login, a collector's fingerprint.
fn own_checks(ctx: &Ctx, inst: &Instance, path_var: &std::ffi::OsStr, cwd: &Path) -> Vec<Line> {
    let m = &inst.plugin.manifest;
    let dir = &inst.plugin.dir;
    let check = |label: String, r: Result<()>| match r {
        Ok(()) => (true, label),
        Err(e) => (false, format!("{label}: {e:#}")),
    };
    let mut lines = Vec::new();
    for r in &m.requires {
        let found = process::resolve_program(r, dir, path_var, cwd).map(|_| ());
        lines.push(check(format!("requires {r}"), found));
    }
    // A built-in's program is the running binary.
    if !inst.plugin.builtin {
        for (key, argv) in m.argvs() {
            let argv = inst.argv(argv);
            let found = process::resolve_program(&argv[0], dir, path_var, cwd).map(|_| ());
            lines.push(check(format!("program {} ({key})", argv[0]), found));
        }
    }
    let status: Option<&[&str]> = match (inst.plugin.builtin, m.id.as_str()) {
        (true, "claude") => Some(&["claude", "auth", "status"]),
        (true, "codex") => Some(&["codex", "login", "status"]),
        _ => None,
    };
    if let Some(argv) = status {
        lines.push(check(
            format!("login ({})", argv.join(" ")),
            logged_in(argv, dir, path_var, cwd),
        ));
    }
    if let Spec::Collector(c) = &m.spec {
        match &c.fingerprint {
            Some(fp) => {
                let input = PluginInput {
                    project: ctx.project.clone(),
                    run_id: ctx.run_id.clone(),
                    params: inst.params_json(),
                };
                let state = ctx.run_dir.join("state").join(&inst.id);
                let reached =
                    process::call_in(ctx, inst, "fingerprint", fp, &input, &state).map(|_| ());
                lines.push(check("fingerprint".into(), reached));
            }
            // limit: a source without a fingerprint has no cheap read, so only a run reaches it.
            None => lines.push((
                true,
                "fingerprint: none (min_interval); a run checks its access".into(),
            )),
        }
    }
    lines
}

/// Runs an agent's own login status command, which makes no model call: exit 0 is logged in.
fn logged_in(argv: &[&str], dir: &Path, path_var: &std::ffi::OsStr, cwd: &Path) -> Result<()> {
    let program = process::resolve_program(argv[0], dir, path_var, cwd)?;
    let mut cmd = Command::new(program);
    cmd.arg0(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let status = process::wait(&mut cmd, argv[0], Vec::new(), Duration::from_secs(30))?;
    anyhow::ensure!(status.success(), "not logged in ({status})");
    Ok(())
}

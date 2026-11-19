//! `rrecall run`: the pipeline of spec §4.1 over the store's enabled plugin instances. The
//! invariants and the merge live in `merge`; this module runs the plugins around them, loops
//! with back pressure, and writes the lock, the commit and the run report.

use crate::config::{self, BranchDef, UserConfig};
use crate::error::{self, Fail};
use crate::lock::{self, CollectorLock, LockFile, PluginLock};
use crate::merge::{self, Effect, Grant, Item, Rules};
use crate::plugin::{self, Instance, Kind, Spec};
use crate::process::{self, Ctx};
use crate::store::{self, Store};
use crate::wire::{
    self, CollectorInput, Event, JudgeInput, JudgeItem, PluginInput, Proposal, Rejection,
    ResolveInput, RoundVerdict, RunnerInput, Verdict, VerdictKind,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub struct Opts {
    pub only: Option<String>,
    pub force: bool,
    pub wait: bool,
}

const DEFAULT_ROUNDS: u32 = 2;

/// Runs the pipeline on the resolved project's store; returns the summary for stdout, or an
/// error carrying exit 4 (aborted) or 5 (committed, but not everything ran).
pub fn run(project: Option<&str>, opts: &Opts) -> Result<String> {
    let (name, root) = config::resolve_project(project)?;
    let Some(_held) = lock::acquire(&root, opts.wait)? else {
        return Err(error::aborted(format!(
            "another run holds the store lock of `{name}`; run again without --no-wait to wait for it"
        )));
    };
    let started = Instant::now();
    let t0 = lock::now();
    let run_id = fresh_run_id(&root, t0)?;
    let mut r = Run {
        ctx: Ctx {
            project: name,
            store: root.clone(),
            run_id: run_id.clone(),
            run_dir: root.join("runs").join(&run_id),
        },
        t0,
        user: UserConfig::default(),
        instances: Vec::new(),
        hashes: BTreeMap::new(),
        budget: Budget::default(),
        log: BTreeMap::new(),
        skipped: Vec::new(),
        collected: BTreeMap::new(),
        unreachable: BTreeMap::new(),
        unsettled: BTreeMap::new(),
        models: BTreeMap::new(),
        warnings: Vec::new(),
        commit: None,
    };
    let result = r.pipeline(opts);
    let report = r.report(&result, started.elapsed().as_secs_f64());
    let path = r.ctx.run_dir.join("report.md");
    let written =
        std::fs::write(&path, &report).with_context(|| format!("writing {}", path.display()));
    let code = match &result {
        Ok(done) => done.code,
        Err(_) => 0,
    };
    let summary = format!(
        "{}report: runs/{run_id}/report.md\n",
        summary_of(&report).trim_start_matches(&format!("# Run {run_id}\n\n"))
    );
    result?;
    written?;
    if code == 5 {
        return Err(Fail {
            code: 5,
            msg: summary,
        }
        .into());
    }
    Ok(summary)
}

/// The run id: the start time, with `-2`, `-3`, ... when a run of the same second exists.
fn fresh_run_id(root: &Path, t0: u64) -> Result<String> {
    let runs = root.join("runs");
    std::fs::create_dir_all(&runs).with_context(|| format!("creating {}", runs.display()))?;
    let base = lock::run_id(t0);
    // limit: a tenth run in one second sorts before the ninth (`-10` < `-9`) for `status`.
    for n in 1.. {
        let id = if n == 1 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        match std::fs::create_dir(runs.join(&id)) {
            Ok(()) => return Ok(id),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("creating runs/{id}")),
        }
    }
    unreachable!("an unbounded range ends only by returning")
}

/// The report's head: the title and the `## Summary` section, without the proposals.
pub fn summary_of(report: &str) -> &str {
    report.split("\n## Proposals").next().unwrap_or(report)
}

/// Why a plugin call gave no answer.
enum CallFail {
    /// The cost cap was reached before a runner call (spec §9).
    Budget,
    Error(String),
}

impl From<anyhow::Error> for CallFail {
    fn from(e: anyhow::Error) -> CallFail {
        CallFail::Error(format!("{e:#}"))
    }
}

/// The cost cap: checked before each runner call; calls in flight may finish.
#[derive(Default)]
struct Budget {
    cap: Option<f64>,
    /// Total spent and the cost per plugin instance.
    spent: Mutex<(f64, BTreeMap<String, f64>)>,
    refused: AtomicBool,
}

impl Budget {
    fn reserve(&self) -> Result<(), CallFail> {
        let spent = self.spent.lock().unwrap_or_else(|p| p.into_inner()).0;
        if self.cap.is_some_and(|cap| spent >= cap) {
            self.refused.store(true, Ordering::SeqCst);
            return Err(CallFail::Budget);
        }
        Ok(())
    }

    fn spend(&self, who: &str, cost: f64) {
        let mut s = self.spent.lock().unwrap_or_else(|p| p.into_inner());
        s.0 += cost;
        *s.1.entry(who.to_string()).or_default() += cost;
    }
}

/// How a proposal ended a round.
#[derive(Debug, Clone, PartialEq)]
enum Outcome {
    Accepted,
    Rejected,
    /// Not judged to the end: a check or judge failed, the budget ran out, or its collector did.
    Dropped(String),
    /// Sent back, but the collector did not propose it again.
    Withdrawn,
}

/// What a collector returned: its proposals, and the runner and model when a runner ran it.
type Collected = (Vec<Proposal>, Option<(String, String)>);

struct RoundRec {
    round: u32,
    proposal: Proposal,
    verdicts: Vec<RoundVerdict>,
    outcome: Outcome,
}

/// Whether a collector is collected this run.
enum Due {
    Yes(String),
    Skip(String),
    Unreachable(String),
}

struct Done {
    code: u8,
    nothing_due: bool,
}

struct Run {
    ctx: Ctx,
    t0: u64,
    user: UserConfig,
    instances: Vec<Instance>,
    /// Content hash of each collector, check, judge and view instance's plugin.
    hashes: BTreeMap<String, String>,
    budget: Budget,
    /// Every proposal by full key, round by round.
    log: BTreeMap<String, Vec<RoundRec>>,
    skipped: Vec<(String, String)>,
    /// Collector instance to the rounds it was collected in.
    collected: BTreeMap<String, Vec<u32>>,
    unreachable: BTreeMap<String, String>,
    unsettled: BTreeMap<String, String>,
    /// Collector instance to the runner and the model it reported.
    models: BTreeMap<String, (String, String)>,
    warnings: Vec<String>,
    commit: Option<(String, String)>,
}

/// `name`, and `name.N` from round 2 on (spec §6.3 log names).
fn entry(name: &str, round: u32) -> String {
    if round <= 1 {
        name.to_string()
    } else {
        format!("{name}.{round}")
    }
}

fn sha256(text: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hex: String = Sha256::digest(text)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

/// Runs `f` on each item in its own thread; results in item order.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    std::thread::scope(|s| {
        let handles: Vec<_> = items.iter().map(|it| s.spawn(|| f(it))).collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    })
}

/// One line of text: whitespace collapsed, at most `max` characters.
fn one_line(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}...", &s[..i]),
        None => s,
    }
}

impl Run {
    fn collector_spec(inst: &Instance) -> Option<&plugin::CollectorSpec> {
        match &inst.plugin.manifest.spec {
            Spec::Collector(c) => Some(c),
            _ => None,
        }
    }

    fn of_kind(&self, kind: Kind) -> Vec<Instance> {
        self.instances
            .iter()
            .filter(|i| i.plugin.manifest.kind() == kind)
            .cloned()
            .collect()
    }

    /// The prompt file of a declarative collector or judge.
    fn prompt_of(inst: &Instance) -> Option<&str> {
        match &inst.plugin.manifest.spec {
            Spec::Collector(c) => c.prompt.as_deref(),
            Spec::Judge(j) => j.prompt.as_deref(),
            _ => None,
        }
    }

    /// The runner and model a declarative plugin is configured with: its instance, else its
    /// manifest, else the user config (spec §6.1, §9).
    fn configured(&self, inst: &Instance) -> (Option<String>, Option<String>) {
        if Self::prompt_of(inst).is_none() {
            return (None, None);
        }
        let (runner, model) = match &inst.plugin.manifest.spec {
            Spec::Collector(c) => (c.runner.clone(), c.model.clone()),
            Spec::Judge(j) => (j.runner.clone(), j.model.clone()),
            _ => (None, None),
        };
        (
            inst.runner.clone().or(runner).or(self.user.runner.clone()),
            Some(
                inst.model
                    .clone()
                    .or(model)
                    .or(self.user.model.clone())
                    .unwrap_or_else(|| "default".into()),
            ),
        )
    }

    /// The runner instance a declarative plugin calls.
    // limit: the runner must be an enabled `[[plugins]]` instance of kind `runner`; the built-in
    // `claude` and `codex` runners come with #7.
    fn runner_of(&self, inst: &Instance) -> Result<&Instance, String> {
        let name = self.configured(inst).0.ok_or_else(|| {
            format!(
                "plugin instance `{}` has a prompt but no runner (set `runner`)",
                inst.id
            )
        })?;
        self.instances
            .iter()
            .find(|r| r.id == name && r.plugin.manifest.kind() == Kind::Runner)
            .ok_or_else(|| {
                format!(
                    "plugin instance `{}` uses the runner `{name}`, which is not an enabled runner instance",
                    inst.id
                )
            })
    }

    fn pipeline(&mut self, opts: &Opts) -> Result<Done> {
        // Step 1: load. Any error here leaves everything as it was (exit 4).
        let ab = |e: anyhow::Error| error::aborted(format!("{e:#}"));
        self.user = config::load_user_config().map_err(ab)?;
        self.instances = plugin::load(&self.ctx.store)?;
        let cfg = config::load_store_config(&self.ctx.store).map_err(ab)?;
        let max_rounds = cfg.max_rounds.unwrap_or(DEFAULT_ROUNDS);
        if max_rounds == 0 {
            return Err(error::aborted(format!(
                "{}: max_rounds must be at least 1",
                config::STORE_CONFIG
            )));
        }
        if let Some(c) = cfg.max_cost_usd
            && !(c.is_finite() && c >= 0.0)
        {
            return Err(error::aborted(format!(
                "{}: max_cost_usd {c} is not a cost",
                config::STORE_CONFIG
            )));
        }
        self.budget.cap = cfg.max_cost_usd;
        let old_lock = LockFile::load(&self.ctx.store).map_err(ab)?;
        for inst in &self.instances {
            if Self::prompt_of(inst).is_some() {
                self.runner_of(inst).map_err(error::aborted)?;
            }
            if matches!(
                inst.plugin.manifest.kind(),
                Kind::Collector | Kind::Check | Kind::Judge | Kind::View
            ) {
                let h = plugin::plugin_hash(&inst.plugin).map_err(ab)?;
                self.hashes.insert(inst.id.clone(), h);
            }
        }
        if let Some(only) = &opts.only
            && !self.of_kind(Kind::Collector).iter().any(|i| &i.id == only)
        {
            return Err(error::usage(format!(
                "--only `{only}`: no enabled collector instance has this id"
            )));
        }
        let mut work = Store::load(&self.ctx.store)?;
        let base = work.clone();

        // Step 2: fingerprint.
        let collectors: Vec<Instance> = self
            .of_kind(Kind::Collector)
            .into_iter()
            .filter(|i| opts.only.as_ref().is_none_or(|o| o == &i.id))
            .collect();
        let verdicts = par_map(&collectors, |inst| self.due(inst, &old_lock, opts.force));
        let mut due: Vec<(Instance, String)> = Vec::new();
        for (inst, d) in collectors.into_iter().zip(verdicts) {
            match d {
                Due::Yes(fp) => due.push((inst, fp)),
                Due::Skip(why) => self.skipped.push((inst.id, why)),
                Due::Unreachable(e) => {
                    self.unsettled.insert(inst.id.clone(), "unreachable".into());
                    self.unreachable.insert(inst.id, e);
                }
            }
        }
        if due.is_empty() {
            let code = if self.unreachable.is_empty() { 0 } else { 5 };
            return Ok(Done {
                code,
                nothing_due: true,
            });
        }
        let due_ids: Vec<&str> = due.iter().map(|(i, _)| i.id.as_str()).collect();
        self.hook(
            Event::RunBefore,
            json!({"collectors": due_ids, "force": opts.force}),
            "",
        )?;

        // Steps 3 to 8: rounds of collect, validate, merge, check and judge.
        let rules = Rules {
            collectors: self
                .of_kind(Kind::Collector)
                .iter()
                .map(|i| {
                    let scope = Self::collector_spec(i)
                        .map(|c| c.scope.clone())
                        .unwrap_or_default();
                    (
                        i.id.clone(),
                        Grant {
                            kinds: i.source_kinds.clone(),
                            scope,
                        },
                    )
                })
                .collect(),
        };
        let rank: BTreeMap<String, usize> = self
            .instances
            .iter()
            .enumerate()
            .map(|(n, i)| (i.id.clone(), n))
            .collect();
        let mut earlier: BTreeMap<String, String> = BTreeMap::new();
        let mut effects: Vec<Effect> = Vec::new();
        let mut sendback: BTreeMap<String, Vec<Rejection>> = BTreeMap::new();
        let mut callers: Vec<Instance> = due.iter().map(|(i, _)| i.clone()).collect();
        for round in 1..=max_rounds {
            if callers.is_empty() {
                break;
            }
            let outs = par_map(&callers, |inst| {
                let rej = sendback.get(&inst.id).cloned().unwrap_or_default();
                self.collect(inst, round, &work, rej)
            });
            let mut items: Vec<Item> = Vec::new();
            for (inst, out) in callers.iter().zip(outs) {
                let back = sendback.remove(&inst.id).unwrap_or_default();
                let props = match out {
                    Ok((props, model)) => {
                        self.collected
                            .entry(inst.id.clone())
                            .or_default()
                            .push(round);
                        if let Some(m) = model {
                            self.models.insert(inst.id.clone(), m);
                        }
                        props
                    }
                    Err(f) => {
                        let why = match f {
                            CallFail::Budget => "budget".to_string(),
                            CallFail::Error(e) => {
                                self.unreachable.insert(inst.id.clone(), e);
                                "unreachable".to_string()
                            }
                        };
                        self.unsettled.insert(inst.id.clone(), why.clone());
                        for b in back {
                            self.record(
                                &inst.id,
                                round,
                                b.proposal,
                                Vec::new(),
                                Outcome::Dropped(why.clone()),
                            );
                        }
                        continue;
                    }
                };
                self.hook(
                    Event::CollectAfter,
                    json!({"collector": inst.id, "round": round, "proposals": props}),
                    &entry(&inst.id, round),
                )?;
                let props = if round == 1 {
                    props
                } else {
                    let sent: BTreeSet<&str> =
                        back.iter().map(|b| b.proposal.key.as_str()).collect();
                    let got: BTreeSet<String> = props.iter().map(|p| p.key.clone()).collect();
                    for b in &back {
                        if !got.contains(&b.proposal.key) {
                            self.record(
                                &inst.id,
                                round,
                                b.proposal.clone(),
                                Vec::new(),
                                Outcome::Withdrawn,
                            );
                        }
                    }
                    let (kept, ignored): (Vec<Proposal>, Vec<Proposal>) = props
                        .into_iter()
                        .partition(|p| sent.contains(p.key.as_str()));
                    for p in ignored {
                        self.warnings.push(format!(
                            "round {round}: `{}/{}` was not sent back, so it was ignored",
                            inst.id, p.key
                        ));
                    }
                    kept
                };
                let r = rank.get(&inst.id).copied().unwrap_or(usize::MAX);
                items.extend(props.into_iter().map(|p| Item {
                    by: inst.id.clone(),
                    rank: r,
                    p,
                }));
            }
            if items.is_empty() {
                break;
            }
            let out = self.round(round, &work, &rules, &items, &earlier)?;
            work = out.store;
            earlier = out.assigned;
            effects.extend(out.effects);
            let last = round == max_rounds || self.budget.refused.load(Ordering::SeqCst);
            for it in &items {
                let key = it.key();
                let vs = out.verdicts.get(&key).cloned().unwrap_or_default();
                let outcome = if let Some(why) = out.dropped.get(&key) {
                    self.unsettled
                        .entry(it.by.clone())
                        .or_insert_with(|| why.clone());
                    Outcome::Dropped(why.clone())
                } else if vs.is_empty() {
                    Outcome::Accepted
                } else if last && self.budget.refused.load(Ordering::SeqCst) {
                    self.unsettled
                        .entry(it.by.clone())
                        .or_insert_with(|| "budget".into());
                    Outcome::Dropped("budget".into())
                } else {
                    if !last {
                        sendback.entry(it.by.clone()).or_default().push(Rejection {
                            proposal: it.p.clone(),
                            verdicts: vs.clone(),
                        });
                    }
                    Outcome::Rejected
                };
                self.record(&it.by, round, it.p.clone(), vs, outcome);
            }
            if last {
                break;
            }
            callers = due
                .iter()
                .map(|(i, _)| i.clone())
                .filter(|i| sendback.contains_key(&i.id))
                .collect();
        }

        // Step 9: the lock, the commit.
        let mut lock = old_lock.clone();
        let root = self
            .ctx
            .store
            .canonicalize()
            .unwrap_or_else(|_| self.ctx.store.clone());
        for (inst, fp) in &due {
            if self.unsettled.contains_key(&inst.id) {
                if let Some(e) = lock.collectors.get_mut(&inst.id) {
                    e.unsettled = true;
                }
                continue;
            }
            let interval = Self::collector_spec(inst)
                .and_then(|c| c.min_interval)
                .is_some();
            let (runner, model) = match self.models.get(&inst.id) {
                Some((r, m)) => (Some(r.clone()), Some(m.clone())),
                None => (None, None),
            };
            lock.collectors.insert(
                inst.id.clone(),
                CollectorLock {
                    fingerprint: fp.clone(),
                    plugin: plugin_label(&root, &inst.plugin),
                    hash: self.hashes.get(&inst.id).cloned().unwrap_or_default(),
                    runner,
                    model,
                    settled_at: interval.then(|| lock::fmt_utc(self.t0)),
                    unsettled: false,
                },
            );
        }
        let collector_ids: BTreeSet<String> = self
            .of_kind(Kind::Collector)
            .into_iter()
            .map(|i| i.id)
            .collect();
        lock.collectors.retain(|id, _| collector_ids.contains(id));
        lock.plugins = self
            .instances
            .iter()
            .filter(|i| {
                matches!(
                    i.plugin.manifest.kind(),
                    Kind::Check | Kind::Judge | Kind::View
                )
            })
            .map(|i| {
                (
                    i.id.clone(),
                    PluginLock {
                        plugin: plugin_label(&root, &i.plugin),
                        hash: self.hashes.get(&i.id).cloned().unwrap_or_default(),
                    },
                )
            })
            .collect();
        let mut changes = work.map_changes()?;
        let lock_text = lock.render();
        // Line endings do not count, as for the map: a CRLF checkout of an unchanged lock stays.
        let on_disk = std::fs::read_to_string(self.ctx.store.join(lock::LOCK_FILE))
            .ok()
            .map(|d| d.replace("\r\n", "\n"));
        if on_disk.as_deref() != Some(lock_text.as_str()) {
            changes.push((lock::LOCK_FILE.to_string(), Some(lock_text)));
        }
        let mut hash: Option<String> = None;
        if !changes.is_empty() {
            let diff = merge::net(&base, &work, &effects);
            let message = self.message(&diff);
            self.hook(
                Event::CommitBefore,
                json!({"diff": diff, "message": message}),
                "",
            )?;
            // `false`: git found nothing to commit (its own normalization), so no commit to report.
            if store::write_and_commit(&self.ctx.store, &changes, &message)? {
                let h = store::git(&self.ctx.store, &["rev-parse", "HEAD"])?
                    .trim()
                    .to_string();
                self.commit = Some((h.clone(), message.lines().next().unwrap_or("").to_string()));
                self.hook(
                    Event::CommitAfter,
                    json!({"commit": h, "message": message}),
                    "",
                )?;
                hash = Some(h);
            }
        }

        // Step 10: views. A failure is reported only.
        self.hook(Event::ViewRender, json!({"commit": hash}), "")?;
        for v in self.of_kind(Kind::View) {
            let Spec::View(spec) = &v.plugin.manifest.spec else {
                continue;
            };
            let input = PluginInput {
                project: self.ctx.project.clone(),
                run_id: self.ctx.run_id.clone(),
                params: v.params_json(),
            };
            if let Err(e) = process::call(&self.ctx, &v, "view", &spec.command, &input) {
                self.warnings.push(format!("view `{}` failed: {e:#}", v.id));
            }
        }
        let code = if self.unreachable.is_empty() && self.unsettled.is_empty() {
            0
        } else {
            5
        };
        Ok(Done {
            code,
            nothing_due: false,
        })
    }

    fn hook(&mut self, event: Event, data: Value, tag: &str) -> Result<()> {
        let w = process::run_hooks(&self.ctx, &self.instances, event, &data, tag)?;
        self.warnings.extend(w);
        Ok(())
    }

    fn record(
        &mut self,
        by: &str,
        round: u32,
        proposal: Proposal,
        verdicts: Vec<RoundVerdict>,
        outcome: Outcome,
    ) {
        self.log
            .entry(format!("{by}/{}", proposal.key))
            .or_default()
            .push(RoundRec {
                round,
                proposal,
                verdicts,
                outcome,
            });
    }

    /// Step 2 for one collector (spec §4.1, §4.2).
    fn due(&self, inst: &Instance, old: &LockFile, force: bool) -> Due {
        let Some(c) = Self::collector_spec(inst) else {
            return Due::Skip("not a collector".into());
        };
        let output = match &c.fingerprint {
            Some(cmd) => {
                let input = PluginInput {
                    project: self.ctx.project.clone(),
                    run_id: self.ctx.run_id.clone(),
                    params: inst.params_json(),
                };
                match process::call(&self.ctx, inst, "fingerprint", cmd, &input) {
                    Ok(out) => Some(sha256(out.as_bytes())),
                    Err(e) => return Due::Unreachable(format!("{e:#}")),
                }
            }
            None => None,
        };
        let (runner, model) = self.configured(inst);
        let combined = sha256(
            json!({
                "fingerprint": output,
                "plugin": self.hashes.get(&inst.id),
                "params": inst.params_json(),
                "runner": runner,
                "model": model,
            })
            .to_string()
            .as_bytes(),
        );
        if force {
            return Due::Yes(combined);
        }
        let Some(last) = old
            .collectors
            .get(&inst.id)
            .filter(|l| l.fingerprint == combined && !l.unsettled)
        else {
            return Due::Yes(combined);
        };
        let Some(interval) = c.min_interval else {
            return Due::Skip("fingerprint unchanged".into());
        };
        match last.settled_at.as_deref().and_then(lock::parse_utc) {
            Some(at) if at.saturating_add(interval.as_secs()) > self.t0 => Due::Skip(format!(
                "not due until {}",
                lock::fmt_utc(at.saturating_add(interval.as_secs()))
            )),
            _ => Due::Yes(combined),
        }
    }

    /// Step 3 for one collector: its proposals, and the runner and model when a runner ran it.
    fn collect(
        &self,
        inst: &Instance,
        round: u32,
        work: &Store,
        rejections: Vec<Rejection>,
    ) -> Result<Collected, CallFail> {
        let Some(c) = Self::collector_spec(inst) else {
            return Err(CallFail::Error("not a collector".into()));
        };
        let scope: Vec<BranchDef> = work
            .branches
            .iter()
            .filter(|b| c.scope.contains(&b.id))
            .cloned()
            .collect();
        let nodes = work
            .nodes
            .values()
            .filter(|n| n.parent.is_some() && c.scope.iter().any(|b| b == work.branch_of(&n.id)))
            .cloned()
            .collect();
        let input = CollectorInput {
            project: self.ctx.project.clone(),
            run_id: self.ctx.run_id.clone(),
            round,
            scope,
            nodes,
            rejections,
            params: inst.params_json(),
        };
        let name = entry("collect", round);
        match (&c.command, &c.prompt) {
            (Some(cmd), _) => {
                let out = process::call(&self.ctx, inst, &name, cmd, &input)?;
                Ok((wire::decode_proposals(&inst.id, &out)?, None))
            }
            (None, Some(prompt)) => {
                let schema = json!({"type": "object", "required": ["proposals"]});
                let input = serde_json::to_value(&input).map_err(anyhow::Error::from)?;
                let (out, runner, model) = self.via_runner(inst, prompt, &name, input, schema)?;
                Ok((
                    wire::decode_proposals(&inst.id, &out)?,
                    Some((runner, model)),
                ))
            }
            (None, None) => Err(CallFail::Error("neither `command` nor `prompt`".into())),
        }
    }

    /// A declarative plugin's call through its runner (spec §6.8): the cost cap first, then the
    /// runner process with the caller's timeout. Returns the output as JSON text, the runner
    /// instance and the model it reported.
    fn via_runner(
        &self,
        caller: &Instance,
        prompt: &str,
        name: &str,
        input: Value,
        schema: Value,
    ) -> Result<(String, String, String), CallFail> {
        let runner = self.runner_of(caller).map_err(CallFail::Error)?;
        let Spec::Runner(spec) = &runner.plugin.manifest.spec else {
            return Err(CallFail::Error(format!("`{}` is not a runner", runner.id)));
        };
        let path = caller.plugin.dir.join(prompt);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("plugin `{}`: reading {}", caller.id, path.display()))?;
        // limit: `allowed_runners` (spec §6.8) is not enforced yet; #7 refuses a collector's nodes
        // and resolved texts to any other runner.
        self.budget.reserve()?;
        let mut r = runner.clone();
        r.timeout = caller.timeout;
        let ri = RunnerInput {
            prompt: text,
            input,
            output_schema: schema,
            model: self
                .configured(caller)
                .1
                .unwrap_or_else(|| "default".into()),
            timeout: caller.timeout.as_secs(),
            read_only: true,
        };
        let out = process::call(
            &self.ctx,
            &r,
            &format!("{}.{name}", caller.id),
            &spec.command,
            &ri,
        )
        .with_context(|| format!("runner of plugin `{}`", caller.id))?;
        let ro = wire::decode_runner(&r.id, &out)?;
        self.budget.spend(&caller.id, ro.cost_usd);
        Ok((ro.output.to_string(), r.id.clone(), ro.model))
    }

    /// Steps 4 to 7 for one round's proposals.
    fn round(
        &mut self,
        round: u32,
        work: &Store,
        rules: &Rules,
        items: &[Item],
        earlier: &BTreeMap<String, String>,
    ) -> Result<RoundOut> {
        let core = |reason: String, with: Option<Proposal>| RoundVerdict {
            by: "core".into(),
            verdict: VerdictKind::Reject,
            reason,
            instruction: None,
            conflict_with: with,
        };
        let mut verdicts: BTreeMap<String, Vec<RoundVerdict>> = BTreeMap::new();
        let mut dropped: BTreeMap<String, String> = BTreeMap::new();
        let mut adds: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        for it in items.iter().filter(|it| it.p.op == wire::Op::Add) {
            adds.entry(&it.by).or_default().insert(it.p.key.clone());
        }
        // Step 4: the invariants, before any plugin sees a proposal.
        let mut valid: Vec<Item> = Vec::new();
        for it in items {
            let a = adds.get(it.by.as_str()).cloned().unwrap_or_default();
            match merge::validate(work, rules, it, &a, earlier) {
                Ok(()) => valid.push(it.clone()),
                Err(reason) => verdicts
                    .entry(it.key())
                    .or_default()
                    .push(core(reason, None)),
            }
        }
        // Step 5: conflicts, then the merge.
        let clash = merge::conflicts(&valid);
        for (key, others) in &clash {
            for o in others {
                verdicts
                    .entry(key.clone())
                    .or_default()
                    .push(core("conflict".into(), Some(o.clone())));
            }
        }
        valid.retain(|it| !clash.contains_key(&it.key()));
        let pre = merge::apply(work, rules, &valid, earlier);
        for (key, reason) in &pre.failed {
            verdicts
                .entry(key.clone())
                .or_default()
                .push(core(reason.clone(), None));
        }
        valid.retain(|it| !pre.failed.contains_key(&it.key()));
        // Step 6: the diff.
        let round_tag = round.to_string();
        self.hook(
            Event::DiffAfter,
            json!({"round": round, "diff": pre.effects}),
            if round > 1 { &round_tag } else { "" },
        )?;
        // Step 7: checks over the diff, then judges over what every check accepted.
        let map_ids: Vec<String> = work.nodes.keys().cloned().collect();
        for check in self.of_kind(Kind::Check) {
            let live: Vec<&Item> = valid
                .iter()
                .filter(|it| !dropped.contains_key(&it.key()))
                .collect();
            self.judge_with(
                &check,
                round,
                work,
                &live,
                None,
                &map_ids,
                &mut verdicts,
                &mut dropped,
            );
        }
        let judged: Vec<&Item> = valid
            .iter()
            .filter(|it| !dropped.contains_key(&it.key()) && !verdicts.contains_key(&it.key()))
            .collect();
        let judges = self.of_kind(Kind::Judge);
        if !judges.is_empty() && !judged.is_empty() {
            let texts = self.resolve(round, &judged);
            for judge in judges {
                let live: Vec<&Item> = judged
                    .iter()
                    .copied()
                    .filter(|it| !dropped.contains_key(&it.key()))
                    .collect();
                self.judge_with(
                    &judge,
                    round,
                    work,
                    &live,
                    Some(&texts),
                    &map_ids,
                    &mut verdicts,
                    &mut dropped,
                );
            }
        }
        let accepted: Vec<Item> = valid
            .iter()
            .filter(|it| !dropped.contains_key(&it.key()) && !verdicts.contains_key(&it.key()))
            .cloned()
            .collect();
        // A parent or a child that was not accepted takes its dependants with it.
        let post = merge::apply(work, rules, &accepted, earlier);
        for (key, reason) in &post.failed {
            verdicts
                .entry(key.clone())
                .or_default()
                .push(core(reason.clone(), None));
        }
        Ok(RoundOut {
            store: post.store,
            effects: post.effects,
            assigned: post.assigned,
            verdicts,
            dropped,
        })
    }

    /// One check or judge over `live`: its verdicts that are not `accept` go to `verdicts`; a
    /// failure drops everything it was given (spec §6.5).
    #[allow(clippy::too_many_arguments)]
    fn judge_with(
        &mut self,
        inst: &Instance,
        round: u32,
        work: &Store,
        live: &[&Item],
        texts: Option<&BTreeMap<String, Option<String>>>,
        map_ids: &[String],
        verdicts: &mut BTreeMap<String, Vec<RoundVerdict>>,
        dropped: &mut BTreeMap<String, String>,
    ) {
        if live.is_empty() {
            return;
        }
        let keys: Vec<String> = live.iter().map(|it| it.key()).collect();
        let input = JudgeInput {
            proposals: live
                .iter()
                .map(|it| JudgeItem {
                    key: it.key(),
                    proposal: it.p.clone(),
                    current: it.p.id.as_ref().and_then(|id| work.nodes.get(id)).cloned(),
                    sources: texts.map(|all| {
                        refs_of(&it.p)
                            .into_iter()
                            .map(|r| {
                                let t = all.get(&r).cloned().flatten();
                                (r, t)
                            })
                            .collect()
                    }),
                })
                .collect(),
            map_ids: map_ids.to_vec(),
            params: inst.params_json(),
        };
        let kind = if texts.is_some() { "judge" } else { "check" };
        match self.verdicts_of(inst, round, &input, &keys) {
            Ok(vs) => {
                for v in vs.into_iter().filter(|v| v.verdict != VerdictKind::Accept) {
                    verdicts
                        .entry(v.key.clone())
                        .or_default()
                        .push(RoundVerdict {
                            by: inst.id.clone(),
                            verdict: v.verdict,
                            reason: v.reason,
                            instruction: v.instruction,
                            conflict_with: None,
                        });
                }
            }
            Err(f) => {
                let why = match f {
                    CallFail::Budget => "budget".to_string(),
                    CallFail::Error(e) => format!("{kind} `{}` failed: {e}", inst.id),
                };
                if why != "budget" {
                    self.warnings.push(why.clone());
                }
                for k in keys {
                    dropped.entry(k).or_insert_with(|| why.clone());
                }
            }
        }
    }

    fn verdicts_of(
        &self,
        inst: &Instance,
        round: u32,
        input: &JudgeInput,
        keys: &[String],
    ) -> Result<Vec<Verdict>, CallFail> {
        let (name, cmd, prompt) = match &inst.plugin.manifest.spec {
            Spec::Check(c) => ("check", Some(&c.command), None),
            Spec::Judge(j) => ("judge", j.command.as_ref(), j.prompt.as_deref()),
            _ => return Err(CallFail::Error("not a check or judge".into())),
        };
        let name = entry(name, round);
        let out = match (cmd, prompt) {
            (Some(cmd), _) => process::call(&self.ctx, inst, &name, cmd, input)?,
            (None, Some(prompt)) => {
                let schema = json!({"type": "object", "required": ["verdicts"]});
                let input = serde_json::to_value(input).map_err(anyhow::Error::from)?;
                self.via_runner(inst, prompt, &name, input, schema)?.0
            }
            (None, None) => return Err(CallFail::Error("neither `command` nor `prompt`".into())),
        };
        Ok(wire::decode_verdicts(&inst.id, &out, keys)?)
    }

    /// The text of every reference the judged proposals cite, through each owner's `resolve`
    /// (spec §6.4); what does not resolve is `None`.
    fn resolve(&mut self, round: u32, items: &[&Item]) -> BTreeMap<String, Option<String>> {
        let mut by_owner: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut texts = BTreeMap::new();
        for it in items {
            for r in refs_of(&it.p) {
                texts.insert(r.clone(), None);
                let kind = r.split_once(':').map_or("", |(k, _)| k);
                if let Some(owner) = plugin::owner(&self.instances, kind) {
                    let refs = by_owner.entry(owner.id.clone()).or_default();
                    if !refs.contains(&r) {
                        refs.push(r);
                    }
                }
            }
        }
        for (owner, refs) in by_owner {
            let Some(inst) = self.instances.iter().find(|i| i.id == owner).cloned() else {
                continue;
            };
            // limit: a `resolve_prompt` runs through a runner from #7; until then its references stay unresolved.
            let Some(cmd) = Self::collector_spec(&inst).and_then(|c| c.resolve.clone()) else {
                continue;
            };
            let got = process::call(
                &self.ctx,
                &inst,
                &entry("resolve", round),
                &cmd,
                &ResolveInput { refs: refs.clone() },
            )
            .and_then(|out| wire::decode_texts(&inst.id, &out, &refs));
            match got {
                Ok(t) => texts.extend(t),
                Err(e) => self.warnings.push(format!(
                    "resolve of `{owner}` failed, its references stay unresolved: {e:#}"
                )),
            }
        }
        texts
    }

    /// The commit message of spec §4.3.
    fn message(&self, effects: &[Effect]) -> String {
        let t = lock::subject_time(self.t0);
        let count = |c: &[&str]| effects.iter().filter(|e| c.contains(&e.change)).count();
        let rejected = self.left_rejected();
        let subject = if effects.is_empty() {
            format!("run {t}: no map changes")
        } else {
            let rej = if rejected > 0 {
                format!(" (rejected {rejected})")
            } else {
                String::new()
            };
            format!(
                "run {t}: +{} ~{} -{}{rej}",
                count(&["added"]),
                count(&["updated", "moved"]),
                count(&["removed"])
            )
        };
        let rows: Vec<(String, String, String)> = effects
            .iter()
            .map(|e| {
                let extra = match (e.change, &e.from) {
                    ("updated", _) => format!(" ({})", e.fields.join(", ")),
                    ("moved", Some(f)) => format!(" (from {f})"),
                    _ => String::new(),
                };
                let reason = one_line(&e.reason, 120);
                let what = if e.change == "removed" {
                    format!("evidence: {reason}")
                } else {
                    reason
                };
                (
                    format!("{:<7} {}/{}{extra}", e.change, e.branch, e.id),
                    format!("by {}", e.by),
                    format!("{what} [{}]", e.refs.join(", ")),
                )
            })
            .collect();
        let w1 = rows.iter().map(|r| r.0.chars().count()).max().unwrap_or(0);
        let w2 = rows.iter().map(|r| r.1.chars().count()).max().unwrap_or(0);
        let mut body: Vec<String> = rows
            .iter()
            .map(|(a, b, c)| format!("{a:<w1$}  {b:<w2$}  {c}").trim_end().to_string())
            .collect();
        if rejected > 0 {
            body.push(format!(
                "rejected {rejected}, see runs/{}/report.md",
                self.ctx.run_id
            ));
        }
        if !self.unreachable.is_empty() {
            let list: Vec<String> = self
                .unreachable
                .iter()
                .map(|(id, e)| format!("{id} ({})", one_line(e.lines().next().unwrap_or(""), 100)))
                .collect();
            body.push(format!("unreachable: {}", list.join(", ")));
        }
        let other: Vec<String> = self
            .unsettled
            .iter()
            .filter(|(id, _)| !self.unreachable.contains_key(*id))
            .map(|(id, why)| format!("{id} ({})", one_line(why, 100)))
            .collect();
        if !other.is_empty() {
            body.push(format!("unsettled: {}", other.join(", ")));
        }
        if body.is_empty() {
            format!("{subject}\n")
        } else {
            format!("{subject}\n\n{}\n", body.join("\n"))
        }
    }

    /// Proposals whose last round left them rejected (withdrawn ones included).
    fn left_rejected(&self) -> usize {
        self.log
            .values()
            .filter(|recs| {
                recs.last()
                    .is_some_and(|r| matches!(r.outcome, Outcome::Rejected | Outcome::Withdrawn))
            })
            .count()
    }

    /// The run report of spec §4.4.
    fn report(&self, result: &Result<Done>, wall: f64) -> String {
        let mut s = format!("# Run {}\n\n## Summary\n", self.ctx.run_id);
        let result_line = match result {
            Ok(Done {
                nothing_due: true,
                code: 0,
            }) => "exit 0, nothing to collect: no collector is due".to_string(),
            Ok(Done { code: 0, .. }) => {
                "exit 0, every collector ran and every proposal was settled".to_string()
            }
            Ok(Done { code, .. }) => {
                format!("exit {code}, committed what was accepted, but not everything ran")
            }
            Err(e) => {
                let code = error::exit_code(e);
                let what = if code == 4 {
                    "aborted, the store is unchanged"
                } else {
                    "failed"
                };
                format!("exit {code}, {what}: {}", one_line(&format!("{e:#}"), 2000))
            }
        };
        s += &format!("- result: {result_line}\n");
        s += &match &self.commit {
            Some((h, subject)) => format!("- commit: {} {subject}\n", &h[..h.len().min(12)]),
            None => "- commit: none\n".into(),
        };
        if !self.collected.is_empty() {
            let list: Vec<String> = self
                .collected
                .iter()
                .map(|(id, rounds)| {
                    let r: Vec<String> = rounds.iter().map(u32::to_string).collect();
                    format!("{id} (round {})", r.join(", "))
                })
                .collect();
            s += &format!("- collected: {}\n", list.join(", "));
        }
        if !self.skipped.is_empty() {
            let list: Vec<String> = self
                .skipped
                .iter()
                .map(|(id, why)| format!("{id} ({why})"))
                .collect();
            s += &format!("- skipped: {}\n", list.join(", "));
        }
        for (id, e) in &self.unreachable {
            s += &format!("- unreachable: {id}: {}\n", one_line(e, 2000));
        }
        for (id, why) in &self.unsettled {
            s += &format!("- unsettled: {id} ({})\n", one_line(why, 2000));
        }
        s += &format!("- rejected: {}\n", self.left_rejected());
        let (total, per) = self
            .budget
            .spent
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|p| p.into_inner().clone());
        if total > 0.0 || !per.is_empty() {
            let list: Vec<String> = per.iter().map(|(id, c)| format!("{id} ${c:.2}")).collect();
            s += &format!("- cost: ${total:.2} ({})\n", list.join(", "));
        }
        s += &format!("- wall time: {wall:.1}s\n");
        if !self.log.is_empty() {
            s += "\n## Proposals\n";
            for (key, recs) in &self.log {
                let first = &recs[0].proposal;
                s += &format!("\n### {key}: {}\n", describe(first));
                for r in recs {
                    let outcome = match &r.outcome {
                        Outcome::Accepted => "accepted".to_string(),
                        Outcome::Rejected => "rejected".to_string(),
                        Outcome::Dropped(why) => format!("dropped: {}", one_line(why, 2000)),
                        Outcome::Withdrawn => "withdrawn by its collector".to_string(),
                    };
                    s += &format!("- round {}: {outcome}", r.round);
                    if r.proposal != *first {
                        s += &format!(" ({})", describe(&r.proposal));
                    }
                    s += "\n";
                    for v in &r.verdicts {
                        let verdict = match v.verdict {
                            VerdictKind::Accept => "accept",
                            VerdictKind::Reject => "reject",
                            VerdictKind::Revise => "revise",
                        };
                        s += &format!("  - {} {verdict}: {}", v.by, one_line(&v.reason, 2000));
                        if let Some(i) = &v.instruction {
                            s += &format!("; instruction: {}", one_line(i, 2000));
                        }
                        if let Some(o) = &v.conflict_with {
                            s += &format!("; conflicts with `{}`", o.key);
                        }
                        s += "\n";
                    }
                }
            }
        }
        if !self.warnings.is_empty() {
            s += "\n## Warnings\n\n";
            for w in &self.warnings {
                s += &format!("- {}\n", one_line(w, 2000));
            }
        }
        s
    }
}

struct RoundOut {
    store: Store,
    effects: Vec<Effect>,
    assigned: BTreeMap<String, String>,
    verdicts: BTreeMap<String, Vec<RoundVerdict>>,
    dropped: BTreeMap<String, String>,
}

/// The references a proposal cites: its sources and its evidence.
fn refs_of(p: &Proposal) -> Vec<String> {
    let mut refs = p.sources.clone();
    if let Some(e) = &p.evidence
        && !refs.contains(e)
    {
        refs.push(e.clone());
    }
    refs
}

/// A proposal in a few words for the report.
fn describe(p: &Proposal) -> String {
    let mut s = wire::op_name(p.op).to_string();
    if let Some(id) = &p.id {
        s += &format!(" `{id}`");
    }
    match (&p.parent, &p.parent_key) {
        (Some(par), _) => s += &format!(" under `{par}`"),
        (None, Some(k)) => s += &format!(" under the proposal `{k}`"),
        _ => {}
    }
    if let Some(t) = &p.title {
        s += &format!(" \"{}\"", one_line(t, 80));
    }
    s
}

/// A plugin's source as the lock records it: `builtin` for one inside the binary, else its
/// directory, relative to the store when inside it.
fn plugin_label(store: &Path, plugin: &plugin::Plugin) -> String {
    if plugin.builtin {
        return "builtin".into();
    }
    let dir: PathBuf = plugin
        .dir
        .canonicalize()
        .unwrap_or_else(|_| plugin.dir.clone());
    match dir.strip_prefix(store) {
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => dir.to_string_lossy().into_owned(),
    }
}

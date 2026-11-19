//! The core's half of a run round (spec §4.1 steps 4 to 6, §5): the invariants on each
//! proposal, conflicts, applying proposals to a copy of the map, and the node diff.
//! No plugin runs here; `run` calls these between collecting and judging.

use crate::node::{self, Node};
use crate::plugin::CORE_KINDS;
use crate::store::Store;
use crate::wire::{Op, Proposal, op_name};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// What one collector instance may touch: the source kinds it owns and the branches it covers.
#[derive(Debug, Clone, Default)]
pub struct Grant {
    pub kinds: Vec<String>,
    pub scope: Vec<String>,
}

/// The grants of every enabled collector, by instance id.
#[derive(Debug, Clone, Default)]
pub struct Rules {
    pub collectors: BTreeMap<String, Grant>,
}

impl Rules {
    fn grant(&self, by: &str) -> Grant {
        self.collectors.get(by).cloned().unwrap_or_default()
    }

    fn owned(&self, kind: &str) -> bool {
        self.collectors
            .values()
            .any(|g| g.kinds.iter().any(|k| k == kind))
    }
}

/// One proposal of one collector in a round. `rank` is the collector's place in the config.
#[derive(Debug, Clone)]
pub struct Item {
    pub by: String,
    pub rank: usize,
    pub p: Proposal,
}

impl Item {
    /// `<collector instance>/<proposal key>`, unique across collectors (spec §6.5).
    pub fn key(&self) -> String {
        format!("{}/{}", self.by, self.p.key)
    }
}

/// One change to the map, as the diff, the commit message and the hooks show it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Effect {
    /// `added`, `updated`, `moved` or `removed`.
    pub change: &'static str,
    pub id: String,
    /// The node's parent after the change (before it, for a removal).
    pub parent: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<&'static str>,
    pub by: String,
    pub key: String,
    /// The branch the node sits under, for `<branch>/<id>`.
    #[serde(skip)]
    pub branch: String,
    /// A move's previous parent.
    #[serde(skip)]
    pub from: Option<String>,
    #[serde(skip)]
    pub reason: String,
    /// The proposal's sources, or a removal's evidence.
    #[serde(skip)]
    pub refs: Vec<String>,
}

/// The fields each op may carry besides `key`, `op` and `reason`.
fn allowed(op: Op) -> &'static [&'static str] {
    match op {
        Op::Add => &[
            "parent",
            "parent_key",
            "title",
            "body",
            "date",
            "state",
            "sources",
        ],
        Op::Update => &["id", "title", "body", "date", "state", "sources"],
        Op::Move => &["id", "parent", "parent_key", "sources"],
        Op::Remove => &["id", "evidence", "sources"],
        Op::AddSource => &["id", "sources"],
    }
}

fn kind_of(r: &str) -> &str {
    r.split_once(':').map_or(r, |(k, _)| k)
}

/// A source reference a proposal may cite: well-formed, of a kind an enabled collector owns.
fn check_ref(rules: &Rules, r: &str) -> Result<(), String> {
    node::check_source(r)?;
    let k = kind_of(r);
    if CORE_KINDS.contains(&k) {
        return Err(format!(
            "source `{r}`: the kind `{k}` belongs to rrecall itself"
        ));
    }
    if !rules.owned(k) {
        return Err(format!(
            "source `{r}`: no enabled collector owns the kind `{k}`"
        ));
    }
    Ok(())
}

/// Whether `id` is `of` or sits under it.
fn within(s: &Store, id: &str, of: &str) -> bool {
    let mut cur = Some(id);
    for _ in 0..=s.nodes.len() {
        match cur {
            Some(c) if c == of => return true,
            Some(c) => cur = s.nodes.get(c).and_then(|n| n.parent.as_deref()),
            None => return false,
        }
    }
    false
}

/// Whether `id` sits under a branch in the scope of the collector `by` (spec §6.4).
fn in_scope(s: &Store, by: &str, g: &Grant, id: &str) -> Result<(), String> {
    let b = s.branch_of(id);
    if g.scope.iter().any(|x| x == b) {
        Ok(())
    } else {
        Err(format!(
            "`{id}` is under the branch `{b}`, outside the scope of `{by}` ({})",
            g.scope.join(", ")
        ))
    }
}

/// The invariants of spec §5 and the rules of the proposal format, on one proposal against the
/// map as it stands before this round. `adds` holds the keys of this collector's `add`
/// proposals in this round; `earlier` maps the full keys of adds accepted in earlier rounds to
/// the ids they got. The error is the reason the collector sees.
pub fn validate(
    work: &Store,
    rules: &Rules,
    item: &Item,
    adds: &BTreeSet<String>,
    earlier: &BTreeMap<String, String>,
) -> Result<(), String> {
    let p = &item.p;
    let op = op_name(p.op);
    let g = rules.grant(&item.by);
    let own = |r: &str| g.kinds.iter().any(|k| k == kind_of(r));
    if p.op == Op::Add && p.id.is_some() {
        return Err("I4: new nodes get their ids from rrecall; an `add` may not set `id`".into());
    }
    let present = [
        ("id", p.id.is_some()),
        ("parent", p.parent.is_some()),
        ("parent_key", p.parent_key.is_some()),
        ("title", p.title.is_some()),
        ("body", p.body.is_some()),
        ("date", p.date.is_some()),
        ("state", p.state.is_some()),
        ("evidence", p.evidence.is_some()),
    ];
    if let Some((f, _)) = present
        .iter()
        .find(|(f, set)| *set && !allowed(p.op).contains(f))
    {
        let hint = if p.op == Op::Update && (*f == "parent" || *f == "parent_key") {
            "; a new parent is a `move`"
        } else {
            ""
        };
        return Err(format!("`{f}` does not apply to `{op}`{hint}"));
    }
    for r in &p.sources {
        check_ref(rules, r)?;
    }
    if matches!(p.op, Op::Update | Op::AddSource) {
        if p.sources.is_empty() {
            return Err(format!(
                "`{op}` needs at least one source of its own kinds ({})",
                g.kinds.join(", ")
            ));
        }
        if let Some(r) = p.sources.iter().find(|r| !own(r)) {
            return Err(format!(
                "`{op}` may cite only its own kinds ({}); `{r}` is not one",
                g.kinds.join(", ")
            ));
        }
    }
    if p.op == Op::Add && p.sources.is_empty() {
        return Err("`add` needs at least one source (every node names its source)".into());
    }
    let in_scope = |id: &str| in_scope(work, &item.by, &g, id);
    if let Some(id) = &p.id {
        let n = work
            .nodes
            .get(id)
            .ok_or_else(|| format!("no node `{id}` in the map"))?;
        if n.parent.is_none() {
            return Err(format!(
                "I5: `{id}` is a branch; only the config changes branches"
            ));
        }
        in_scope(id)?;
        if matches!(p.op, Op::Update | Op::Move | Op::Remove) && !n.sources.iter().any(|r| own(r)) {
            return Err(format!(
                "I8: `{id}` cites none of the kinds of `{}` ({}); it may only propose `add_source` on it",
                item.by,
                g.kinds.join(", ")
            ));
        }
        if p.op == Op::Remove {
            let Some(ev) = &p.evidence else {
                return Err(
                    "I2: a removal needs `evidence`, a source showing the fact is no longer true"
                        .into(),
                );
            };
            check_ref(rules, ev).map_err(|e| format!("evidence: {e}"))?;
            if n.note.is_some() {
                return Err(format!(
                    "I3: `{id}` has the user's note; only `rrecall rm` removes it"
                ));
            }
        }
    }
    if let Some(parent) = &p.parent {
        if !work.nodes.contains_key(parent) {
            return Err(format!("no node `{parent}` in the map"));
        }
        in_scope(parent)?;
        if let Some(id) = &p.id
            && within(work, parent, id)
        {
            return Err(format!("`{id}` cannot move under itself (`{parent}`)"));
        }
    }
    if let Some(k) = &p.parent_key {
        let full = format!("{}/{k}", item.by);
        if *k == p.key {
            return Err(format!("`parent_key` `{k}` names the proposal itself"));
        }
        if !adds.contains(k) && !earlier.contains_key(&full) {
            return Err(format!(
                "`parent_key` `{k}` names no `add` of `{}` in this or an earlier round",
                item.by
            ));
        }
    }
    Ok(())
}

/// Proposals that change the same node (I9): for each full key, the other proposals on its node.
pub fn conflicts(items: &[Item]) -> BTreeMap<String, Vec<Proposal>> {
    let mut by_node: BTreeMap<&str, Vec<&Item>> = BTreeMap::new();
    for it in items {
        if let Some(id) = &it.p.id {
            by_node.entry(id).or_default().push(it);
        }
    }
    let mut out = BTreeMap::new();
    for group in by_node.values().filter(|g| g.len() > 1) {
        for it in group {
            let others = group
                .iter()
                .filter(|o| o.key() != it.key())
                .map(|o| o.p.clone())
                .collect();
            out.insert(it.key(), others);
        }
    }
    out
}

/// The result of applying proposals to a copy of the map.
#[derive(Debug)]
pub struct Applied {
    pub store: Store,
    pub effects: Vec<Effect>,
    /// Full key to the reason the proposal could not be applied.
    pub failed: BTreeMap<String, String>,
    /// `earlier` plus the ids this application gave to new nodes, by full key.
    pub assigned: BTreeMap<String, String>,
}

/// Applies validated, conflict-free proposals to a copy of `work` in collector order, then by
/// proposal key (spec §4.1 step 5). An add waits for the add its `parent_key` names; removals
/// go last, as a set, so a node and all its children can go together (I3). A proposal whose
/// parent did not apply fails with "parent rejected".
///
/// Placement is checked on the merged tree: a node added or moved in this round must end under
/// a branch in its collector's scope, whichever proposal (a move of an ancestor by another
/// collector) put it elsewhere. Such a proposal fails and the round is applied again without it.
pub fn apply(
    work: &Store,
    rules: &Rules,
    items: &[Item],
    earlier: &BTreeMap<String, String>,
) -> Applied {
    let mut out_of_scope: BTreeMap<String, String> = BTreeMap::new();
    loop {
        let left: Vec<Item> = items
            .iter()
            .filter(|it| !out_of_scope.contains_key(&it.key()))
            .cloned()
            .collect();
        let mut a = apply_round(work, rules, &left, earlier);
        let bad: Vec<(String, String)> = a
            .effects
            .iter()
            .filter(|e| matches!(e.change, "added" | "moved"))
            .filter_map(|e| {
                let err = in_scope(&a.store, &e.by, &rules.grant(&e.by), &e.id).err()?;
                Some((format!("{}/{}", e.by, e.key), err))
            })
            .collect();
        if bad.is_empty() {
            a.failed.extend(out_of_scope);
            return a;
        }
        out_of_scope.extend(bad);
    }
}

fn apply_round(
    work: &Store,
    rules: &Rules,
    items: &[Item],
    earlier: &BTreeMap<String, String>,
) -> Applied {
    let mut s = work.clone();
    let mut assigned = earlier.clone();
    let mut failed: BTreeMap<String, String> = BTreeMap::new();
    let mut effects = Vec::new();
    let mut sorted: Vec<&Item> = items.iter().collect();
    sorted.sort_by(|a, b| (a.rank, &a.p.key).cmp(&(b.rank, &b.p.key)));
    let (removes, mut pending): (Vec<&Item>, Vec<&Item>) =
        sorted.into_iter().partition(|it| it.p.op == Op::Remove);
    while !pending.is_empty() {
        let waiting: BTreeSet<String> = pending.iter().map(|it| it.key()).collect();
        let mut deferred = Vec::new();
        let mut progress = false;
        for it in pending {
            let parent = match (&it.p.parent, &it.p.parent_key) {
                (Some(id), _) => Some(id.clone()),
                (None, Some(k)) => {
                    let full = format!("{}/{k}", it.by);
                    match assigned.get(&full) {
                        Some(id) => Some(id.clone()),
                        None if waiting.contains(&full) && !failed.contains_key(&full) => {
                            deferred.push(it);
                            continue;
                        }
                        None => {
                            failed.insert(it.key(), "parent rejected".into());
                            progress = true;
                            continue;
                        }
                    }
                }
                (None, None) => None,
            };
            progress = true;
            match apply_one(&mut s, rules, it, parent) {
                Ok(eff) => {
                    if it.p.op == Op::Add
                        && let Some(e) = &eff
                    {
                        assigned.insert(it.key(), e.id.clone());
                    }
                    effects.extend(eff);
                }
                Err(e) => {
                    failed.insert(it.key(), e);
                }
            }
        }
        if !progress {
            for it in &deferred {
                failed.insert(
                    it.key(),
                    "parent rejected: the `parent_key` chain never reaches an applied node".into(),
                );
            }
            break;
        }
        pending = deferred;
    }
    remove_set(&mut s, &removes, &mut failed, &mut effects);
    Applied {
        store: s,
        effects,
        failed,
        assigned,
    }
}

/// The removals as one set: a node goes only when each of its children goes too (I3).
fn remove_set(
    s: &mut Store,
    removes: &[&Item],
    failed: &mut BTreeMap<String, String>,
    effects: &mut Vec<Effect>,
) {
    let mut set: Vec<&Item> = Vec::new();
    for it in removes {
        let id = it.p.id.as_deref().unwrap_or_default();
        if s.nodes.contains_key(id) {
            set.push(it);
        } else {
            failed.insert(it.key(), format!("no node `{id}` in the map"));
        }
    }
    loop {
        let going: BTreeSet<&str> = set.iter().filter_map(|it| it.p.id.as_deref()).collect();
        let kids = s.child_map();
        let (keep, drop): (Vec<&Item>, Vec<&Item>) = set.iter().partition(|it| {
            let id = it.p.id.as_deref().unwrap_or_default();
            kids.get(id)
                .is_none_or(|ks| ks.iter().all(|k| going.contains(k.id.as_str())))
        });
        if drop.is_empty() {
            break;
        }
        for it in drop {
            failed.insert(
                it.key(),
                format!(
                    "I3: `{}` has children that are not removed or moved in this run; remove it by hand with `rrecall rm --recursive`",
                    it.p.id.as_deref().unwrap_or_default()
                ),
            );
        }
        set = keep;
    }
    // Branches first: once a parent goes, its child's branch can no longer be looked up.
    let branches: Vec<String> = set
        .iter()
        .map(|it| {
            s.branch_of(it.p.id.as_deref().unwrap_or_default())
                .to_string()
        })
        .collect();
    for (it, branch) in set.into_iter().zip(branches) {
        let id = it.p.id.clone().unwrap_or_default();
        if let Some(n) = s.nodes.remove(&id) {
            effects.push(Effect {
                change: "removed",
                id,
                parent: n.parent.unwrap_or_default(),
                fields: Vec::new(),
                by: it.by.clone(),
                key: it.p.key.clone(),
                branch,
                from: None,
                reason: it.p.reason.clone(),
                refs: it.p.evidence.iter().cloned().collect(),
            });
        }
    }
}

/// The run's net diff (spec §4.3): each node that differs between `before` and `after`, in the
/// order of its last change in `effects`. A node changed and changed back shows nothing. A line
/// names the last proposal of its kind on the node (the add for `added`), else the last one.
pub fn net(before: &Store, after: &Store, effects: &[Effect]) -> Vec<Effect> {
    let mut seen = BTreeSet::new();
    let mut last: Vec<&Effect> = effects
        .iter()
        .rev()
        .filter(|e| seen.insert(e.id.as_str()))
        .collect();
    last.reverse();
    let mut out = Vec::new();
    for e in last {
        let line = |change: &'static str, store: &Store, parent: &Option<String>| {
            let src = effects
                .iter()
                .rev()
                .find(|x| x.id == e.id && x.change == change)
                .unwrap_or(e);
            Effect {
                change,
                id: e.id.clone(),
                parent: parent.clone().unwrap_or_default(),
                fields: Vec::new(),
                by: src.by.clone(),
                key: src.key.clone(),
                branch: store.branch_of(&e.id).to_string(),
                from: None,
                reason: src.reason.clone(),
                refs: src.refs.clone(),
            }
        };
        match (before.nodes.get(&e.id), after.nodes.get(&e.id)) {
            (None, None) => {}
            (Some(b), None) => out.push(line("removed", before, &b.parent)),
            (None, Some(a)) => out.push(line("added", after, &a.parent)),
            (Some(b), Some(a)) => {
                if b.parent != a.parent {
                    let mut m = line("moved", after, &a.parent);
                    m.from = b.parent.clone();
                    out.push(m);
                }
                let fields: Vec<&'static str> = [
                    ("title", a.title != b.title),
                    ("body", a.body != b.body),
                    ("sources", a.sources != b.sources),
                    ("date", a.date != b.date),
                    ("state", a.state != b.state),
                ]
                .into_iter()
                .filter_map(|(f, changed)| changed.then_some(f))
                .collect();
                if !fields.is_empty() {
                    let mut u = line("updated", after, &a.parent);
                    u.fields = fields;
                    out.push(u);
                }
            }
        }
    }
    out
}

/// The next sibling order under `parent`: new and moved nodes go last.
fn next_order(s: &Store, parent: &str) -> u32 {
    s.nodes
        .values()
        .filter(|n| n.parent.as_deref() == Some(parent))
        .map(|n| n.order.saturating_add(1))
        .max()
        .unwrap_or(0)
}

/// Applies one non-removal; `None` when the node did not change.
fn apply_one(
    s: &mut Store,
    rules: &Rules,
    it: &Item,
    parent: Option<String>,
) -> Result<Option<Effect>, String> {
    let p = &it.p;
    let effect = |change, id: &str, parent: &str, fields, s: &Store| Effect {
        change,
        id: id.to_string(),
        parent: parent.to_string(),
        fields,
        by: it.by.clone(),
        key: p.key.clone(),
        branch: s.branch_of(id).to_string(),
        from: None,
        reason: p.reason.clone(),
        refs: p.sources.clone(),
    };
    if let Some(par) = &parent
        && !s.nodes.contains_key(par)
    {
        return Err(format!("no node `{par}` in the map"));
    }
    if p.op == Op::Add {
        let parent = parent.unwrap_or_default();
        let title = p.title.clone().unwrap_or_default();
        let n = Node {
            id: node::unique_slug(&title, |x| s.id_taken(x)),
            parent: Some(parent.clone()),
            title,
            body: p.body.clone().unwrap_or_default(),
            sources: p.sources.clone(),
            date: p.date.clone(),
            state: p.state.clone(),
            order: next_order(s, &parent),
            note: None,
        };
        n.validate()?;
        let id = n.id.clone();
        s.nodes.insert(id.clone(), n);
        return Ok(Some(effect("added", &id, &parent, Vec::new(), s)));
    }
    let id = p.id.clone().unwrap_or_default();
    let cur = s
        .nodes
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("no node `{id}` in the map"))?;
    let mut n = cur.clone();
    match p.op {
        Op::Move => {
            let parent = parent.unwrap_or_default();
            if within(s, &parent, &id) {
                return Err(format!("`{id}` cannot move under itself (`{parent}`)"));
            }
            if cur.parent.as_deref() == Some(parent.as_str()) {
                return Ok(None);
            }
            n.order = next_order(s, &parent);
            n.parent = Some(parent.clone());
            s.nodes.insert(id.clone(), n);
            let mut e = effect("moved", &id, &parent, Vec::new(), s);
            e.from = cur.parent;
            return Ok(Some(e));
        }
        Op::Update => {
            let g = rules.grant(&it.by);
            let own = |r: &str| g.kinds.iter().any(|k| k == kind_of(r));
            // The collector's own references are replaced; the others stay where they are.
            let mut sources: Vec<String> = cur
                .sources
                .iter()
                .filter(|r| !own(r) || p.sources.contains(r))
                .cloned()
                .collect();
            for r in &p.sources {
                if !sources.contains(r) {
                    sources.push(r.clone());
                }
            }
            n.sources = sources;
            if let Some(t) = &p.title {
                n.title = t.clone();
            }
            if let Some(b) = &p.body {
                n.body = b.clone();
            }
            if p.date.is_some() {
                n.date = p.date.clone();
            }
            if p.state.is_some() {
                n.state = p.state.clone();
            }
        }
        Op::AddSource => {
            for r in &p.sources {
                if !n.sources.contains(r) {
                    n.sources.push(r.clone());
                }
            }
        }
        Op::Add | Op::Remove => {}
    }
    n.validate()?;
    let fields: Vec<&'static str> = [
        ("title", n.title != cur.title),
        ("body", n.body != cur.body),
        ("sources", n.sources != cur.sources),
        ("date", n.date != cur.date),
        ("state", n.state != cur.state),
    ]
    .into_iter()
    .filter_map(|(f, changed)| changed.then_some(f))
    .collect();
    if fields.is_empty() {
        return Ok(None);
    }
    let parent = n.parent.clone().unwrap_or_default();
    s.nodes.insert(id.clone(), n);
    Ok(Some(effect("updated", &id, &parent, fields, s)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn store(lines: &[&str]) -> (TempDir, Store) {
        let t = TempDir::new();
        std::fs::write(t.0.join(crate::config::STORE_CONFIG), "").unwrap();
        std::fs::create_dir_all(t.0.join("map")).unwrap();
        let mut by_branch: BTreeMap<String, String> = BTreeMap::new();
        for l in lines {
            let n: Node = serde_json::from_str(l).unwrap();
            // Test lines name their branch as parent or sit one level below `api`, in layout.
            let b = if n.parent.as_deref() == Some("api") {
                "layout".to_string()
            } else {
                n.parent.clone().unwrap()
            };
            let e = by_branch.entry(b).or_default();
            e.push_str(l);
            e.push('\n');
        }
        for (b, text) in by_branch {
            std::fs::write(t.0.join(format!("map/{b}.jsonl")), text).unwrap();
        }
        let s = Store::load(&t.0).unwrap();
        (t, s)
    }

    const API: &str = r#"{"id":"api","parent":"layout","title":"API","body":"HTTP API.","sources":["file:api.md","gh-pr:o/r#1"],"order":0}"#;
    const AUTH: &str = r#"{"id":"auth","parent":"api","title":"Auth","body":"Tokens.","sources":["file:auth.md"],"order":0}"#;
    const NOTED: &str = r#"{"id":"noted","parent":"layout","title":"Noted","body":"","sources":["file:n.md"],"order":1,"note":"keep"}"#;
    const THEIRS: &str = r#"{"id":"theirs","parent":"layout","title":"Theirs","body":"","sources":["gh-pr:o/r#2"],"order":2}"#;

    fn rules() -> Rules {
        let mut r = Rules::default();
        r.collectors.insert(
            "repo".into(),
            Grant {
                kinds: vec!["file".into()],
                scope: vec!["layout".into(), "how-it-works".into()],
            },
        );
        r.collectors.insert(
            "github".into(),
            Grant {
                kinds: vec!["gh-pr".into()],
                scope: vec!["layout".into(), "done".into()],
            },
        );
        r
    }

    fn item(by: &str, json: &str) -> Item {
        let mut v: serde_json::Value = serde_json::from_str(json).unwrap();
        let o = v.as_object_mut().unwrap();
        o.entry("reason").or_insert("why".into());
        o.entry("sources").or_insert(serde_json::json!([]));
        Item {
            by: by.into(),
            rank: if by == "repo" { 0 } else { 1 },
            p: serde_json::from_value(v).unwrap(),
        }
    }

    fn check(s: &Store, it: &Item) -> Result<(), String> {
        let adds = BTreeSet::from(["new".to_string()]);
        validate(s, &rules(), it, &adds, &BTreeMap::new())
    }

    #[test]
    fn invariants_reject_with_a_reason() {
        let (_t, s) = store(&[API, AUTH, NOTED, THEIRS]);
        for (by, json, want) in [
            // I4: new nodes get ids from the core.
            (
                "repo",
                r#"{"key":"a","op":"add","id":"mine","parent":"layout","title":"T","body":"","sources":["file:x"]}"#,
                "I4",
            ),
            // I2: no removal without evidence.
            ("repo", r#"{"key":"a","op":"remove","id":"auth"}"#, "I2"),
            // I3: a node with the user's note stays.
            (
                "repo",
                r#"{"key":"a","op":"remove","id":"noted","evidence":"file:gone"}"#,
                "I3",
            ),
            // I8: only add_source on another collector's node.
            (
                "repo",
                r#"{"key":"a","op":"update","id":"theirs","body":"b","sources":["file:x"]}"#,
                "I8",
            ),
            (
                "repo",
                r#"{"key":"a","op":"move","id":"theirs","parent":"api","sources":["file:x"]}"#,
                "I8",
            ),
            (
                "repo",
                r#"{"key":"a","op":"remove","id":"theirs","evidence":"file:gone"}"#,
                "I8",
            ),
            // An update cites only its own kinds.
            (
                "repo",
                r#"{"key":"a","op":"update","id":"auth","body":"b","sources":["gh-pr:o/r#3"]}"#,
                "own kinds",
            ),
            (
                "repo",
                r#"{"key":"a","op":"update","id":"auth","body":"b"}"#,
                "own kinds",
            ),
            (
                "github",
                r#"{"key":"a","op":"add_source","id":"auth","sources":["file:x"]}"#,
                "own kinds",
            ),
            // I5: branches come from the config.
            (
                "repo",
                r#"{"key":"a","op":"update","id":"layout","body":"b","sources":["file:x"]}"#,
                "I5",
            ),
            // Every kind has an owner, and the core's kinds are the core's.
            (
                "repo",
                r#"{"key":"a","op":"add","parent":"layout","title":"T","body":"","sources":["slack:c/1"]}"#,
                "`slack`",
            ),
            (
                "repo",
                r#"{"key":"a","op":"add","parent":"layout","title":"T","body":"","sources":["import:o.txt"]}"#,
                "`import`",
            ),
            (
                "repo",
                r#"{"key":"a","op":"add","parent":"layout","title":"T","body":"","sources":["nokind"]}"#,
                "nokind",
            ),
            // Scope, existence, fields per op, parent keys.
            (
                "repo",
                r#"{"key":"a","op":"add","parent":"done","title":"T","body":"","sources":["file:x"]}"#,
                "scope",
            ),
            (
                "repo",
                r#"{"key":"a","op":"update","id":"ghost","body":"b","sources":["file:x"]}"#,
                "`ghost`",
            ),
            (
                "repo",
                r#"{"key":"a","op":"update","id":"auth","parent":"layout","sources":["file:x"]}"#,
                "`parent`",
            ),
            (
                "repo",
                r#"{"key":"a","op":"remove","id":"auth","title":"T","evidence":"file:gone"}"#,
                "`title`",
            ),
            (
                "repo",
                r#"{"key":"a","op":"add","parent_key":"nope","title":"T","body":"","sources":["file:x"]}"#,
                "`nope`",
            ),
            (
                "repo",
                r#"{"key":"a","op":"move","id":"api","parent":"auth","sources":["file:x"]}"#,
                "under itself",
            ),
            (
                "repo",
                r#"{"key":"a","op":"remove","id":"auth","evidence":"import:o.txt"}"#,
                "`import`",
            ),
        ] {
            let e = check(&s, &item(by, json)).expect_err(json);
            assert!(e.contains(want), "want `{want}` for {json}: {e}");
        }
        for (by, json) in [
            (
                "repo",
                r#"{"key":"a","op":"add","parent":"api","title":"T","body":"","sources":["file:x","gh-pr:o/r#9"]}"#,
            ),
            (
                "repo",
                r#"{"key":"b","op":"add","parent_key":"new","title":"T","body":"","sources":["file:x"]}"#,
            ),
            (
                "github",
                r#"{"key":"a","op":"add_source","id":"auth","sources":["gh-pr:o/r#3"]}"#,
            ),
            (
                "github",
                r#"{"key":"a","op":"update","id":"api","body":"b","sources":["gh-pr:o/r#3"]}"#,
            ),
            (
                "repo",
                r#"{"key":"a","op":"remove","id":"auth","evidence":"file:gone"}"#,
            ),
            (
                "repo",
                r#"{"key":"a","op":"move","id":"auth","parent":"how-it-works","sources":["file:x"]}"#,
            ),
        ] {
            assert_eq!(check(&s, &item(by, json)), Ok(()), "{json}");
        }
    }

    #[test]
    fn two_proposals_on_one_node_conflict_each_with_the_other() {
        let a = item(
            "repo",
            r#"{"key":"a","op":"update","id":"api","body":"x","sources":["file:x"]}"#,
        );
        let b = item(
            "github",
            r#"{"key":"b","op":"add_source","id":"api","sources":["gh-pr:o/r#3"]}"#,
        );
        let c = item(
            "github",
            r#"{"key":"c","op":"add","parent":"api","title":"T","body":"","sources":["file:x"]}"#,
        );
        let found = conflicts(&[a.clone(), b.clone(), c]);
        assert_eq!(found.len(), 2);
        assert_eq!(found["repo/a"], [b.p]);
        assert_eq!(found["github/b"], [a.p]);
    }

    #[test]
    fn apply_adds_parents_before_children_and_assigns_unique_ids() {
        let (_t, s) = store(&[API, AUTH]);
        let child = item(
            "repo",
            r#"{"key":"a-child","op":"add","parent_key":"z-parent","title":"API","body":"","sources":["file:c"]}"#,
        );
        let parent = item(
            "repo",
            r#"{"key":"z-parent","op":"add","parent":"layout","title":"Queue","body":"q","sources":["file:q"]}"#,
        );
        let out = apply(&s, &rules(), &[child, parent], &BTreeMap::new());
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        assert_eq!(out.assigned["repo/z-parent"], "queue");
        assert_eq!(
            out.assigned["repo/a-child"], "api-2",
            "the title `API` is taken"
        );
        let q = &out.store.nodes["queue"];
        assert_eq!(
            (q.parent.as_deref(), q.order),
            (Some("layout"), 1),
            "new nodes go last"
        );
        assert_eq!(out.store.nodes["api-2"].parent.as_deref(), Some("queue"));
        let changes: Vec<_> = out
            .effects
            .iter()
            .map(|e| (e.change, e.id.as_str(), e.branch.as_str()))
            .collect();
        assert_eq!(
            changes,
            [("added", "queue", "layout"), ("added", "api-2", "layout")]
        );
    }

    #[test]
    fn merge_goes_in_plugin_order_then_by_key_and_ids_never_change() {
        let (_t, s) = store(&[API]);
        // `github` comes after `repo` in the config, so `repo`'s add takes the plain slug,
        // although its key sorts last.
        let late = item(
            "github",
            r#"{"key":"a","op":"add","parent":"layout","title":"Queue","body":"","sources":["gh-pr:o/r#4"]}"#,
        );
        let early = item(
            "repo",
            r#"{"key":"z","op":"add","parent":"layout","title":"Queue","body":"","sources":["file:q"]}"#,
        );
        let retitle = item(
            "repo",
            r#"{"key":"t","op":"update","id":"api","title":"Gateway","sources":["file:api.md"]}"#,
        );
        let out = apply(&s, &rules(), &[late, early, retitle], &BTreeMap::new());
        assert_eq!(out.assigned["repo/z"], "queue");
        assert_eq!(out.assigned["github/a"], "queue-2");
        assert_eq!(
            out.store.nodes["api"].title, "Gateway",
            "I4: the id stays when the title changes"
        );
    }

    #[test]
    fn update_keeps_bytes_when_nothing_changes_and_others_sources_in_place() {
        let (_t, s) = store(&[API, AUTH]);
        let same = item(
            "repo",
            r#"{"key":"a","op":"update","id":"api","title":"API","body":"HTTP API.","sources":["file:api.md"]}"#,
        );
        let out = apply(&s, &rules(), &[same], &BTreeMap::new());
        assert!(out.effects.is_empty(), "{:?}", out.effects);
        assert_eq!(out.store.nodes["api"], s.nodes["api"]);
        let new = item(
            "repo",
            r#"{"key":"a","op":"update","id":"api","body":"gRPC API.","sources":["file:api2.md"]}"#,
        );
        let out = apply(&s, &rules(), &[new], &BTreeMap::new());
        assert_eq!(
            out.store.nodes["api"].sources,
            ["gh-pr:o/r#1", "file:api2.md"]
        );
        assert_eq!(out.effects[0].fields, ["body", "sources"]);
        let cite = item(
            "github",
            r#"{"key":"c","op":"add_source","id":"auth","sources":["gh-pr:o/r#7"]}"#,
        );
        let out = apply(&s, &rules(), &[cite], &BTreeMap::new());
        assert_eq!(
            out.store.nodes["auth"].sources,
            ["file:auth.md", "gh-pr:o/r#7"]
        );
    }

    #[test]
    fn an_add_under_a_node_moved_out_of_its_scope_fails_in_either_order() {
        let (_t, s) = store(&[API]);
        let mut mv = item(
            "repo",
            r#"{"key":"m","op":"move","id":"api","parent":"how-it-works","sources":["file:x"]}"#,
        );
        let mut ad = item(
            "github",
            r#"{"key":"c","op":"add","parent":"api","title":"Child","body":"","sources":["gh-pr:o/r#9"]}"#,
        );
        for (rm, ra) in [(0, 1), (1, 0)] {
            (mv.rank, ad.rank) = (rm, ra);
            let out = apply(&s, &rules(), &[mv.clone(), ad.clone()], &BTreeMap::new());
            assert!(
                out.failed["github/c"].contains("outside the scope of `github`"),
                "{:?}",
                out.failed
            );
            assert!(!out.failed.contains_key("repo/m"), "{:?}", out.failed);
            assert_eq!(out.store.branch_of("api"), "how-it-works");
            assert!(!out.store.nodes.contains_key("child"));
            assert!(out.effects.iter().all(|e| e.id != "child"));
        }
    }

    #[test]
    fn the_net_diff_folds_rounds_and_drops_what_was_undone() {
        let (_t, s) = store(&[API, AUTH, THEIRS]);
        let r = |s: &Store, its: &[Item]| apply(s, &rules(), its, &BTreeMap::new());
        let one = r(
            &s,
            &[
                item(
                    "repo",
                    r#"{"key":"m","op":"move","id":"auth","parent":"how-it-works","sources":["file:x"]}"#,
                ),
                item(
                    "github",
                    r#"{"key":"t","op":"update","id":"theirs","title":"Other","sources":["gh-pr:o/r#2"]}"#,
                ),
            ],
        );
        let two = r(
            &one.store,
            &[
                item(
                    "repo",
                    r#"{"key":"u","op":"update","id":"auth","body":"Keys.","sources":["file:auth.md"]}"#,
                ),
                item(
                    "github",
                    r#"{"key":"b","op":"update","id":"theirs","title":"Theirs","sources":["gh-pr:o/r#2"]}"#,
                ),
            ],
        );
        let all: Vec<Effect> = one.effects.into_iter().chain(two.effects).collect();
        assert_eq!(all.len(), 4);
        let d: Vec<_> = net(&s, &two.store, &all)
            .into_iter()
            .map(|e| (e.change, e.branch, e.id, e.from, e.fields, e.key))
            .collect();
        assert_eq!(
            d,
            [
                (
                    "moved",
                    "how-it-works".into(),
                    "auth".into(),
                    Some("api".into()),
                    vec![],
                    "m".into()
                ),
                (
                    "updated",
                    "how-it-works".into(),
                    "auth".into(),
                    None,
                    vec!["body"],
                    "u".into()
                ),
            ]
        );
    }

    #[test]
    fn a_node_goes_with_its_children_or_not_at_all() {
        let (_t, s) = store(&[API, AUTH]);
        let parent = item(
            "repo",
            r#"{"key":"a","op":"remove","id":"api","evidence":"file:gone"}"#,
        );
        let child = item(
            "repo",
            r#"{"key":"b","op":"remove","id":"auth","evidence":"file:gone"}"#,
        );
        let out = apply(
            &s,
            &rules(),
            std::slice::from_ref(&parent),
            &BTreeMap::new(),
        );
        assert!(out.failed["repo/a"].contains("I3"), "{:?}", out.failed);
        assert!(out.store.nodes.contains_key("api"));
        let out = apply(&s, &rules(), &[parent.clone(), child], &BTreeMap::new());
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        assert!(!out.store.nodes.contains_key("api") && !out.store.nodes.contains_key("auth"));
        let moved = item(
            "repo",
            r#"{"key":"m","op":"move","id":"auth","parent":"how-it-works","sources":["file:x"]}"#,
        );
        let out = apply(&s, &rules(), &[parent, moved], &BTreeMap::new());
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        let e: Vec<_> = out
            .effects
            .iter()
            .map(|e| (e.change, e.id.as_str(), e.from.as_deref()))
            .collect();
        assert_eq!(
            e,
            [("moved", "auth", Some("api")), ("removed", "api", None)]
        );
    }

    #[test]
    fn failures_cascade_to_children_and_bad_nodes_fail() {
        let (_t, s) = store(&[API, AUTH]);
        let child = item(
            "repo",
            r#"{"key":"b","op":"add","parent_key":"a","title":"C","body":"","sources":["file:c"]}"#,
        );
        // The parent is not among the items: rejected before merge.
        let out = apply(&s, &rules(), std::slice::from_ref(&child), &BTreeMap::new());
        assert_eq!(out.failed["repo/b"], "parent rejected");
        let long = item(
            "repo",
            &format!(
                r#"{{"key":"a","op":"add","parent":"layout","title":"{}","body":"","sources":["file:c"]}}"#,
                "x".repeat(81)
            ),
        );
        let out = apply(&s, &rules(), &[long, child], &BTreeMap::new());
        assert!(out.failed["repo/a"].contains("title"), "{:?}", out.failed);
        assert_eq!(out.failed["repo/b"], "parent rejected");
        // A parent_key accepted in an earlier round resolves to its id.
        let earlier = BTreeMap::from([("repo/a".to_string(), "api".to_string())]);
        let child = item(
            "repo",
            r#"{"key":"b","op":"add","parent_key":"a","title":"C","body":"","sources":["file:c"]}"#,
        );
        let out = apply(&s, &rules(), &[child], &earlier);
        assert_eq!(out.store.nodes["c"].parent.as_deref(), Some("api"));
        // Two moves that would make a cycle: the second fails.
        let (_t, s) = store(&[
            API,
            AUTH,
            r#"{"id":"web","parent":"layout","title":"Web","body":"","sources":["file:w"],"order":1}"#,
        ]);
        let a = item(
            "repo",
            r#"{"key":"a","op":"move","id":"api","parent":"web","sources":["file:x"]}"#,
        );
        let b = item(
            "repo",
            r#"{"key":"b","op":"move","id":"web","parent":"auth","sources":["file:x"]}"#,
        );
        let out = apply(&s, &rules(), &[a, b], &BTreeMap::new());
        assert!(
            out.failed["repo/b"].contains("under itself"),
            "{:?}",
            out.failed
        );
    }
}

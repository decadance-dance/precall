//! The built-in checks of spec §10: `no-op`, `size-limits` and `secrets`. Each ships as a
//! manifest inside the binary and runs as `precall builtin <id>` through the check protocol of
//! §6.5, so a user can disable one or replace it by `id` like any other plugin (§6.2).

use crate::error;
use crate::node::{BODY_MAX, TITLE_MAX};
use crate::secrets;
use crate::wire::{JudgeInput, JudgeItem, Op, Verdict, VerdictKind};
use anyhow::{Context, Result, anyhow};

/// The built-in manifests. `precall` as the program stands for the running binary.
pub const MANIFESTS: [&str; 3] = [
    r#"id = "no-op"
name = "No rewording: rejects an update that changes only the wording"
version = "0.1.0"
min_precall = "0.1.0"
kind = "check"

[check]
command = ["precall", "builtin", "no-op"]
"#,
    r#"id = "size-limits"
name = "Title and body length limits"
version = "0.1.0"
min_precall = "0.1.0"
kind = "check"

[check]
command = ["precall", "builtin", "size-limits"]

[params]
title_max = 80
body_max = 600
"#,
    r#"id = "secrets"
name = "Rejects proposals that hold something that looks like a credential"
version = "0.1.0"
min_precall = "0.1.0"
kind = "check"

[check]
command = ["precall", "builtin", "secrets"]
"#,
];

/// Runs built-in check `id` on a §6.5 check input and returns its output document: one
/// verdict per proposal, in input order. An unknown `id` is a usage error.
pub fn run(id: &str, input: &str) -> Result<String> {
    if !matches!(id, "no-op" | "size-limits" | "secrets") {
        return Err(error::usage(format!("no built-in check `{id}`")));
    }
    // serde's message can quote the offending value, which may be a credential: say where only.
    let input: JudgeInput = serde_json::from_str(input).map_err(|e| {
        anyhow!(
            "the check input does not decode at line {} column {}",
            e.line(),
            e.column()
        )
    })?;
    let (title_max, body_max) = if id == "size-limits" {
        (
            limit(&input, "title_max", TITLE_MAX)?,
            limit(&input, "body_max", BODY_MAX)?,
        )
    } else {
        (TITLE_MAX, BODY_MAX)
    };
    let verdicts: Vec<Verdict> = input
        .proposals
        .iter()
        .map(|it| {
            let (verdict, reason) = match id {
                "no-op" => no_op(it),
                "size-limits" => size_limits(it, title_max, body_max),
                _ => secrets(it),
            };
            Verdict {
                key: it.key.clone(),
                verdict,
                reason,
                instruction: None,
            }
        })
        .collect();
    Ok(serde_json::json!({ "verdicts": verdicts }).to_string())
}

/// A limit param: a non-negative integer, else the §3.1 default when the core passes none.
/// The error does not quote the value.
fn limit(input: &JudgeInput, name: &str, default: usize) -> Result<usize> {
    match input.params.get(name) {
        None => Ok(default),
        Some(v) => v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .with_context(|| format!("param `{name}` must be a non-negative integer")),
    }
}

/// Lowercase, punctuation as a space, whitespace collapsed. Symbols that carry meaning
/// (`+`, `#`, `%`, `$`) stay, and so does the difference between `1.2` and `12`.
fn normalize(s: &str) -> String {
    const PUNCT: &str =
        ".,;:!?'\"`()[]{}-_/\u{2026}\u{2013}\u{2014}\u{2018}\u{2019}\u{201c}\u{201d}\u{ab}\u{bb}";
    let spaced: String = s
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| if PUNCT.contains(c) { ' ' } else { c })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Spec §5 "No rewording": an `update` whose title and body equal the current ones after
/// normalizing, and that sets no other date, no other state and no new source.
fn no_op(it: &JudgeItem) -> (VerdictKind, String) {
    let p = &it.proposal;
    let (Op::Update, Some(cur)) = (p.op, &it.current) else {
        return (
            VerdictKind::Accept,
            "not an update of an existing node".into(),
        );
    };
    let same_text = |new: &Option<String>, old: &str| {
        new.as_ref().is_none_or(|n| normalize(n) == normalize(old))
    };
    let same_opt = |new: &Option<String>, old: &Option<String>| new.is_none() || new == old;
    if same_text(&p.title, &cur.title)
        && same_text(&p.body, &cur.body)
        && same_opt(&p.date, &cur.date)
        && same_opt(&p.state, &cur.state)
        && p.sources.iter().all(|s| cur.sources.contains(s))
    {
        return (
            VerdictKind::Reject,
            "the update changes only wording: title and body equal the current ones after normalizing whitespace, case and punctuation, and it sets no new date, state or source".into(),
        );
    }
    (VerdictKind::Accept, "a real change".into())
}

/// Spec §3.1 limits on the text an `add` or `update` writes, counted in characters as the node
/// format counts them.
fn size_limits(it: &JudgeItem, title_max: usize, body_max: usize) -> (VerdictKind, String) {
    if !matches!(it.proposal.op, Op::Add | Op::Update) {
        return (VerdictKind::Accept, "writes no title or body".into());
    }
    let over: Vec<String> = [
        ("title", &it.proposal.title, title_max),
        ("body", &it.proposal.body, body_max),
    ]
    .into_iter()
    .filter_map(|(field, text, max)| {
        let n = text.as_ref()?.chars().count();
        (n > max).then(|| format!("{field} has {n} characters, over the limit of {max}"))
    })
    .collect();
    if over.is_empty() {
        (VerdictKind::Accept, "within the size limits".into())
    } else {
        (VerdictKind::Reject, over.join("; "))
    }
}

/// Every field a proposal could carry a credential in into the map, the commit message or the
/// report, its key included. The reason names the field and the pattern, never the text.
fn secrets(it: &JudgeItem) -> (VerdictKind, String) {
    let p = &it.proposal;
    let fields = [
        ("key", Some(it.key.as_str())),
        ("title", p.title.as_deref()),
        ("body", p.body.as_deref()),
        ("evidence", p.evidence.as_deref()),
        ("reason", Some(p.reason.as_str())),
    ]
    .into_iter()
    .chain(p.sources.iter().map(|s| ("sources", Some(s.as_str()))));
    let mut found: Vec<String> = fields
        .filter_map(|(field, text)| {
            secrets::find(text?).map(|name| format!("{field} matches the pattern `{name}`"))
        })
        .collect();
    found.dedup();
    if found.is_empty() {
        (VerdictKind::Accept, "no credential found".into())
    } else {
        (
            VerdictKind::Reject,
            format!("looks like a credential: {}", found.join("; ")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(proposal: serde_json::Value, current: Option<serde_json::Value>) -> String {
        let mut it = json!({ "key": "repo/p1", "proposal": proposal });
        if let Some(c) = current {
            it["current"] = c;
        }
        it.to_string()
    }

    fn node() -> serde_json::Value {
        json!({
            "id": "sync-service", "parent": "layout", "title": "Sync service",
            "body": "Merges read state across devices.", "sources": ["file:src/sync.rs@a4586a6"],
            "date": "2026-01-02", "state": "open", "order": 1
        })
    }

    fn update(fields: serde_json::Value) -> serde_json::Value {
        let mut p = json!({ "key": "p1", "op": "update", "id": "sync-service",
            "sources": ["file:src/sync.rs@a4586a6"], "reason": "r" });
        for (k, v) in fields.as_object().unwrap() {
            p[k] = v.clone();
        }
        p
    }

    /// Runs check `id` on the items and returns (verdict, reason) per item.
    fn verdicts(id: &str, items: &[String], params: serde_json::Value) -> Vec<(String, String)> {
        let input = format!(
            "{{\"proposals\":[{}],\"map_ids\":[\"layout\",\"sync-service\"],\"params\":{params}}}",
            items.join(",")
        );
        let out: serde_json::Value = serde_json::from_str(&run(id, &input).unwrap()).unwrap();
        out["verdicts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                (
                    v["verdict"].as_str().unwrap().to_string(),
                    v["reason"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    fn one(id: &str, item: String, params: serde_json::Value) -> (String, String) {
        verdicts(id, &[item], params).remove(0)
    }

    #[test]
    fn no_op_rejects_an_update_that_changes_only_wording() {
        for (case, fields, want) in [
            (
                "same text",
                json!({"title": "Sync service", "body": "Merges read state across devices."}),
                "reject",
            ),
            (
                "case, spaces, punctuation",
                json!({"title": "sync  SERVICE", "body": "merges read-state, across devices"}),
                "reject",
            ),
            (
                "title only, same",
                json!({"title": "Sync Service."}),
                "reject",
            ),
            ("nothing set", json!({}), "reject"),
            (
                "same date and state",
                json!({"body": "Merges read state across devices", "date": "2026-01-02", "state": "open"}),
                "reject",
            ),
            // Near-misses: real changes.
            (
                "new body",
                json!({"body": "Merges read state and stars across devices."}),
                "accept",
            ),
            (
                "decimal point",
                json!({"title": "Sync service v1.2"}),
                "accept",
            ),
            ("C++", json!({"title": "Sync service C++"}), "accept"),
            ("state change", json!({"state": "blocked"}), "accept"),
            ("date change", json!({"date": "2026-02-01"}), "accept"),
        ] {
            let (v, reason) = one("no-op", item(update(fields), Some(node())), json!({}));
            assert_eq!(v, want, "case `{case}`: {reason}");
        }
        let extra =
            update(json!({"sources": ["file:src/sync.rs@a4586a6", "file:src/merge.rs@b5"]}));
        assert_eq!(
            one("no-op", item(extra, Some(node())), json!({})).0,
            "accept",
            "new source"
        );
        let add = json!({"key": "p2", "op": "add", "parent": "layout", "title": "Sync service",
            "body": "Merges read state across devices.", "sources": ["file:a@b"], "reason": "r"});
        assert_eq!(one("no-op", item(add, None), json!({})).0, "accept", "add");
    }

    #[test]
    fn size_limits_reject_long_titles_and_bodies_with_the_lengths() {
        let params = json!({"title_max": 80, "body_max": 600});
        let p = |title: usize, body: usize| {
            json!({"key": "p", "op": "add", "parent": "layout", "title": "t".repeat(title),
                "body": "é".repeat(body), "sources": ["file:a@b"], "reason": "r"})
        };
        assert_eq!(
            one("size-limits", item(p(80, 600), None), params.clone()).0,
            "accept"
        );
        let (v, reason) = one("size-limits", item(p(81, 600), None), params.clone());
        assert_eq!(v, "reject");
        assert!(reason.contains("81") && reason.contains("80"), "{reason}");
        let (v, reason) = one("size-limits", item(p(10, 601), None), params.clone());
        assert_eq!(v, "reject");
        assert!(reason.contains("601") && reason.contains("600"), "{reason}");
        // Limits are params.
        let tight = json!({"title_max": 20, "body_max": 50});
        let (v, reason) = one("size-limits", item(p(21, 10), None), tight.clone());
        assert_eq!(v, "reject", "{reason}");
        assert_eq!(one("size-limits", item(p(20, 50), None), tight).0, "accept");
        // An update checks only the fields it sets.
        let u = update(json!({"body": "x".repeat(601)}));
        assert_eq!(
            one("size-limits", item(u, Some(node())), params.clone()).0,
            "reject"
        );
        assert_eq!(
            one("size-limits", item(update(json!({})), Some(node())), params).0,
            "accept"
        );
        // Defaults when the core passes no params; a negative limit fails the check.
        assert_eq!(
            one("size-limits", item(p(81, 1), None), json!({})).0,
            "reject"
        );
        let bad = format!(
            "{{\"proposals\":[{}],\"map_ids\":[],\"params\":{{\"title_max\":-1,\"body_max\":600}}}}",
            item(p(1, 1), None)
        );
        assert!(run("size-limits", &bad).is_err());
    }

    #[test]
    fn secrets_names_the_field_and_pattern_and_never_the_secret() {
        let secret = ["gh", "p_", "Zq3vK9mW2xR7tY5pL8nB4cD6fG1hJ0kAa2Ss"].concat();
        let base = || {
            json!({"key": "p", "op": "add", "parent": "layout", "title": "Deploy",
                "body": "Deploys with the CI token.", "sources": ["file:a@b"], "reason": "r"})
        };
        assert_eq!(one("secrets", item(base(), None), json!({})).0, "accept");
        for field in ["title", "body", "sources", "evidence", "reason"] {
            let mut p = base();
            let text = format!("uses {secret} here");
            p[field] = if field == "sources" {
                json!(["file:a@b", text])
            } else {
                json!(text)
            };
            let (v, reason) = one("secrets", item(p, None), json!({}));
            assert_eq!(v, "reject", "field `{field}`");
            assert!(
                reason.contains(field) && reason.contains("github-token"),
                "field `{field}`"
            );
            assert!(
                !reason.contains(&secret[4..]),
                "field `{field}`: the reason echoes the secret"
            );
        }
    }

    #[test]
    fn secrets_scans_the_proposal_key_and_keeps_the_key_as_given() {
        let secret = ["gh", "p_", "Zq3vK9mW2xR7tY5pL8nB4cD6fG1hJ0kAa2Ss"].concat();
        let p = json!({"key": secret, "op": "add", "parent": "layout", "title": "Deploy",
            "body": "Deploys.", "sources": ["file:a@b"], "reason": "r"});
        let it = json!({"key": format!("repo/{secret}"), "proposal": p}).to_string();
        let input = format!("{{\"proposals\":[{it}],\"map_ids\":[],\"params\":{{}}}}");
        let out = run("secrets", &input).unwrap();
        // §6.5: the verdict names the key exactly as given, or the core cannot match it.
        let keys = [format!("repo/{secret}")];
        let v = crate::wire::decode_verdicts("secrets", &out, &keys).unwrap();
        assert_eq!(v[0].verdict, VerdictKind::Reject);
        assert!(
            v[0].reason
                .contains("key matches the pattern `github-token`")
        );
        assert!(
            !v[0].reason.contains(&secret[4..]),
            "the reason echoes the secret"
        );
    }

    #[test]
    fn errors_never_echo_an_input_value() {
        let secret = ["gh", "p_", "Zq3vK9mW2xR7tY5pL8nB4cD6fG1hJ0kAa2Ss"].concat();
        let add = json!({"key": "p", "op": "add", "parent": "layout", "title": "t",
            "body": "b", "sources": ["file:a@b"], "reason": "r"});
        let with_params = |params: serde_json::Value| {
            format!(
                "{{\"proposals\":[{}],\"map_ids\":[],\"params\":{params}}}",
                item(add.clone(), None)
            )
        };
        // A size param is no business of the other checks.
        assert!(run("secrets", &with_params(json!({"title_max": secret}))).is_ok());
        let e = format!(
            "{:#}",
            run("size-limits", &with_params(json!({"title_max": secret}))).unwrap_err()
        );
        // Fixed messages: a failing assertion must not print the diagnostic it caught leaking.
        assert!(
            e.contains("title_max"),
            "bad param: the error does not name the param"
        );
        assert!(
            !e.contains(&secret[4..]),
            "bad param: the error quotes the value"
        );
        // A malformed input says where, not what.
        let mut bad = node();
        bad["order"] = json!(secret);
        let e = format!(
            "{:#}",
            run(
                "secrets",
                &format!(
                    "{{\"proposals\":[{}],\"map_ids\":[],\"params\":{{}}}}",
                    item(update(json!({})), Some(bad))
                )
            )
            .unwrap_err()
        );
        assert!(
            e.contains("line"),
            "bad input: the error does not say where"
        );
        assert!(
            !e.contains(&secret[4..]),
            "bad input: the error quotes the value"
        );
    }

    #[test]
    fn size_limits_apply_only_to_add_and_update() {
        let long = "é".repeat(81);
        for op in ["remove", "move", "add_source"] {
            let p = json!({"key": "p", "op": op, "id": "old", "parent": "layout",
                "title": long, "body": long.repeat(8), "evidence": "file:src/main.rs@a1",
                "sources": ["file:a@b"], "reason": "r"});
            assert_eq!(
                one("size-limits", item(p, Some(node())), json!({})).0,
                "accept",
                "{op}"
            );
        }
    }

    #[test]
    fn one_verdict_per_key_in_input_order() {
        let a = item(update(json!({"title": "Sync service"})), Some(node()));
        let b = item(update(json!({"state": "done"})), Some(node())).replace("repo/p1", "repo/p2");
        let input = format!("{{\"proposals\":[{a},{b}],\"map_ids\":[],\"params\":{{}}}}");
        let out = run("no-op", &input).unwrap();
        let keys = ["repo/p1".to_string(), "repo/p2".to_string()];
        let v = crate::wire::decode_verdicts("no-op", &out, &keys).unwrap();
        assert_eq!(v[0].verdict, VerdictKind::Reject);
        assert_eq!(v[1].verdict, VerdictKind::Accept);
        assert!(run("no-op", "not json").is_err());
        assert!(run("no-such-check", &input).is_err());
    }
}

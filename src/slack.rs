//! The built-in `slack` collector of spec §10: declarative, a manifest and two prompts compiled
//! into the binary. The agent that runs it reads the named channels and DMs through its own
//! Slack access (§12) and proposes decisions, people and blockers; `resolve.md` fetches the
//! messages its references name. No cheap fingerprint exists, so it runs on `min_interval`.

// limit: an incremental read starts at the later of one day before `since.at` and
// `lookback_days` before the run (`collect.md`), so a gap between runs longer than
// `lookback_days` loses what came before it, as a full read does.
/// The built-in manifest. It names no runner, so the instance's or the user config's applies
/// (§6.8); that runner must have Slack access.
pub const MANIFESTS: [&str; 1] = [r#"id = "slack"
name = "Slack: decisions, people and blockers from named channels and DMs"
version = "0.1.0"
min_precall = "0.1.0"
kind = "collector"

[collector]
scope = ["decisions", "people", "blocked"]
source_kinds = ["slack"]
min_interval = "1d"
prompt = "collect.md"
resolve_prompt = "resolve.md"

[params]
channels = []
dms = []
lookback_days = 14
"#];

/// The prompt files of the built-in collector, as (plugin id, file name, text).
pub const PROMPTS: [(&str, &str, &str); 2] = [
    ("slack", "collect.md", include_str!("slack/collect.md")),
    ("slack", "resolve.md", include_str!("slack/resolve.md")),
];

/// A `slack` instance that names no channel and no DM would still pay for a model call that
/// can only come back empty (spec §10): a config error instead. Each entry must name something.
pub fn check_params(params: &toml::Table) -> Result<(), String> {
    let mut named = 0;
    for k in ["channels", "dms"] {
        for (i, v) in params
            .get(k)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .enumerate()
        {
            match v.as_str() {
                Some(t) if !t.trim().is_empty() => named += 1,
                // Never the value: a wrong entry may hold private text.
                _ => return Err(format!("`{k}[{i}]` must be a nonblank name or id")),
            }
        }
    }
    if named == 0 {
        return Err("the built-in `slack` collector reads nothing: set `channels` or `dms`".into());
    }
    Ok(())
}

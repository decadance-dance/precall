//! The built-in `slack` collector of spec §10: declarative, a manifest and two prompts compiled
//! into the binary. The agent that runs it reads the named channels and DMs through its own
//! Slack access (§12) and proposes decisions, people and blockers; `resolve.md` fetches the
//! messages its references name. No cheap fingerprint exists, so it runs on `min_interval`.

/// The built-in manifest. It names no runner, so the instance's or the user config's applies
/// (§6.8); that runner must have Slack access.
pub const MANIFESTS: [&str; 1] = [r#"id = "slack"
name = "Slack: decisions, people and blockers from named channels and DMs"
version = "0.1.0"
min_rrecall = "0.1.0"
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

# rrecall

`rrecall` keeps a versioned map of a software project and answers questions about it: what lives where, how it works, who owns what, what was done lately, what is blocked and why decisions were made.

A run sends collectors over the project's sources (the repo, its tracker, its chat, your notes), merges what they find into the current map, and lets checks and judges send rejected changes back for rework before anything is committed. The map lives in its own git repository, so every change shows in `git diff` and `git blame`. People read it as an HTML page; coding agents query one branch at a time (`rrecall show decisions`).

Status: milestone 1: the map store and the query commands (`init`, `import`, `ls`, `show`, `find`, `why`, `recent`, `status`, `note`, `rm`, `agent-instructions`); milestone 2: the plugin protocol, `rrecall run` and the built-in checks; milestone 3 in progress: the built-in `claude` and `codex` runners and declarative plugins (a manifest and a prompt), the built-in judges `support`, `removal` and `duplicate`, and the built-in collectors `files`, `git-history`, `github` and `slack`. A run collects incrementally: `files` in a git repository, `github` and `slack` read only what changed since their last settled run (`slack` never further back than `lookback_days`), while `git-history` and `files` outside git always read in full; `rrecall run --force` reads everything again and can commit a lock-only change. The product and the plan: [docs/VISION.md](docs/VISION.md) and [docs/SPEC.md](docs/SPEC.md). Contributing: [AGENTS.md](AGENTS.md).

## Slack

The built-in `slack` collector reads named channels and DMs through an agent that can reach Slack: the claude.ai Slack connector of your Claude login. Enable a second `claude` runner instance that loads the connector (`connectors = true`) and may use only the connector's read tools, and let the collector run on it. Add to the store's `rrecall.toml`, next to the `claude` instance `init` enables (`init` does not add these):

```toml
[[plugins]]
id = "claude-slack"
use = "claude"

[plugins.params]
connectors = true
tools = [
  "mcp__claude_ai_Slack__slack_list_user_channels",
  "mcp__claude_ai_Slack__slack_search_channels",
  "mcp__claude_ai_Slack__slack_read_channel",
  "mcp__claude_ai_Slack__slack_read_thread",
  "mcp__claude_ai_Slack__slack_search_users",
  "mcp__claude_ai_Slack__slack_read_user_profile",
]

[[plugins]]
id = "slack"
runner = "claude-slack"
allowed_runners = ["claude-slack", "claude"]
params = { channels = ["eng-platform"], dms = [], lookback_days = 14 }
```

It runs at most once a day (`min_interval = "1d"`), reads from a day before its last settled run but never further back than `lookback_days` (the first run, and `--force`, read the last `lookback_days` days) and proposes decisions, people and blockers, each citing `slack:<channel-id>/<ts>`. `allowed_runners` keeps Slack text on these two runner instances; judges can stay on the plain `claude` one. `connectors = true` runs the agent in a fresh Claude Code config dir that keeps your login: your CLAUDE.md, settings, permission rules, hooks and local plugins stay out; what your account syncs from claude.ai (its connectors and synced plugins) still loads.

License: [Apache-2.0](LICENSE).

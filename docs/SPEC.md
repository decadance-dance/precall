# rrecall: product spec

Why rrecall exists and what it must never become: [VISION.md](VISION.md). This document is what it does and the plan to build it. Sections are cited as `spec §N`. A change to behavior described here updates this file in the same PR.

Status: milestone 1 (store and queries) built; milestone 2 next.

## 1. Terms

- **Project**: a software project the user maps. Identified by a name the user picks (default: the basename of the repo's `origin` remote).
- **Map**: the project's tree of nodes, stored in the map store.
- **Node**: one item of the map: a fact, a component, a person's role, a decision, a blocker.
- **Branch**: a top-level node. The set of branches is the **skeleton**.
- **Map store**: a git repository that holds one project's map, its config and its lock file. Never the project's own repository.
- **Source**: where facts come from: a repository, a tracker, a chat, a folder of notes.
- **Plugin**: a directory with a manifest and, optionally, executables and prompts. Kinds: collector, check, judge, view, runner, hook (§6).
- **Collector**: a plugin that reads a source and proposes changes to the map.
- **Check**: a plugin that accepts or rejects proposed changes by fixed rules, without a model.
- **Judge**: a plugin that accepts or rejects proposed changes with a model.
- **Runner**: a plugin that runs an agent (`claude -p`, `codex exec`, ...) for declarative collectors and judges.
- **View**: a plugin that renders the map for people (HTML) or other tools.
- **Run**: one execution of the pipeline (§4).
- **Proposal**: one change a collector wants: add, update, move or remove one node.
- **Invariant**: a stability rule the core enforces itself (§5). Plugins cannot turn one off.

## 2. Users and commands

| Who | Command | What it does |
| --- | --- | --- |
| Maintainer | `rrecall init [<project>]` | Creates the map store with the default skeleton and a config naming the sources found (repo path, `origin` remote), and registers the project in the user config. |
| Maintainer | `rrecall import <outline>` | Seeds an empty map from an indented outline (`title | detail` per line). Top-level lines that match a branch title or id fill that branch; the others become new branches, added to the config in the same commit. Every imported node gets the source `import:<file>`. A map that already has nodes is a usage error. |
| Maintainer | `rrecall run [--only <plugin>] [--force]` | Runs the pipeline (§4). `--force` ignores fingerprints and collects every source again. |
| Maintainer | `rrecall open` | Renders the HTML view and opens it. |
| Maintainer | `rrecall note <id> <text>` | Adds the user's annotation to a node; runs never change it. An empty text clears it. |
| Maintainer | `rrecall rm <id> [--reason <text>] [--recursive]` | Removes a node by hand, for the cases a run may not (a node with a note or children, §5 I3). A node with children needs `--recursive`, which removes them too. |
| Maintainer | `rrecall doctor` | Checks that each enabled plugin's requirements (binaries, logins, access) are met; prints what a run would skip. |
| Maintainer | `rrecall plugin list | add <git-url|path> [--ref <ref>] | trust <dir>` | Manages plugins (§6.7). |
| Agent and maintainer | `rrecall ls [--depth N]` | The tree of titles and ids. Default depth 2. |
| Agent and maintainer | `rrecall show <id> [--depth N] [--max-tokens N]` | One node and its subtree, with bodies and sources. |
| Agent and maintainer | `rrecall find <text>` | Nodes whose title or body matches, each with its path. |
| Agent and maintainer | `rrecall recent [--since 7d]` | What changed in the map in the window (from the map store's history) plus dated nodes in the window. |
| Agent and maintainer | `rrecall why <id>` | When the node appeared and changed, by which runs, with which sources and commit reasons (from `git log`/`git blame` of the store). |
| Agent and maintainer | `rrecall status` | Last run, its result, sources that were unreachable, proposals left rejected. |
| Maintainer | `rrecall agent-instructions` | Prints a short paragraph to paste into an agent's instruction file: that the map exists and which commands answer which question. |

The project is resolved from `--project`, else `RRECALL_PROJECT`, else the current directory's git `origin` remote looked up in the user config (§9). Remote URLs match as exact strings. With none of the three (no flag, no variable, no `origin` in the current directory), the command exits 2; an `origin` the user config does not list, or a project whose store does not exist, exits 3. Query commands print compact Markdown by default and JSON with `--json`.

`init` details:
- The project name is the argument, else `--project` or `RRECALL_PROJECT`, else the basename of the `origin` URL without `.git`; with none of these it exits 2. A name is a plain directory name (no `/`, not starting with `.`). A store that already exists exits 2.
- The store config names the sources found as the plugin instances `rrecall run` will use: inside a git repository, the top-level `repo = "<repo root>"` (§6.2) and `[[plugins]] id = "repo"`, `use = "files"`, `params = { path = "<repo root>", git = true, preset = "codebase" }`; for a GitHub `origin`, `[[plugins]] id = "github"`, `params = { repo = "<owner>/<name>" }`.
- When the user config has no entry for the project, `init` appends `[[projects]]` with `name` and `remotes = ["<origin url>"]` (§9), so later commands resolve the project from inside the repo. It never edits an existing entry.

`import` details: one node per line, `title | detail` split at the first `|` (the detail is optional), nesting by leading spaces (no tabs), blank lines skipped. A top-level line matches a branch by title or id, case-insensitively; a matching line may not carry a detail, because the branch's question comes from the config. A new top-level line becomes a branch whose question is its detail. Limits (§3.1) are checked before anything is written; a violation exits 2 naming the line.

Exit codes:
- `0`: done. For `run`: every collector ran and every proposal was settled (accepted, or rejected by checks and judges that ran); rejected proposals are a normal outcome, listed in the report.
- `1`: not found, or any other error (a corrupt map file, a git or file system failure); the message says which. `2`: usage error. `3`: no map for this project.
- `4`: the run was aborted before its commit (manifest error, a hook denied the run or the commit, another run holds the store lock). The store is unchanged.
- `5`: the run committed what was accepted, but not everything ran: a source was unreachable, the budget ran out, or a check or judge failed and its proposals were dropped. The report says which; the lock keeps those collectors unsettled (§4.2), so the next run collects them again.

## 3. The map

### 3.1 Node

| Field | Meaning |
| --- | --- |
| `id` | Stable identifier, a slug unique within the map (`search-service`). Assigned by the core when the node is created and never changed, also when the node moves or its title changes. |
| `parent` | The parent's `id`; absent for branches. |
| `title` | Short name, at most 80 characters. |
| `body` | The fact, at most 600 characters. Plain text. |
| `sources` | One or more source references (§3.3). Required for every node except branches, whose source is the config. |
| `date` | Optional: the date the fact is about (a release date, a decision date), `YYYY-MM-DD`. Never the date of a run. |
| `state` | Optional, for branches that track status: `open`, `blocked`, `done`. |
| `order` | Integer that orders siblings. Kept on update; new nodes go last. |
| `note` | The user's annotation (§2 `rrecall note`). Written only by the user. |

### 3.2 Storage

- Branches are not stored as lines: they come from the config, the six of §3.4 plus `[[skeleton.branch]]` entries, in that order. A branch shows as a node whose body is its question and whose source is `config:rrecall.toml`. `note` and `rm` on a branch exit 2 (I5).
- A map store holds: `map/<branch-id>.jsonl` (one file per branch, one node per line, holding every node under that branch; the file exists only while the branch has nodes), `rrecall.toml` (the project config, §9), `rrecall.lock` (fingerprints and plugin hashes, §4.2), `plugins/` (the project's own plugins, trusted because the user owns the store).
- Each line is one node as JSON with keys in a fixed order and no insignificant whitespace; lines are sorted by `id`. A node whose content did not change keeps a byte-identical line, so `git diff` shows only real changes and `git blame` attributes each node to the run that last changed it.
- Generated and private-to-the-run files (`cache/`, `runs/`, `views/`) and the plugins' persistent state (`state/<instance-id>/`, §6.3) are untracked; `init` lists all four in `.gitignore`.
- The store has no git remote by default. rrecall never adds one.
- `init` writes `.gitattributes` with `*.jsonl text eol=lf`, so map lines stay LF in the working tree whatever `core.autocrlf` says (I6). A command rewrites only the branch files whose nodes changed.
- Store commits are authored as `rrecall <rrecall@localhost>` and never signed, so they work with no git identity configured. A command commits exactly the files it wrote; other files in the store, staged or not, stay out of its commit. If the commit fails (a hook rejects it), the files get their previous content back and the command exits 1. Commits outside a run: `init <project>`; `import <file>: +<N> nodes, +<K> branches`; `note <branch>/<id>` (`: cleared` when emptied); `rm <branch>/<id>: <reason>` (default reason `removed by hand`) with one `removed <branch>/<id>` line per node removed.
- Default location: `$XDG_DATA_HOME/rrecall/<project>/` (`~/.local/share/rrecall/<project>/`), overridable per project in the user config.

### 3.3 Source references

A reference is a short string `<kind>:<locator>`, specific enough that a person or an agent can open it: `file:src/store.rs#L40-L80@a4586a6`, `gh-pr:owner/repo#212`, `gh-issue:owner/repo#17`, `commit:owner/repo@06aff55`, `slack:C0123ABCDEF/1795033170.000100`, `note:research/search-options.md`, `import:outline.txt`, `config:rrecall.toml`. Each kind belongs to the collector instance that declares it (§6.1 `source_kinds`, or a param such as `files`'s `ref_kind`); two enabled instances may not own the same kind, and the kinds `config` and `import` belong to the core. That instance's `resolve` entrypoint turns a reference back into text (§6.4). The core checks the format and that the kind has an owner.

### 3.4 Default skeleton

Six stored branches, one per question an agent asks, plus one computed answer:

| Branch id | Title | Question | Granularity told to collectors |
| --- | --- | --- | --- |
| `layout` | Layout | Where does something live? | one node per component, directory or service |
| `how-it-works` | How it works | How does something work? | one node per mechanism or flow |
| `people` | People | Who is responsible for what? | one node per person or team, with their areas |
| `done` | Done | What was done? | one node per release or finished stream of work, not per commit or PR |
| `blocked` | Blocked | What is open or blocked, and on what? | one node per blocker or open question |
| `decisions` | Decisions | Why was a decision made? | one node per decision, with its reason, date and source |
| `recent` (computed) | | What was worked on lately? | answered by `rrecall recent` from the store's history and dated nodes; not stored, so it does not churn |

A project adds branches in its config (`[[skeleton.branch]]` with `id`, `title`, `question`, `granularity`; for example `services`). The granularity text goes into every collector's input for that branch. A run never adds, removes or renames a branch; only a config change does, and it shows as its own commit.

## 4. The run pipeline

### 4.1 Steps

1. **Load.** Read the user config, the store's config and lock, and every enabled plugin's manifest. A manifest error stops the run before anything is collected (exit 4).
2. **Fingerprint.** For each collector, compute its combined fingerprint: the output of its `fingerprint` entrypoint (cheap and deterministic, for example `git rev-parse HEAD`, the newest `updatedAt` of the tracker's items, a hash of the notes folder), the plugin's content hash, its prompt's hash, its effective params, and the runner and model names as configured. A collector without a cheap fingerprint declares `min_interval` instead (§6.1) and is due when that interval has passed since its last settled run. A collector whose fingerprint equals the lock's settled one, or that is not yet due, is skipped; its nodes stay as they are.
3. **Collect.** Run the remaining collectors in parallel. Each gets on stdin the current nodes in its scope and returns proposals (§6.4). A collector that fails, times out or returns output that does not decode proposes nothing; its source is reported unreachable and it stays unsettled.
4. **Validate.** The core enforces the invariants (§5) on each proposal and rejects violations with a reason, before any plugin sees them.
5. **Merge.** Apply the surviving proposals to a copy of the map, in plugin order, then by proposal key. Two proposals that change the same node in the same round are both rejected with the reason "conflict", each carrying the other's proposal, and go back to their collectors (step 8). If they still conflict after the last round, neither is applied.
6. **Diff.** Compute the node diff against the current map: added, updated (which fields), moved, removed.
7. **Check, then judge.** Run every enabled check over the diff, then every enabled judge over what the checks accepted. Each returns `accept`, `reject` with a reason, or `revise` with an instruction, per proposal. Judges get the source text resolved through the owning collector's `resolve` entrypoint (§6.4). A proposal is accepted only if every check and judge accepted it. A proposal whose parent proposal (§6.4 `parent_key`) was not accepted is rejected with the reason "parent rejected".
8. **Back pressure.** Proposals rejected or marked `revise` go back to their collector with the verdicts, and steps 3 to 7 run again for those proposals only. At most `max_rounds` rounds (default 2). After the last round, what is still not accepted is dropped from this run and listed in the report.
9. **Commit.** Write the map files (§3.2) and the lock. If either changed, commit them with a structured message (§4.3); a run that changes only the lock commits with the subject `run <time>: no map changes`. If neither changed: no commit. Write the run report to `runs/<run-id>/report.md` in every case.
10. **Render.** Run the views (`views/`). A view failure is reported and does not change the exit code.

A run takes a lock on the store; a second run on the same store waits or fails fast (`--no-wait`, exit 4).

### 4.2 Lock file and settled collectors

`rrecall.lock` records, per collector: the combined fingerprint of its last **settled** run, the plugin's source and content hash, the runner and model used (as returned by the runner), and the time of the last settled run (used only for `min_interval`). A collector's run is settled when it ran without failure and every proposal it made ended accepted, or rejected by checks and judges that themselves ran. A collector whose proposals were dropped because a check or judge failed, or because the budget ran out, is not settled: its lock entry does not advance, and the next run collects it again. Per check, judge and view the lock records the plugin's source and content hash. The lock is tracked, so the history shows when a plugin or prompt changed, and a diff after a prompt change is attributed to it.

### 4.3 Commit message

```
run 2026-11-25T09:14Z: +4 ~2 -1 (rejected 3)

added   decisions/postgres-for-queue  by github   [gh-pr:org/repo#109]
updated blocked/deploy-key (body)     by tracker  [issue:org/repo#55]
removed blocked/staging-access        by tracker  evidence: issue closed as completed [issue:org/repo#368]
rejected 3, see runs/<run-id>/report.md
unreachable: chat (login expired)
```

### 4.4 Run report

Untracked Markdown in `runs/<run-id>/`: every proposal with its verdicts per round, unreachable sources with the error, unsettled collectors and why, cost per plugin, wall time. `rrecall status` summarizes the last one.

## 5. Stability rules

The invariants are enforced by the core; no configuration turns them off. Plugins (checks and judges) add judgement on top. Each rule has a test in M2 unless it says otherwise.

- **I1. Unchanged inputs, no model calls.** If no collector is due (§4.1 step 2), the run calls no runner and makes no commit. Fingerprint entrypoints still run. Test: second run over an unchanged fixture calls no runner and leaves the store byte-identical.
- **I2. No removal without evidence.** A `remove` proposal must carry `evidence`: a source reference showing the fact is no longer true (a closed issue, a deleted file at a commit, a reversing decision). A node missing from a collector's output is never a removal. The core rejects a `remove` without evidence; the built-in `removal` judge checks that the evidence says what it claims.
- **I3. No collateral loss.** A run never removes a node that has a `note`, and never removes a node with children unless every child is moved or removed with evidence in the same run. Such a proposal is rejected and listed in the report so the user can decide (`rrecall rm`).
- **I4. Stable identity.** A node's `id` never changes. Collectors refer to existing nodes by `id`; new nodes get ids from the core.
- **I5. Fixed skeleton.** Only the config and `rrecall import` change branches.
- **I6. Deterministic files.** The same map serializes to the same bytes on every platform.
- **I7. Notes are the user's.** The proposal schema has no `note` field; output that carries one does not decode.
- **I8. Ownership.** A collector may update, move or remove a node only if the node has at least one source of a kind that collector declares. For any other node it may only propose `add_source`. An `update` may replace only the sources of the collector's own kinds; others' sources are kept.
- **I9. No silent conflicts.** Two changes to the same node in one round are handled as in §4.1 step 5; neither wins by order.
- **I10. Nothing unsettled is forgotten.** §4.2: a collector whose proposals were dropped for a failure is collected again.

Plugin-level rules (built-in, can be disabled or replaced):

- **No rewording.** The `no-op` check rejects an `update` whose title and body are identical to the current ones after normalizing whitespace, case and punctuation. The `support` judge rejects an `update` that keeps the meaning and changes only the wording, and an `add` or `update` its resolved source does not back.
- **Duplicates.** The `duplicate` judge rejects an `add` that restates an existing node.

Measured, not enforced:

- **Drift under `--force`.** Running `rrecall run --force` twice in a row over unchanged sources changes at most 5% of nodes. This is the acceptance criterion of M3's live run, reported as a metric in every forced run's report. Normal runs over unchanged sources change nothing (I1); the drift bound is about how well collectors and judges hold the map when they do run.

## 6. Plugin protocol

### 6.1 Unit and manifest

A plugin is a directory with `rrecall-plugin.toml`. Unknown keys are an error at any level.

```toml
id = "github"                      # kebab-case, unique among discovered plugins (instances: §6.2)
name = "GitHub pull requests and issues"
version = "0.1.0"
min_rrecall = "0.1.0"
kind = "collector"                 # collector | check | judge | view | runner | hook
requires = ["gh"]                  # binaries rrecall doctor checks on PATH

[collector]
scope = ["done", "blocked", "decisions", "people"]
source_kinds = ["gh-pr", "gh-issue", "commit"]
fingerprint = ["gh", "api", "repos/{repo}/events", "--jq", ".[0].id"]
resolve = ["./resolve.sh"]         # reads references on stdin, prints their text (§6.4)
command = ["./collect.sh"]         # argv, run in the plugin's directory; no shell
timeout = "10m"

[params]                           # names and defaults the store's config may set
repo = ""
```

A declarative plugin replaces `command` with a prompt run by a runner. A source with no cheap fingerprint uses `min_interval`:

```toml
[collector]
scope = ["decisions", "people"]
source_kinds = ["slack"]
min_interval = "1d"                # due at most once a day; replaces `fingerprint`
prompt = "prompt.md"               # rendered with the input JSON and the proposal schema
resolve_prompt = "resolve.md"      # the runner fetches the referenced messages
runner = "claude"                  # a runner plugin id; the store's config may override
model = "default"
```

Every manifest has exactly the table named by its `kind`; a table of another kind is an error.

| Table | Keys |
| --- | --- |
| `[collector]` | `scope` and `source_kinds` (each at least one entry); `fingerprint` or `min_interval`, not both; `command` or `prompt`, not both; optionally `resolve` or `resolve_prompt`; `runner` and `model` only with `prompt` or `resolve_prompt`; `timeout` |
| `[check]`, `[view]`, `[runner]` | `command`, `timeout` |
| `[judge]` | `command` or `prompt`, not both; `runner` and `model` only with `prompt`; `timeout` |
| `[hook]` | `events` (at least one event of §6.6), `command`, `timeout` |

- An argv is a list of strings; its program is looked up on `PATH`, or, when it holds a `/`, taken relative to the plugin directory. A prompt file is a path inside the plugin directory and must exist. The manifest, prompt files and programs given relative to the plugin directory must stay inside it with links resolved, and a plugin directory inside its place (§6.2), so what runs is what the content hash covers.
- Durations are `<N>s`, `<N>m`, `<N>h` or `<N>d`. `timeout` defaults to 30s for a hook and 10m for every other kind; an instance's `timeout` (§9) overrides it.
- `id` is kebab-case; `version` and `min_rrecall` are `x.y.z`, and a `min_rrecall` newer than the running rrecall is an error.
- `[params]` names each param with its default. A name is a lowercase letter followed by lowercase letters, digits, `_` and `-`; a value is anything TOML holds that JSON can hold too: no date-time, no `inf` or `nan`, also inside arrays and tables. An instance may set only declared params, each with the same TOML type as its default.
- `{name}` inside an argv entry or a `source_kinds` entry is replaced by the instance's param `name` (a string as it is, any other value as JSON), so `files` declares `source_kinds = ["{ref_kind}"]`. Braces around anything but a param name (a jq filter) stay as they are; a `{name}` that names no param is an error.
- A manifest error names the plugin and the key; any manifest, discovery or instance error stops a run before anything runs (exit 4).

### 6.2 Discovery and order

Plugins come from four places, in this order: built-in (shipped inside the binary), the user directory `$XDG_CONFIG_HOME/rrecall/plugins/`, a project repository's `.rrecall/plugins/` once trusted (§6.7), and the store's `plugins/`. A later one with the same `id` replaces an earlier one. The store's config enables plugins and sets their params; the order of `[[plugins]]` entries there is the run order wherever order matters (§4.1 steps 5 and 7, §6.6).

The project's repository is the store config's top-level `repo` (an absolute path, written by `init`), so a run loads the same plugins from any directory; without it there is no third place. A place is a directory whose subdirectories holding `rrecall-plugin.toml` are plugins; two plugins with one `id` in the same place are an error.

One plugin can be enabled several times. Each `[[plugins]]` entry is an **instance** with its own `id`, the plugin it `use`s, and its own params; the instance id is what the lock, the report, the commit message and `--only` name. Without `use`, the entry's `id` is the plugin's id and there is one instance. Instance ids are kebab-case and unique; an entry that names no discovered plugin, sets an undeclared param or a param of another type, or scopes a collector to a branch the map does not have, is a config error (exit 4).

```toml
[[plugins]]
id = "repo"
use = "files"
params = { path = ".", git = true, preset = "codebase" }

[[plugins]]
id = "notes"
use = "files"
params = { path = "/home/me/notes/myproject", preset = "notes", ref_kind = "note" }
```

### 6.3 Process contract

- The argv runs in the plugin's directory as its own process group, never through a shell: the program must be a native executable or a script starting with `#!`, since the C library would hand any other file to `/bin/sh`. At the timeout the whole group is killed.
- Input: one JSON document on stdin. Output: one JSON document on stdout. Logs: stderr. Both are kept in the run dir as `runs/<run-id>/<instance>.<entrypoint>.stdout` and `.stderr`. A non-zero exit, a timeout or more than 64 MiB of stdout is a failure.
- Environment: rrecall's own, plus `RRECALL_BIN` (the rrecall binary), `RRECALL_PROJECT`, `RRECALL_STORE`, `RRECALL_PLUGIN_DIR`, `RRECALL_STATE_DIR` (the instance's own persistent directory, `<store>/state/<instance-id>/`, created before the call), `RRECALL_RUN_ID`, and each param as `RRECALL_PARAM_<NAME>`: the name upper-cased with `-` as `_`, a string as it is, any other value as JSON. Two params that map to one variable are a manifest error; `RRECALL_PARAM_*` variables in rrecall's own environment are not passed on.
- A plugin reads the map through the CLI (`$RRECALL_BIN show <id> --json`); there is no other API.
- Output is untrusted. Decoding is strict on shape (the document is one JSON object; unknown fields, a key twice at any level, wrong types, `null` for a field (only an unresolved text may be `null`), a missing required field such as `sources`): one item that does not decode rejects the whole output, naming the plugin and the item. Rule violations in well-formed proposals (a removal without evidence, a change to another collector's node) are rejected per proposal by the invariants (§5), with a reason the collector sees in the next round.

### 6.4 Collector input and output

Input: `{ "project", "run_id", "round", "scope": [{ "id", "title", "question", "granularity" }], "nodes": [current nodes in scope], "rejections": [the previous round's proposals with their verdicts, when round > 1], "params" }`. A rejection is `{ "proposal", "verdicts": [ { "by", "verdict", "reason", "instruction"?, "conflict_with"?: <proposal> } ] }`; `by` is the check or judge instance id, or `core` for an invariant or a conflict, which carries the other proposal in `conflict_with`.

Output: `{ "proposals": [ { "key", "op": "add" | "update" | "move" | "remove" | "add_source", "id"?, "parent"?, "parent_key"?, "title"?, "body"?, "sources", "date"?, "state"?, "evidence"?, "reason" } ] }`.
- `key` is non-empty, unique within the output, and names the proposal in verdicts and in the next round.
- Required by `op`: `add` needs `title`, `body` and exactly one of `parent` and `parent_key`; `move` needs `id` and exactly one of `parent` and `parent_key`; `update`, `remove` and `add_source` need `id`. A proposal that lacks one does not decode.
- A new node's parent is either an existing `id` (`parent`) or another proposal's `key` (`parent_key`), so a collector can add a parent and its children at once.
- `reason` is one line that goes into the commit message. `evidence` is a source reference, required for `remove`.

Resolve: the `resolve` entrypoint gets `{ "refs": [references] }` and returns `{ "texts": { "<ref>": "<text or null>" } }`; a reference it omits is unresolved, and one it was not asked for fails the output. A reference that does not resolve is passed to judges as unresolved, and a judge may not accept a claim whose only sources are unresolved.

### 6.5 Check and judge input and output

Input: `{ "proposals": [ { "key", "proposal", "current"?: <node>, "sources": { "<ref>": "<resolved text or null>" } } ], "map_ids": [...], "params" }`, where `key` is `<collector instance>/<proposal key>`. Checks get the same input without `sources`. Output: `{ "verdicts": [ { "key", "verdict": "accept" | "reject" | "revise", "reason", "instruction"? } ] }`, exactly one verdict per key given; a verdict for a key not given, a key twice, or `revise` without `instruction` fails the whole output. A check or judge that fails, times out or omits a verdict accepts nothing it was given: those proposals are dropped, and their collectors stay unsettled (§4.2).

### 6.6 Hooks and events

| Event | When | A hook may |
| --- | --- | --- |
| `run.before` | before collecting | deny the run |
| `collect.after` | each collector's raw proposals | observe |
| `diff.after` | the merged diff | observe |
| `commit.before` | the final diff and message | deny the commit |
| `commit.after` | after the commit | observe |
| `view.render` | after the commit | observe |

Only `*.before` events can deny, and a hook on them fails closed. Hooks run in config order with a timeout (default 30s).

A hook gets `{ "event", "project", "run_id", "data" }`, `data` being the event's payload. On a `*.before` event it prints `{}` to allow or `{ "deny": "<reason>" }` to deny; a non-zero exit, a timeout or any other output denies too, and the first deny aborts the run (exit 4) without running the hooks after it. On the other events its output is ignored and a failure is reported without changing the run.

### 6.7 Installing and trusting

- `rrecall plugin add <git-url|path> [--ref <ref>]` copies the plugin into the user directory, shows its manifest and the commands it will run, and records its source and content hash in the user config. `--ref` pins a tag or commit.
- A `.rrecall/plugins/` directory inside a project's own repository is never loaded unless the user runs `rrecall plugin trust <dir>`; the decision is remembered per path and content hash, and a changed hash needs trust again. The record is a user-config entry `[[trusted]]` with `path` (the absolute path of the `.rrecall/plugins/` directory) and `hash`: `sha256:<hex>` over every entry under the directory (its relative path, whether it is a directory, a symlink, an executable or a plain file, and its content; a symlink by its target). An untrusted directory is skipped; an instance that needs one of its plugins fails with a message naming the directory to trust.
- Plugins run with the user's permissions; rrecall is not a sandbox. The manifest preview is the safety step.

### 6.8 Runner contract and what reaches a model

A runner gets `{ "prompt", "input", "output_schema", "model", "timeout", "read_only": true }` and returns `{ "output": <JSON matching the schema>, "cost_usd", "model" }`.
- `read_only` means the agent may read files, run read-only commands and use its configured tools (for example a chat connector), and may not write files, push or post anything.
- `cost_usd` is required; a runner that cannot report cost estimates it from token counts and a price table in its params. `model` is the model the provider actually used; it goes into the lock.
- The built-in runners call `claude -p` and `codex exec`, with the prompt passed by file or stdin, never on the command line, and with a wrap-up message when the timeout nears. They are adapted from the headless runner of rreview (same author, same license).
- Sending source text and nodes to the runner's model provider is the same exposure as any coding-agent session on that machine; rrecall itself publishes and stores nothing off the machine (VISION principle 8). Each source can restrict which runners may receive its content (`allowed_runners` in the store config, §9); the core refuses to give that collector's nodes or resolved texts to any other runner, including judges.

## 7. Query output for agents

- Markdown, one node per block: `## <path> (<id>)`, the body, then `sources:` with the short references, then `date`, `state` and `note` when set. The path is the titles from the branch down, joined by ` > `. Children appear as titles with ids (`- <title> (<id>)`) unless `--depth` asks for more; a title line ends with `+N` when N descendants are not shown.
- `ls` prints titles and ids to depth 2 by default; for the reference map (about 300 nodes) that stays under about 1,500 tokens. Deeper levels are one `show <id>` away.
- `--max-tokens N` cuts the output at a node boundary and ends with how many nodes were left out and the command to see them. Tokens are estimated as bytes / 4, rounded up. The first node is always printed.
- `--json` prints the same nodes as JSON for scripts and plugins (§6.3): `show` and `find` print `{"nodes": [...]}`, each node with its stored fields plus `path` and `children` (the ids of its children); a `--max-tokens` cut adds `"left_out": [ids]`. `ls` prints `{"nodes": [{"id", "parent", "title"}]}` (`parent` absent for branches).

## 8. Views

The built-in `html` view writes one self-contained `views/index.html`: a collapsible mind map, the full outline with search, a node inspector with sources, the user's note and the node's history (from `git log` of the store), and a filter "changed since the last run". It works offline and loads nothing from the network. `rrecall open` renders and opens it.

## 9. Configuration

Both configs reject unknown keys at every level.

- **User config** `$XDG_CONFIG_HOME/rrecall/config.toml`: projects as `[[projects]]` with `name`, an optional absolute `store` path and `remotes` (the URLs that map to it), the default `runner` and `model`, installed plugins with their sources and hashes (M4), and trusted plugin directories as `[[trusted]]` (§6.7).
- **Store config** `rrecall.toml`: the project's `repo` (§6.2); enabled plugins in order as `[[plugins]]` with `id`, `use`, `params`, `runner`, `model` (both only for a plugin with a prompt), `timeout` and `allowed_runners` (collectors only); skeleton additions (`[[skeleton.branch]]`); `max_rounds`; and a cost cap per run (`max_cost_usd`). The core checks the cap before starting each runner call; calls already in flight may finish, so a run can exceed the cap by at most the cost of the calls in flight. When the cap is reached, pending proposals are dropped with the reason "budget", their collectors stay unsettled, and the run exits 5.

## 10. Built-in plugins for the first release

| Kind | Id | Reads | How |
| --- | --- | --- | --- |
| collector | `files` | one file, or the files in a folder that match `include` and not `exclude` (globs; default: every text file, minus `.gitignore`d ones under git) | prompt + runner. With `git = true`: fingerprint is a hash of the matched files' blob ids at `HEAD` (a commit that touches none of them does not trigger a run), references `file:<path>#<lines>@<commit>`, resolve reads the file at that commit. Without git: fingerprint is a hash of the matched files' contents, references `<ref_kind>:<path>`, resolve reads the current file. `preset` picks the prompt and default scope: `codebase` (docs, specs, manifests, layout → `layout`, `how-it-works`, `decisions`) or `notes` (informal notes that may be stale or contradict each other → `decisions`, `blocked`, `people`). A typical project enables it twice, as `repo` and `notes`. |
| collector | `git-history` | merged branches, tags, release notes | script, no model |
| collector | `github` | PRs, issues, releases via `gh` | script fetches; prompt + runner proposes; resolve via `gh` |
| collector | `slack` | named channels and DMs, through an agent that has Slack access | prompt + runner; `min_interval`; resolve through the runner |
| check | `no-op`, `size-limits`, `secrets` | the diff | code |
| judge | `support`, `removal`, `duplicate` | the diff and the resolved sources | prompt + runner |
| runner | `claude`, `codex` | | adapted from rreview |
| view | `html` | the map | code |

The invariants of §5 are core code, not plugins. Anything else (another tracker, past agent sessions, a wiki) is a plugin outside the core.

## 11. Milestones

Each milestone is one GitHub issue (or a few), merged into `develop` before the next starts.

- **M1. Store and queries.** Node model, the JSONL store in git, deterministic serialization, `init`, `import`, `ls`, `show`, `find`, `why`, `recent`, `status`, `note`, `rm`, `agent-instructions`. Tests: round-trip of an imported outline, byte-identical rewrite, query output, token size of `ls` on a 300-node fixture, exit codes.
- **M2. The pipeline with fake plugins.** Manifest loading and validation, discovery and trust, plugin instances (`use`), the process contract, fingerprints, `min_interval` and the lock with settled state, the invariants, merge and conflicts, node diff, the built-in checks, the back-pressure loop, commit message, run report, exit codes 4 and 5, the cost cap with fake costs, hooks. Tests with fake collectors and judges for every invariant in §5 (a collector that forgets a node, one that rewords, one that removes without evidence, one that removes a node with a note or children, one that edits another collector's node, two that conflict, one that crashes, a judge that times out and leaves its collector unsettled) and for I1.
- **M3. Runners and model plugins.** The `claude` and `codex` runners, declarative plugins and `resolve`, `allowed_runners`, the built-in collectors and judges. A live test on a real project: build its map from scratch, run normally twice (I1 holds), run `--force` twice and measure drift (§5, at most 5%).
- **M4. People and other users.** The `html` view, `open`, `doctor`, `plugin add/trust`, docs for plugin authors with one example plugin outside the core.
- **M5. Release.** Install with `cargo install` and a Homebrew tap; a release workflow; the first tagged version.

## 12. Open questions

- **Headless access to chat.** Whether `claude -p` sees the claude.ai Slack connector without an interactive session has not been checked. If not, the `slack` collector needs its own token and a script. Check at the start of M3.
- **Growth of `done` and `decisions`.** Facts there stay true, so I2 never removes them, and the branches grow. The granularity rules (§3.4) slow that; if they are not enough, a `merge` operation (several nodes into one summary, keeping all sources) is the likely next step. Decide after M3 shows the growth rate.
- **Node size.** 600 characters for `body` is a guess from the reference map; adjust after M3's live run.
- **Judge cost.** Judging every proposal with a model may cost more than collecting. Judging only updates and removals, or sampling adds, are options after M3 measures it.
- **Sharing code with rreview.** M3 copies and adapts its runner; a shared crate is a later issue if both keep changing it.

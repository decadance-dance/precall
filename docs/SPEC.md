# rrecall: product spec

Why rrecall exists and what it must never become: [VISION.md](VISION.md). This document is what it does and the plan to build it. Sections are cited as `spec §N`. A change to behavior described here updates this file in the same PR.

Status: milestones 1 (store and queries) and 2 (plugin protocol and run pipeline) built; milestone 3 in progress (the `claude` and `codex` runners, declarative plugins, the built-in judges and the built-in collectors `files`, `git-history`, `github` and `slack`, and incremental collection built).

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
| Maintainer | `rrecall run [--only <instance>] [--force] [--no-wait]` | Runs the pipeline (§4). `--force` ignores fingerprints and collects every source again with a full read (`since` is `null`, §4.5). `--only` runs one collector instance (another id exits 2), still subject to its fingerprint unless `--force`. `--no-wait` exits 4 instead of waiting when another run holds the store lock. |
| Maintainer | `rrecall open` | Renders the HTML view (§8) to the store's `views/index.html`, prints its path and opens it with the program `RRECALL_BROWSER` names (given the file's path as its one argument), else `open` on macOS and `xdg-open` elsewhere. It renders with the built-in `html` view whatever the store's config enables, and holds no lock. An opener that fails or exits non-zero exits 1 naming the file, which stays written. |
| Maintainer | `rrecall note <id> <text>` | Adds the user's annotation to a node; runs never change it. An empty text clears it. |
| Maintainer | `rrecall rm <id> [--reason <text>] [--recursive]` | Removes a node by hand, for the cases a run may not (a node with a note or children, §5 I3). A node with children needs `--recursive`, which removes them too. |
| Maintainer | `rrecall doctor` | Checks that each enabled plugin instance can run, one `ok` or `fail` line per check with the reason: each `requires` binary on `PATH`, each program of a plugin that is not built in, a built-in `claude` or `codex` runner's login (`claude auth status`, `codex login status`: no model call), a declarative plugin's runner (an enabled runner instance that passes its own checks), and a collector's access by running its fingerprint (§6.3; its logs go to the store's git-ignored `cache/doctor/`, not `runs/`, and its `RRECALL_STATE_DIR` is a fresh scratch directory there, not the instance's `state/`; a `min_interval` collector has none, so only a run checks its access). Then it prints what a run would skip and exits 1 when anything failed. A plugin config that does not load, or that `allowed_runners` rules out (§6.8), fails as `run` would (exit 4). It takes no store lock and runs nothing else. |
| Maintainer | `rrecall plugin list | add <git-url|path> [--ref <ref>] | trust <dir>` | Manages plugins (§6.7). `list` prints one line per discovered plugin: id, kind, origin (built-in, user, project, store), version, content hash, then the instances that use it, or `replaced by <origin>` for one a later place replaces, and for an installed one `from <source>[@<ref>]` (with `changed since add` when its hash no longer matches the record); an untrusted `.rrecall/plugins/` is named with the command that trusts it. Without a resolvable project it lists the built-in and user plugins only and says so. |
| Agent and maintainer | `rrecall ls [--depth N]` | The tree of titles and ids. Default depth 2. |
| Agent and maintainer | `rrecall show <id> [--depth N] [--max-tokens N]` | One node and its subtree, with bodies and sources. |
| Agent and maintainer | `rrecall find <text>` | Nodes whose title or body matches, each with its path. |
| Agent and maintainer | `rrecall recent [--since 7d]` | What changed in the map in the window (from the map store's history) plus dated nodes in the window. |
| Agent and maintainer | `rrecall why <id>` | When the node appeared and changed, by which runs, with which sources and commit reasons (from `git log`/`git blame` of the store). |
| Agent and maintainer | `rrecall status` | Last run, its result, sources that were unreachable, proposals left rejected: the `## Summary` section of the last run report (§4.4). |
| Maintainer | `rrecall agent-instructions` | Prints a short paragraph to paste into an agent's instruction file: that the map exists and which commands answer which question. |

The project is resolved from `--project`, else `RRECALL_PROJECT`, else the current directory's git `origin` remote looked up in the user config (§9). Remote URLs match as exact strings. With none of the three (no flag, no variable, no `origin` in the current directory), the command exits 2; an `origin` the user config does not list, or a project whose store does not exist, exits 3. Query commands print compact Markdown by default and JSON with `--json`.

`init` details:
- The project name is the argument, else `--project` or `RRECALL_PROJECT`, else the basename of the `origin` URL without `.git`; with none of these it exits 2. A name is a plain directory name (no `/`, not starting with `.`). A store that already exists exits 2.
- The store config names the sources found as the plugin instances `rrecall run` will use: inside a git repository, the top-level `repo = "<repo root>"` (§6.2) and `[[plugins]] id = "repo"`, `use = "files"`, `params = { path = "<repo root>", git = true, preset = "codebase" }`; for a GitHub `origin`, `[[plugins]] id = "github"`, `params = { repo = "<owner>/<name>" }`. After the sources, always, the built-in checks in this order: `[[plugins]] id = "no-op"`, `id = "size-limits"`, `id = "secrets"`; then the built-in judges `id = "support"`, `id = "removal"`, `id = "duplicate"` (§10); then the instance of the user config's default runner, `[[plugins]] id = "claude"` unless the user config names another (§6.8); last, the built-in view, `[[plugins]] id = "html"` (§8). That default is an instance id (§9), so `init` enables it only when a runner plugin with that id is found (the built-ins, the user plugin directory, the repository's trusted `.rrecall/plugins/`, the store's `plugins/` when its directory already exists) and its id is not one of the instances `init` writes; otherwise it exits 2 before creating anything and says to set `runner` to a runner plugin id or add the instance to the store's config after `init`. The user config is written before the store and restored if creating the store fails, so a failed `init` leaves nothing behind and can run again.
- When the user config has no entry for the project, `init` appends `[[projects]]` with `name` and `remotes = ["<origin url>"]` (§9), so later commands resolve the project from inside the repo. It never edits an existing entry. When the user config has no default `runner`, `init` writes `runner = "claude"` at its top (§9), so the built-in judges, whose manifests name no runner, have one.

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

A reference is a short string `<kind>:<locator>`, specific enough that a person or an agent can open it: `file:src/store.rs#L40-L80@a4586a6`, `gh-pr:owner/repo#212`, `gh-issue:owner/repo#17`, `gh-release:owner/repo@v1.2.0`, `commit:owner/repo@06aff55`, `slack:C0123ABCDEF/1795033170.000100`, `note:research/search-options.md`, `import:outline.txt`, `config:rrecall.toml`. Each kind belongs to the collector instance that declares it (§6.1 `source_kinds`, or a param such as `files`'s `ref_kind`); two enabled instances may not own the same kind, and the kinds `config` and `import` belong to the core. That instance's `resolve` entrypoint turns a reference back into text (§6.4). The core checks the format and that the kind has an owner.

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
2. **Fingerprint.** For each collector, compute its combined fingerprint: the output of its `fingerprint` entrypoint (cheap and deterministic, for example `git rev-parse HEAD`, the newest `updatedAt` of the tracker's items, a hash of the notes folder), the plugin's content hash, its prompt's hash, its effective params, and the runner and model names as configured, with the built-in `claude` runner's `effort` when set (§6.8). A collector without a cheap fingerprint declares `min_interval` instead (§6.1) and is due when that interval has passed since its last settled run. A collector whose fingerprint equals the lock's settled one, or that is not yet due, is skipped; its nodes stay as they are.
   - The entrypoint gets `{ "project", "run_id", "params" }` on stdin; its stdout is taken as raw text, not JSON. The combined fingerprint is `sha256:<hex>` over that output, the plugin's content hash (§6.7, which covers the prompt), the effective params and the configured runner and model, and the built-in `claude` runner's effort when set.
   - A `min_interval` collector hashes the same without an output, and is due when that hash differs from the lock's or the interval has passed.
   - A fingerprint entrypoint that fails makes its collector unreachable: not collected, unsettled (its lock entry gets the `unsettled` mark, §4.2), exit 5.
   - If no collector is due and no lock entry needs a new `unsettled` mark, the run stops here: it writes the report and runs no hook, check, judge or view and makes no commit (I1). Otherwise `run.before` fires once, after this step and before collecting.
3. **Collect.** Run the remaining collectors in parallel. Each gets on stdin the current nodes in its scope and where its last settled run stopped (`since`, §4.5), and returns proposals (§6.4). A collector that fails, times out or returns output that does not decode proposes nothing; its source is reported unreachable and it stays unsettled.
4. **Validate.** The core enforces the invariants (§5) on each proposal and rejects violations with a reason, before any plugin sees them.
5. **Merge.** Apply the surviving proposals to a copy of the map, in plugin order, then by proposal key. Two proposals that change the same node in the same round are both rejected with the reason "conflict", each carrying the other's proposal, and go back to their collectors (step 8). If they still conflict after the last round, neither is applied.
6. **Diff.** Compute the node diff against the current map: added, updated (which fields), moved, removed.
7. **Check, then judge.** Run every enabled check over the diff, then every enabled judge over what the checks accepted. Each returns `accept`, `reject` with a reason, or `revise` with an instruction, per proposal. Judges get the source text resolved through the owning collector's `resolve` entrypoint (§6.4). A proposal is accepted only if every check and judge accepted it. A proposal whose parent proposal (§6.4 `parent_key`) was not accepted is rejected with the reason "parent rejected". An `add` or `move` that, once the accepted proposals are applied, lands in another branch than a judge was shown it under, or that a judge's batch gave no place to land (a move of an ancestor was not accepted), is rejected by the core, since no judge compared it with the nodes there.
   - The accepted proposals are applied again: an add whose parent was not accepted ("parent rejected"), or a removal whose child's removal or move was not accepted (I3), is rejected in turn and goes back like any rejection.
8. **Back pressure.** Proposals rejected or marked `revise` go back to their collector with the verdicts, and steps 3 to 7 run again for those proposals only. At most `max_rounds` rounds (default 2). After the last round, what is still not accepted is dropped from this run and listed in the report.
   - Round N > 1 calls only the collectors with proposals sent back, with those in `rejections` and the current map: proposals accepted in earlier rounds stay accepted and are applied.
   - Only proposals whose `key` was sent back count; other keys are ignored and reported. A key sent back but not returned is withdrawn.
   - `parent_key` may name an `add` of the same collector accepted in an earlier round.
9. **Commit.** Write the map files (§3.2) and the lock. If either changed, commit them with a structured message (§4.3); a run that changes only the lock commits with the subject `run <time>: no map changes`. If neither changed: no commit. Write the run report to `runs/<run-id>/report.md` in every case.
10. **Render.** Run the views (`views/`). A view failure is reported and does not change the exit code.

A run takes a lock on the store; a second run on the same store waits or fails fast (`--no-wait`, exit 4). The lock is an exclusive `flock` on `.git/rrecall-run.lock`: the OS releases it when the process ends, and it never shows in the working tree. `note`, `rm` and `import` wait for it too, so a run cannot commit its in-memory map over their change.

The run id is the start time in UTC, `YYYY-MM-DDTHHMMSSZ` (`-2`, `-3`, ... when one of the same second exists), so the last run sorts last by name.

### 4.2 Lock file and settled collectors

`rrecall.lock` records, per collector: the combined fingerprint of its last **settled** run, a hash of that fingerprint's non-source parts, the plugin's source and content hash, the runner and model used (as returned by the runner), the time of the last settled run (for `min_interval` and `since.at`, §4.5), and the cursor that run returned. A collector's run is settled when it ran without failure and every proposal it made ended accepted, or rejected by checks and judges that themselves ran. A collector that failed (its fingerprint entrypoint or its collect call), or whose proposals were dropped because a check or judge failed, a judge left a verdict out even when asked again (§6.5), or the budget ran out, is not settled: its lock entry does not advance, and the next run collects it again. The entry records that with `"unsettled": true`, so the next run collects it whatever its fingerprint or `min_interval` says: after a `--force` run whose attempt failed, and after a failing fingerprint even when the recovered one equals the settled one; the next settled run drops the mark. Per check, judge and view the lock records the plugin's source and content hash. The lock is tracked, so the history shows when a plugin or prompt changed, and a diff after a prompt change is attributed to it.

The file is JSON with two-space indentation, keys in this order, a final newline:

```json
{
  "collectors": {
    "<instance>": { "fingerprint": "sha256:...", "setup": "sha256:...", "plugin": "plugins/files", "hash": "sha256:...", "runner": "claude", "model": "...", "settled_at": "2026-11-25T09:14:00Z", "cursor": <JSON>, "unsettled": true }
  },
  "plugins": {
    "<instance>": { "plugin": "...", "hash": "sha256:..." }
  }
}
```

- `plugin` is the plugin directory, relative to the store when inside it, else absolute; `hash` its content hash (§6.7). A built-in plugin (§6.2) has no directory of its own: `plugin` is `builtin` and `hash` is `sha256:<hex>` over the rrecall version, the plugin's manifest and its built-in prompt files (each as its file name and text, by name), so an upgrade of rrecall shows in the lock.
- `setup` is `sha256:<hex>` over the plugin's content hash, the effective params and the configured runner and model, and the built-in `claude` runner's effort (when set; an unset one, or another runner's param of that name, leaves the hash as it was before the param existed): the combined fingerprint without the entrypoint's output. When it changes, the cursor no longer applies (§4.5).
- `runner` and `model` only for a collector a runner ran (its collect call, else its `resolve_prompt`). `settled_at` (UTC) is the start time of the last settled run, written on every settled run, so a successful forced run over unchanged sources can commit a lock-only change with the subject `run <time>: no map changes` (none when it starts in the same second as the last settled run) (I1 still holds: a run with nothing due writes nothing).
- `cursor` only when the last settled run returned one (§4.5): an opaque JSON value the core keeps as given and passes back. It advances only when a run settles; an unsettled attempt keeps the settled one. A cursor the `secrets` scanner (§10) matches in its JSON text is not kept, and the report warns, since redacting it would corrupt it.
- `unsettled` only while the collector's latest attempt has not settled.
- `plugins` lists the check, judge and view instances. Entries of instances no longer configured are dropped whenever the lock is written.

### 4.3 Commit message

```
run 2026-11-25T09:14Z: +4 ~2 -1 (rejected 3)

added   decisions/postgres-for-queue  by github   [gh-pr:org/repo#109]
updated blocked/deploy-key (body)     by tracker  [issue:org/repo#55]
removed blocked/staging-access        by tracker  evidence: issue closed as completed [issue:org/repo#368]
rejected 3, see runs/<run-id>/report.md
unreachable: chat (login expired)
```

- The changes are the run's net diff against the map before the run, the same diff `commit.before` gets: a node changed in several rounds shows once per kind of change, a change undone in a later round not at all, and a line names the last proposal of its kind on the node. A removed node shows under the branch it was in. The run report keeps each round's changes (§4.4).
- One line per change, columns aligned: `added|updated|moved|removed <branch>/<id>`, then `(<fields>)` for an update (`order` when a node moved away and back in one run, since it lands last among its siblings) or `(from <old parent>)` for a move, `by <collector>`, the proposal's one-line `reason` and its sources in brackets; a removal shows `evidence: <reason> [<evidence>]`. An `add_source` shows as an update of `sources`.
- The subject counts `+added ~updated-or-moved -removed`, and `(rejected N)` when proposals were left rejected.
- After the changes: `rejected N, see runs/<run-id>/report.md`, `unreachable: <id> (<first line of the error>), ...`, and `unsettled: <id> (<why>), ...` for collectors left unsettled for another reason.
- The message is redacted like the logs (§6.3) before `commit.before` sees it, since its error lines quote a plugin's stderr or a decode error.

### 4.4 Run report

Untracked Markdown in `runs/<run-id>/`: every proposal with its verdicts per round, unreachable sources with the error, unsettled collectors and why, cost and tokens per plugin, wall time. `rrecall status` summarizes the last one.

It starts with `# Run <run-id>` and a `## Summary` section (result and exit code, commit, collected and skipped collectors, unreachable and unsettled ones, each judge skipped for holding none of its ops and why, each judge asked again for verdicts it left out with the keys asked and those dropped (§6.5), the count left rejected, drift on a `--force` run (§5), cost, tokens, wall time), then `## Proposals` (one `### <collector>/<key>` per proposal, its outcome and verdicts per round) and `## Warnings` (hooks, views, resolve). `rrecall status` prints the summary; `rrecall run` prints it too.

The `tokens` line sums the `usage` of every runner call (§6.8), every round and call (`resolve` included), per plugin instance, in the form `- tokens: input N (cached C), output O; <plugin>: input N (cached C), output O; ...` (plugins in id order). Input counts every input token, uncached, read from a cache and written to one; cached is the part read from a cache. A plugin whose runner reported no usage is left out; the line is absent when no call reported any. On a subscription login the cost is what the tokens would cost at API prices; the tokens are what was used.

The report quotes plugins (proposal keys and titles, verdict reasons, error lines), so it is redacted like the logs (§6.3) before it is written or printed.

### 4.5 Incremental collection

A due collector reads only what changed since its last settled run; a full read happens on `--force` and whenever the collector itself changes.

| | Read everything again | Read only what is new since the last settled run |
| --- | --- | --- |
| Noise in the diff | The agent sees the whole source each time: more proposals, more chances to reword, more work for checks and judges. Stability rests on the filters alone. | The agent proposes from new evidence only, so the diff follows what happened: the weekly diff reads like a changelog (VISION). |
| Speed and cost | Grows with the size of the project: one edit in the repo means a full pass. | Grows with the amount of change; a quiet week costs almost nothing. |
| Self-correction | Strong: the agent sees the whole picture again, finds what it missed the first time and fixes its own mistakes. | Weak: what was missed the first time never appears, and nobody fixes a mistake. |
| Deletions and late edits | Seen at once. | Seen only when the source reports them: `git diff` shows deleted files; in Slack an edit to an old message or a new reply in an old thread is easy to miss. |
| For an agent reading the map | More accurate over time, but wording moves more often. | Nodes stay stable and wording does not drift; that matters more to an agent that remembered a node. |

Hence: incremental by default, for the noise, cost and stability rows; a full read on `--force` (for example a monthly `--force` run) and whenever the collector's plugin, prompt, params, runner or model change, for the self-correction and late-edit rows. The collector still gets the current nodes in its scope, so new evidence can update or remove an existing node.

How it works:
- Every collect input and every `fetch` input carries `since`: `{ "at": <settled_at of the lock entry>, "cursor": <its cursor, or null> }`, or `null` for a full read. `since` is `null` on the first run, on `--force`, when the entry's `setup` differs from the current one or is missing, and when the entry has no `settled_at` (its last settled state is unknown). An unsettled entry still has its last settled state, so a retry reads from there. `since` is the same in every round of a run.
- A collector returns where it stopped as `cursor`: a top-level key of its `fetch` output when that output is an object (`null` or absent: none), or of a command collector's collect output (§6.4). Only the fetch, or round 1's collect, counts. A cursor is any JSON value but `null`, at most 4 KiB as JSON, whose numbers are integers that fit a 64-bit signed or unsigned integer, so it comes back exactly as given; a larger one, or one holding another number (a fraction, an exponent, a larger integer), fails the output like any malformed output (§6.3), naming the instance. A collector that needs an exact position beyond that puts it in a string. A settled run that returns no cursor clears the stored one.
- What a built-in does with it is in §10: `files` with git and `github` keep a cursor; `slack` reads from `since.at`, its window capped at `lookback_days`; `git-history` (a script, no model, cheap) and `files` without git always read everything.

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

- **No rewording.** The `no-op` check rejects an `update` whose title and body are identical to the current ones after normalizing whitespace, case and punctuation, and that sets no other `date` or `state` and no source the node lacks; an update that changes only the state, the date or adds a source is a real change. Normalizing lowercases, turns punctuation (`. , ; : ! ? ' " ( ) [ ] { } - _ /`, dashes, ellipsis and typographic quotes) into spaces and collapses whitespace; symbols such as `+`, `#`, `%` and `$` stay, so `v1.2` and `v12` or `C++` and `C` differ. Every other op is accepted. The `support` judge rejects an `update` that keeps the meaning and changes only the wording, and an `add` or `update` its resolved source does not back.
- **Duplicates.** The `duplicate` judge rejects an `add` that restates an existing node.

Measured, not enforced:

- **Drift under `--force`.** Running `rrecall run --force` twice in a row over unchanged sources changes at most 5% of nodes. This is the acceptance criterion of M3's live run, reported as a metric in every forced run's report. Normal runs over unchanged sources change nothing (I1); the drift bound is about how well collectors and judges hold the map when they do run.
  - Drift is the number of nodes the run's net diff (§4.3) added, updated, moved or removed, over the nodes in the map before or after the run (the nodes before plus those added); branches do not count, and a node both updated and moved counts once. A forced run shows it, once its changes are written, in the report's summary (§4.4), after `rejected`: `- drift: 2 of 13 nodes changed (15.4%): 2 added, 0 updated, 0 moved, 0 removed`, the percentage with one decimal; a forced run that changes nothing shows `0 of <total> nodes changed (0.0%)`. A normal run, and a run that stops before writing its changes, show no drift.

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
source_kinds = ["gh-pr", "gh-issue", "gh-release"]
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
| `[collector]` | `scope` and `source_kinds` (each at least one entry); `fingerprint` or `min_interval`, not both; `command` or `prompt`, not both; `fetch` only with `prompt`; optionally `resolve` or `resolve_prompt`; `runner` and `model` only with `prompt` or `resolve_prompt`; `timeout` |
| `[check]`, `[view]`, `[runner]` | `command`, `timeout` |
| `[judge]` | `command` or `prompt`, not both; `runner` and `model` only with `prompt`; optionally `ops`, the ops it checks (at least one of `add`, `update`, `move`, `remove`, `add_source`; default every op, §6.5); `timeout` |
| `[hook]` | `events` (at least one event of §6.6), `command`, `timeout` |

- An argv is a list of strings. Its program, after `{param}` expansion, is an absolute path, a path with a `/` taken relative to the plugin directory, or a bare name looked up on `PATH` (relative `PATH` entries against rrecall's working directory). rrecall resolves it once to an absolute path and both checks and runs that path. An absolute program path is allowed and lies outside the plugin and its content hash. A prompt file is a path inside the plugin directory and must exist; a built-in plugin (§6.2) has no directory, so its prompt files are compiled into the binary and the name must be one of them. The manifest, prompt files and programs given relative to the plugin directory must stay inside it with links resolved, and a plugin directory inside its place (§6.2), so what runs is what the content hash covers.
- Durations are `<N>s`, `<N>m`, `<N>h` or `<N>d`. `timeout` defaults to 30s for a hook and 10m for every other kind; an instance's `timeout` (§9) overrides it.
- `id` is kebab-case; `version` and `min_rrecall` are `x.y.z`, and a `min_rrecall` newer than the running rrecall is an error.
- `[params]` names each param with its default. A name is a lowercase letter followed by lowercase letters, digits, `_` and `-`; a value is anything TOML holds that JSON can hold too: no date-time, no `inf` or `nan`, also inside arrays and tables. An instance may set only declared params, each with the same TOML type as its default.
- `{name}` inside an argv entry or a `source_kinds` entry is replaced by the instance's param `name` (a string as it is, any other value as JSON), so `files` declares `source_kinds = ["{ref_kind}"]`. Braces around anything but a param name (a jq filter) stay as they are; a `{name}` that names no param is an error.
- `fetch` (an argv, only with `prompt`) is how a script gives a declarative collector its source data, when the agent cannot read the source itself (an API it has no tool for, the commit to cite). The core runs it once per run, on the instance's first collect, with `{ "project", "run_id", "params", "since" }` on stdin (`since` as in §4.5), and passes its stdout, decoded as strictly as any plugin output (§6.3), as `fetched` in every collect input of that instance in the run. A fetch that fails, times out or prints anything but one JSON document makes its collector unreachable, as a failing collect call does (§4.1 step 3). A top-level `cursor` of an object output is the collector's cursor (§4.5); one that breaks the cursor rules makes the collector unreachable too.
- A manifest error names the plugin and the key; any manifest, discovery or instance error stops a run before anything runs (exit 4).

### 6.2 Discovery and order

Plugins come from four places, in this order: built-in (shipped inside the binary, see below), the user directory `$XDG_CONFIG_HOME/rrecall/plugins/`, a project repository's `.rrecall/plugins/` once trusted (§6.7), and the store's `plugins/`. A later one with the same `id` replaces an earlier one. The store's config enables plugins and sets their params; the order of `[[plugins]]` entries there is the run order wherever order matters (§4.1 steps 5 and 7, §6.6).

A built-in plugin is a manifest compiled into the binary and checked like any other. A built-in with a `command` (the checks, the runners) runs the running rrecall binary by absolute path, taken as it is (never one found on `PATH`, never `{param}`-expanded), with a hidden subcommand, `rrecall builtin <id>`, so it runs through the process contract (§6.3) and speaks its kind's protocol like any plugin; its directory (working directory and `RRECALL_PLUGIN_DIR`) is the binary's directory. A declarative built-in (the judges) has no `command`: its prompt files are compiled in (§6.1) and its configured runner runs it (§6.8). It runs only when a `[[plugins]]` entry enables it: removing the entry disables it, and a plugin with the same `id` in a later place replaces it. The lock records a built-in plugin's source as `builtin`.

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

- The argv runs in the plugin's directory as its own process group, never through a shell: the program must be a native executable, or a script whose `#!` line names a native interpreter by absolute path (`#!/bin/sh`, `#!/usr/bin/env python3`), since the C library would hand any other file to `/bin/sh`. At the timeout the whole group is killed.
- Input: one JSON document on stdin. Output: one JSON document on stdout. Logs: stderr. Both are kept in the run dir as `runs/<run-id>/<instance>.<entrypoint>.stdout` and `.stderr`; from round 2 on (§4.1 step 8) the entrypoint gets `.<round>` appended, a runner call logs as `<runner>.<caller>.<entrypoint>`, and a hook as `<hook>.<event>`, with `.<collector>` for `collect.after` and `.<round>` from round 2. A non-zero exit, a timeout or more than 64 MiB of stdout is a failure.
- Credentials a plugin prints, in the run dir. While the process runs, its stdout and stderr go to files that have no name in the run dir; when it ends (a failure or a timeout included), the core writes them under the names above as text with every match of the `secrets` scanner (§10) replaced by `[redacted:<pattern>]`. A log longer than 64 MiB keeps its first 64 MiB and a line saying it was cut; a match that starts before the cut is redacted whole, read on up to 64 KiB past it. When a plugin exits non-zero, the error quotes its last non-blank stderr line, redacted, from the last 64 KiB of stderr (a line that starts before them is not quoted); a timeout or an oversized stdout names the stderr log instead. The logs are scanned as printed: a credential a plugin escapes in its JSON (`\uXXXX`) stays there. The report (§4.4) is redacted the same way, and so is each value it quotes before it is shortened for display.
- Credentials a plugin prints, in tracked files. What the core copies from plugin or runner output into the store's tracked files is scanned as decoded values: the node fields of an accepted proposal (title, body, sources, date, state) before they enter the map, the model a runner reports before it enters the lock, a collector's cursor (kept only when nothing matches, §4.2), and the commit message (§4.3), each value whole before it is shortened. A proposal is validated (shape and limits, §3.1, §6.4) on its own values first, and the node it stores is validated again after redaction, so redaction never makes an invalid proposal pass. With the `secrets` check enabled such a proposal is rejected first; the redaction is the net without it.
- Both are core code and run even when the `secrets` check is disabled. The core reads the output as printed, so checks still see a credential and reject it. The redaction catches what that check catches: a credential the scanner misses stays, and a look-alike that is no credential is redacted too. rrecall's own error output on the terminal and a plugin's state directory are not redacted.
- Environment: rrecall's own, plus `RRECALL_BIN` (the rrecall binary), `RRECALL_PROJECT`, `RRECALL_STORE`, `RRECALL_PLUGIN_DIR`, `RRECALL_STATE_DIR` (the instance's own persistent directory, `<store>/state/<instance-id>/`, created before the call), `RRECALL_RUN_ID`, and each param as `RRECALL_PARAM_<NAME>`: the name upper-cased with `-` as `_`, a string as it is, any other value as JSON. Two params that map to one variable are a manifest error; `RRECALL_PARAM_*` variables in rrecall's own environment are not passed on.
- A plugin reads the map through the CLI (`$RRECALL_BIN show <id> --json`); there is no other API.
- Output is untrusted. Decoding is strict on shape (the document is one JSON object; unknown fields, a key twice at any level, wrong types, `null` for a field (only an unresolved text may be `null`), a missing required field such as `sources`): one item that does not decode rejects the whole output, naming the plugin and the item. Rule violations in well-formed proposals (a removal without evidence, a change to another collector's node) are rejected per proposal by the invariants (§5), with a reason the collector sees in the next round.

### 6.4 Collector input and output

Input: `{ "project", "run_id", "round", "scope": [{ "id", "title", "question", "granularity" }], "nodes": [current nodes in scope], "rejections": [the previous round's proposals with their verdicts, when round > 1], "params", "fetched"?: <the output of the collector's `fetch` entrypoint, only when it declares one (§6.1)>, "since": <where the last settled run stopped, or null for a full read (§4.5)> }`. A rejection is `{ "proposal", "verdicts": [ { "by", "verdict", "reason", "instruction"?, "conflict_with"?: <proposal> } ] }`; `by` is the check or judge instance id, or `core` for an invariant or a conflict, which carries the other proposal in `conflict_with`.

Output: `{ "proposals": [ { "key", "op": "add" | "update" | "move" | "remove" | "add_source", "id"?, "parent"?, "parent_key"?, "title"?, "body"?, "sources", "date"?, "state"?, "evidence"?, "reason" } ], "cursor"?: <JSON> }`. Only a command collector returns `cursor` (§4.5); a declarative one returns it through its `fetch`, and its agent's output has `proposals` only.
- `key` is non-empty, unique within the output, and names the proposal in verdicts and in the next round.
- Required by `op`: `add` needs `title`, `body` and exactly one of `parent` and `parent_key`; `move` needs `id` and exactly one of `parent` and `parent_key`; `update`, `remove` and `add_source` need `id`. A proposal that lacks one does not decode.
- A new node's parent is either an existing `id` (`parent`) or another proposal's `key` (`parent_key`), so a collector can add a parent and its children at once.
- `reason` is one line that goes into the commit message. `evidence` is a source reference, required for `remove`.

Besides the invariants of §5, the core rejects a proposal (step 4) when:
- it carries a field its op does not take: `add` takes `parent` or `parent_key`, `title`, `body`, `date`, `state`; `update` takes `id`, `title`, `body`, `date`, `state`; `move` takes `id` and `parent` or `parent_key`; `remove` takes `id` and `evidence`; `add_source` takes `id` (all take `sources`). An `add` with `id` breaks I4; an `update` with a parent is a `move`.
- a source or the evidence is malformed, of a kind no enabled collector owns, or of the core's kinds `config` and `import`;
- an `update` or `add_source` has no sources, or one not of the collector's own kinds. An `update` replaces the collector's own sources and keeps the others where they are, so resending the same sources changes no byte; `add_source` appends;
- its target or new parent is not in the map, or sits under a branch outside the collector's `scope`; a `move` would put a node under itself;
- the merged node breaks a limit of §3.1.

Any two proposals on one node `id` in one round conflict (I9), `add_source` included.

Resolve: the `resolve` entrypoint gets `{ "refs": [references] }` and returns `{ "texts": { "<ref>": "<text or null>" } }`; a reference it omits is unresolved, and one it was not asked for fails the output. A reference that does not resolve is passed to judges as unresolved, and a judge may not accept a claim whose only sources are unresolved: the core turns a judge's `accept` on a proposal whose every cited reference (its sources and evidence) is unresolved into a `reject` by that judge, with a reason the collector sees.

### 6.5 Check and judge input and output

Input: `{ "proposals": [ { "key", "proposal", "current"?: <node>, "sources": { "<ref>": "<resolved text or null>" } } ], "nodes": [<node>...], "map_ids": [...], "params" }`, where `key` is `<collector instance>/<proposal key>` and `nodes` holds the current nodes (the map before the round, branches left out) of every branch an `add` or `move` in the batch lands in when exactly that batch is applied, so a judge can compare a new node with the ones already there. Checks get the same input without `sources`. Output: `{ "verdicts": [ { "key", "verdict": "accept" | "reject" | "revise", "reason", "instruction"? } ] }`, exactly one verdict per key given; a verdict for a key not given, a key twice, or `revise` without `instruction` fails the whole output. A check or judge that fails or times out, and a check that omits a verdict, accepts nothing it was given: those proposals are dropped, and their collectors stay unsettled (§4.2). A judge whose output is valid but omits verdicts is asked once more, in the same round, with only the proposals it left out (logged as entrypoint `reask`, §6.3; its cost counts toward the run and the cap, §9). That follow-up is a batch like any other: its `nodes` are those of the branches its own proposals land in, and the branch it shows a proposal under is the one §4.1 step 7 holds that judge's verdict to; the verdicts it gave stand, and a proposal still without a verdict after that call is dropped alone, its collector unsettled. The report names every key asked again and every key dropped, each as a JSON string so whitespace and commas in a key stay visible, and a follow-up the cap refused as refused (§4.4). A judge whose manifest names `ops` (§6.1) is not called for a round's batch that holds none of them: it makes no call and no cost, and no source is resolved for it (§6.4), accepts every proposal of that batch, and the report says it was skipped and why; a batch with one of its ops gets the whole batch. The `ops` filter applies only to that first batch: the follow-up holds every key the judge left out, whatever its op.

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

A hook gets `{ "event", "project", "run_id", "data" }`, `data` being the event's payload:

| Event | `data` |
| --- | --- |
| `run.before` | `{ "collectors": [the due collector ids], "force" }` |
| `collect.after` | `{ "collector", "round", "proposals" }`, per collector and round |
| `diff.after` | `{ "round", "diff" }` |
| `commit.before` | `{ "diff", "message" }`: the run's whole diff and the commit message |
| `commit.after` | `{ "commit", "message" }` |
| `view.render` | `{ "commit" }` (`null` when the run made none), before the views run |

A diff entry is `{ "change": "added" | "updated" | "moved" | "removed", "id", "parent", "fields"?, "by", "key" }`: `parent` is the parent after the change (before it, for a removal), `fields` the updated fields, `by` the collector instance and `key` its proposal key. A view gets `{ "project", "run_id", "params" }` on stdin. On a `*.before` event it prints `{}` to allow or `{ "deny": "<reason>" }` to deny; a non-zero exit, a timeout or any other output denies too, and the first deny aborts the run (exit 4) without running the hooks after it. On the other events its output is ignored and a failure is reported without changing the run.

### 6.7 Installing and trusting

- `rrecall plugin add <git-url|path> [--ref <ref>]` copies the plugin into the user directory, shows its manifest and the commands it will run, and records its source and content hash in the user config. `--ref` pins a tag or commit.
  - The source: an existing local directory (its path made absolute) is copied as it is, without its `.git`; anything else, and a local directory given with `--ref`, is cloned with git without a checkout, and the raw tree of `--ref` (else `HEAD`) is written from git's objects: no checkout, so no filter, hook or attribute conversion runs, and a submodule entry (a gitlink), a path with `.` or `..` parts, `.git`, or two paths that collide (also when they differ only in case) refuse the install. Git runs with only the `https`, `ssh`, `git` and `file` transports allowed, whatever the user's git config says, and with hooks off. The source's root holds the one plugin it installs. Its manifest is checked as the loader checks it (§6.1) before anything is shown.
  - The preview: the manifest as written, its `requires`, every argv by key (`command`, `fingerprint`, `fetch`, `resolve`) and every prompt with the runner its manifest names. Then it asks `Install <id> into <dir>? [y/N]` on stderr and reads one line from stdin; only `y` or `yes` (any case) installs. Any other answer, or none (stdin closed), installs nothing and exits 1. There is no flag that skips the question.
  - The plugin goes to `<user plugin directory>/<id>/`. An id whose directory or record already exists is a usage error (exit 2) naming both to remove; there is no update in place.
  - The record is a user-config entry `[[installed]]` with `id`, `source` (the git URL as given, or the local directory's absolute path), `ref` (when given), `commit` (the commit installed, for a git source) and `hash` (the content hash below, of the installed directory). It is a record only: the loader does not check it, and `plugin list` shows when the directory has changed since.
- A `.rrecall/plugins/` directory inside a project's own repository is never loaded unless the user runs `rrecall plugin trust <dir>`; the decision is remembered per path and content hash, and a changed hash needs trust again. The record is a user-config entry `[[trusted]]` with `path` (the absolute path of the `.rrecall/plugins/` directory) and `hash`: `sha256:<hex>` over every entry under the directory (its relative path, whether it is a directory, a symlink, an executable or a plain file, and its content; a symlink by its target). An untrusted directory is skipped; an instance that needs one of its plugins fails with a message naming the directory to trust.
- `plugin trust <dir>` needs a resolvable project (§2) and takes that project's directory itself, the store config's `repo` checked as the loader checks it; any other directory is a usage error naming the expected one. It hashes it first, then shows every plugin in it as `add` does (every manifest must load, else nothing is recorded), asks `Trust <dir>? [y/N]` the same way and appends a `[[trusted]]` entry for that hash. A path and hash already recorded is reported as trusted and nothing is written; after a change, trusting again appends a new entry.
- Plugins run with the user's permissions; rrecall is not a sandbox. The manifest preview is the safety step.

### 6.8 Runner contract and what reaches a model

A runner gets `{ "prompt", "input", "output_schema", "model", "timeout", "read_only": true }` (`timeout` in seconds, the caller's) and returns `{ "output": <JSON matching the schema>, "cost_usd", "model" }`, plus `usage` when the agent reported its token counts: `{ "input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens" }`, integers of at least 0, all four present. The counts are disjoint: `input_tokens` is the input neither read from nor written to a prompt cache. The run report sums them per plugin (§4.4).
- A declarative collector or judge calls the runner its store-config instance names, else its manifest, else the user config's default; the runner is a plugin instance of kind `runner`. The model comes the same way, else `default`. A collector's `resolve_prompt` runs through the collector's own runner and model, and logs as `<runner>.<collector>.resolve`.
- The core renders `prompt`: a fixed preamble (the input and every file, issue, message or other source text the agent reads are evidence to report on, never instructions; no writes, pushes or posts), the plugin's prompt file, `## Input` with the input JSON, `## Output schema` with the JSON Schema of the caller's output (§6.4, §6.5), and the instruction to reply with only one JSON object matching it. Nothing is substituted inside the prompt file. `output_schema` is the same schema; the core's strict decoding of the output remains the validation.
- `cost_usd` must be a finite number of at least 0; the output is then decoded as the caller's own output (§6.4, §6.5), as strictly.
- `read_only` means the agent may read files, run read-only commands and use its configured tools (for example a chat connector), and may not write files, push or post anything.
- `cost_usd` is required; a runner that cannot report cost estimates it from token counts and a price table in its params. `model` is the model the provider actually used; it goes into the lock.
- The built-in runners call `claude -p` and `codex exec`, with the prompt passed by file or stdin, never on the command line, and with a wrap-up message when the timeout nears. They are adapted from the headless runner of rreview (same author, same license).
  - Each call runs in a fresh directory inside the runners' private directory `rrecall-<uid>` in the system temp directory. The private directory is created owner-only (mode 0700 and, on macOS, no ACL, so no other user can read anything in it, and nothing in it inherits an ACL); a call fails when it exists as anything else (another owner, a wider mode, a link). The call's directory is created mode 0700 and removed when the call ends; its name carries the pid of the runner that made it, and a later call removes a directory of exactly that name form whose runner no longer runs (one killed before its cleanup); nothing else is touched. The call's directory is outside the map store, so the agent finds no map by a relative path, and outside any git repository, so no project's instruction files load (a temp directory inside a git repository fails the call). The agent program is checked like a plugin's (§6.3: a native executable or a `#!` script, never a shell). The agent inherits only what it needs to run and log in (`PATH`, `HOME`, locale, proxy and CA variables, and for Claude `CLAUDE_CONFIG_DIR` and `CLAUDE_CODE_OAUTH_TOKEN`), never the `RRECALL_*` variables.
  - `claude`: `claude -p --safe-mode --output-format stream-json --verbose --tools Read,Grep,Glob --allowedTools Read,Grep,Glob --permission-mode dontAsk --json-schema <output_schema> --session-id <uuid>`, plus `--model <model>` unless the model is `default`. With the param `effort` (one of `low`, `medium`, `high`, `xhigh`, `max`; default `""`, unset) it adds `--effort <effort>` after that, on the main and the wrap-up turn, and counts in its collectors' fingerprint and `setup` like the model (§4.1, §4.2); another value fails the load (exit 4) naming the instance. `--safe-mode` keeps the user's CLAUDE.md, skills, plugins, hooks and MCP servers out of the call. With the param `connectors = true` (default `false`), for an agent that reads chat through the user's claude.ai connectors (§12), `--safe-mode` is replaced by `--setting-sources user --disable-slash-commands --settings '{"disableAllHooks":true}'` with `CLAUDE_CONFIG_DIR` a fresh owner-only directory inside the call's directory (removed with it), and the call runs with `ENABLE_TOOL_SEARCH=true`, `MCP_CONNECTION_NONBLOCKING=false`, `CLAUDE_CODE_MCP_STARTUP_WAIT_MS=30000` and `CLAUDE_CODE_DISABLE_CLAUDE_MDS=1`. The login stays the user's and is never copied or linked: Claude Code reads it (the macOS keychain entry, else `.credentials.json`) from `CLAUDE_SECURESTORAGE_CONFIG_DIR`, set to the user's own config directory (`CLAUDE_CONFIG_DIR` made absolute against the directory rrecall was started in, empty for the default), and a token refresh updates it there, as in any session. The connectors load only from the user setting source, which is now the empty fresh directory, so the user's CLAUDE.md, settings, permission rules, hooks, local plugins and their MCP servers stay out; skills and slash commands stay off; the connectors connect before the first turn (the fresh directory has no discovery cache); tool search keeps their tool schemas out of the prompt until used (about 10k instead of 160k tokens per cold call in the §12 probes). Accepted risk: what the account syncs from claude.ai still loads (its connectors, the plugins it syncs, remote settings and policy). The tools allowed stay `Read,Grep,Glob`, plus `ToolSearch` (which loads the deferred tool schemas) and the param `tools` (default `[]`), a list of exact `mcp__<server>__<tool>` names added to `--allowedTools` (a wildcard, a server-wide name or anything else fails the call, and so does `tools` without `connectors`), so rrecall allows none of a connector's write tools unless named. Claude Code still applies the effective permission policy: a managed or organization allow rule that loads with the account can preapprove a tool in `dontAsk` mode, so the guarantee assumes that policy allows none of them. Tested: the argv rrecall passes (fake agent) and a live read through the named tools (§12); a denied call to an unnamed tool was not tested live. Not tested live: tool search on Sonnet or Opus. The output is the `result` event's `structured_output` (else its `result` text as JSON), the cost its `total_cost_usd` (else an estimate from its `usage`), the model the `modelUsage` entry with the most output tokens, the `usage` its `usage` (`cache_read_input_tokens` and `cache_creation_input_tokens` as the cache read and write). A killed turn adds its own `total_cost_usd` when it printed its `result`, else an estimate from the usage of the messages it streamed, and its tokens the same way: its `result`'s `usage`, else that of the messages it streamed; a record cut short by the kill is skipped, every complete one is decoded strictly; a request in flight at the kill is not counted.
  - `codex`: `codex exec --json -s read-only --skip-git-repo-check -C <dir> -o <dir>/reply.txt -`, plus `-m <model>` unless `default` (no `effort` param yet), with `CODEX_HOME` a fresh directory holding only a link to the user's `auth.json`, so no user config, MCP server, plugin or AGENTS.md reaches the call. No `--output-schema`: OpenAI's strict schemas cannot express optional fields or the resolve map; the schema is in the prompt. The output is the final message as JSON (a Markdown fence around it is allowed); the model and the session's token counts, a killed turn included, come from its session log (else the last `turn.completed` event, whose usage is the thread's running total, so a resumed turn's already holds the turns before it, and the requested model). The `usage` is those counts: Codex counts cache reads and writes inside `input_tokens`, so `cache_read_tokens` is `cached_input_tokens`, `cache_write_tokens` is `cache_write_input_tokens` (0 when a record has none) and `input_tokens` the rest.
  - An agent's JSON is read as strictly as plugin output (§6.3): a key twice fails the call. An agent that reports neither a cost nor its token counts fails the call; a missing cost is never taken as 0.
  - Wrap-up: the caller's timeout T splits into a main turn of T − w, w = min(T/5, 3 min). An agent still running at the end of the main turn is killed with its process group, and its session is resumed (`claude --resume <id>`, `codex exec ... resume <thread> -`) with a fixed "time is up, reply now with only the JSON object" message and a deadline of w − 2s, so the runner answers before the core kills it at T. A wrap-up turn that also runs out is killed and the call fails. A timeout of 10s or less leaves no wrap-up turn (w − 2s ≤ 0): the agent killed at T − w fails the call, and the error says so.
  - Params: `prices = { "<model>" = { input = N, cached_input = N, output = N } }`, USD per 1M tokens, default `{}`; cache writes are priced as input. It is used only when the agent reports no cost (Codex never does). A model missing from the table fails the call with an error naming the model and the param.
  - The model recorded is the one the agent reports; when it reports none, the requested model unless it is `default`, else the call fails.
- Sending source text and nodes to the runner's model provider is the same exposure as any coding-agent session on that machine; rrecall itself publishes and stores nothing off the machine (VISION principle 8). Each source can restrict which runners may receive its content (`allowed_runners` in the store config, §9); the core refuses to give that collector's nodes or resolved texts to any other runner, including judges.
  - At load (a config error, exit 4): an `allowed_runners` entry that names no enabled runner instance; a collector whose own runner (for its prompt or its `resolve_prompt`) its list leaves out; a declarative judge whose runner some collector's list leaves out, since a judge sees every collector's proposals and could never accept that collector's.
  - At each call: a declarative collector's input leaves out every node that cites a source of a kind owned by a collector whose list leaves out that collector's runner, and the `conflict_with` proposal of such a collector in its rejections. A check's or judge's verdict from a call whose batch held a proposal, a target node or a node in `nodes` with a left-out source reaches it with the reason replaced by a `withheld:` notice and no instruction, whichever proposal the verdict is about, since the check or judge saw that whole batch.
  - The boundary: `allowed_runners` governs what rrecall itself sends to a runner (its input, resolved texts and check or judge feedback). A read-only agent can still read files on the machine, the store included, which is the same exposure as any coding-agent session there.

## 7. Query output for agents

- Markdown, one node per block: `## <path> (<id>)`, the body, then `sources:` with the short references, then `date`, `state` and `note` when set. The path is the titles from the branch down, joined by ` > `. Children appear as titles with ids (`- <title> (<id>)`) unless `--depth` asks for more; a title line ends with `+N` when N descendants are not shown.
- `ls` prints titles and ids to depth 2 by default; for the reference map (about 300 nodes) that stays under about 1,500 tokens. Deeper levels are one `show <id>` away.
- `--max-tokens N` cuts the output at a node boundary and ends with how many nodes were left out and the command to see them. Tokens are estimated as bytes / 4, rounded up. The first node is always printed.
- `--json` prints the same nodes as JSON for scripts and plugins (§6.3): `show` and `find` print `{"nodes": [...]}`, each node with its stored fields plus `path` and `children` (the ids of its children); a `--max-tokens` cut adds `"left_out": [ids]`. `ls` prints `{"nodes": [{"id", "parent", "title"}]}` (`parent` absent for branches).

## 8. Views

The built-in `html` view writes one self-contained `views/index.html`: a collapsible mind map, the full outline with search, a node inspector with sources, the user's note and the node's history (from `git log` of the store), and a filter "changed since the last run". It works offline and loads nothing from the network. `rrecall open` renders and opens it (§2).

- A node's history is what `rrecall why <id>` lists, oldest first: each commit's date, short hash, subject and the body lines that name the node.
- "Changed since the last run" holds the nodes whose history has the newest commit whose subject starts with `run ` (§4.3), or a later one (a `note`, say). With no such commit the filter is empty and says there was no run yet.
- The page embeds the map as JSON data and draws it with inline script and style; for the same map and history it is byte for byte the same.

## 9. Configuration

Both configs reject unknown keys at every level.

- **User config** `$XDG_CONFIG_HOME/rrecall/config.toml`: projects as `[[projects]]` with `name`, an optional absolute `store` path and `remotes` (the URLs that map to it), the default `runner` (an enabled runner instance id, such as the built-in `claude` once `[[plugins]] id = "claude"` enables it; `init` writes `runner = "claude"` when there is none, §2) and `model`, installed plugins as `[[installed]]` (§6.7), and trusted plugin directories as `[[trusted]]` (§6.7).
- **Store config** `rrecall.toml`: the project's `repo` (§6.2); enabled plugins in order as `[[plugins]]` with `id`, `use`, `params`, `runner`, `model` (both only for a plugin with a prompt), `timeout` and `allowed_runners` (collectors only; runner instance ids, checked as in §6.8); skeleton additions (`[[skeleton.branch]]`); `max_rounds`; and a cost cap per run (`max_cost_usd`). The core checks the cap before starting each runner call; calls already in flight may finish, so a run can exceed the cap by at most the cost of the calls in flight. When the cap is reached, pending proposals are dropped with the reason "budget", their collectors stay unsettled, and the run exits 5.

## 10. Built-in plugins for the first release

| Kind | Id | Reads | How |
| --- | --- | --- | --- |
| collector | `files` | one file, or the files in a folder that match `include` and not `exclude` (globs: `*` and `?` within a path segment, `**` as a whole segment for any number of them; default `["**"]` and `[]`), text files only (no NUL byte in the first 8000 bytes of the raw content, git's rule with git attributes ignored; an empty file is text) | prompt + runner; a `fetch` gives the agent the folder, the matched files, how to cite them and the preset's branches; the agent reads the files with its own tools. With `git = true`: the files tracked at `HEAD` (so `.gitignore`d ones are out), regular files only; the fingerprint is a hash of the matched files' blob ids at `HEAD` (a commit that touches none of them does not trigger a run), references `<ref_kind>:<path>[#L<a>[-L<b>]]@<commit>` (12-character commit id), resolve reads the file at that commit; the cursor is the `HEAD` commit id, and with a cursor git knows the fetch adds `focus`: `{ "since": <12-character id>, "changed": [...], "deleted": [...] }`, the matched files whose blob differs from or is missing at that commit, and those matched there and not at `HEAD`; the full file list stays, so the agent can read other files for context (a cursor git does not know: a full read). Limit: an instance whose `path` names one file fails (its source unreachable, naming the missing path) once that file is deleted, so the user removes or repoints the instance. Without git: no cursor, so every run is a full read; hidden files and folders and symlinks are skipped, the fingerprint is a hash of the matched files' contents, references `<ref_kind>:<path>[#L<a>[-L<b>]]`, resolve reads the current file. `#L<a>[-L<b>]` is a line selector only at the end of a reference, so a path may contain `#`. Paths are relative to `path` (the folder, or the folder of the one file), `ref_kind` defaults to `file`, and resolve reads only what collection reads: a regular text file that matches the globs (with git, at the cited commit). `preset` picks the prompt's instructions and the branches the agent proposes into: `codebase` (docs, specs, manifests, layout → `layout`, `how-it-works`, `decisions`) or `notes` (informal notes that may be stale or contradict each other → `decisions`, `blocked`, `people`). The manifest's `scope` is the union of both, so the core enforces only that union; the preset's narrower set is told to the agent, not enforced. A typical project enables it twice, as `repo` and `notes` (with `ref_kind = "note"`, since two instances may not own one kind). |
| collector | `git-history` | merged branches, tags, release notes, in the repository at `path` | script, no model. `done` only: one node per tag on a commit of the main line, the first-parent history of `branch` (default `origin/HEAD`, else `HEAD`), so a tag on a merged side branch is no release; a release: title the tag (a tag longer than 80 characters: its first 70 characters, `…` and 8 hex characters of its hash), body its annotation (the release notes) or else the branches merged since the previous tag, date the tag's, state `done`. Merges after the newest tag wait for the next release. A repository with no tag gets one node per branch merged into the main line, a finished stream of work, titled `Merged <branch>`. A node is matched by its reference and its title, which differ for every tag and every stream, so two tags on one commit are two nodes. It proposes adds and, when the history reads otherwise, updates of its own nodes; never a removal. References `commit:<repo>@<sha>` (12-character commit id; `repo` defaults to `<owner>/<name>` of a GitHub `origin`, else the directory's name); resolve shows the commit and the tags on it. Its fingerprint covers the merges and the tags, so other commits do not trigger a run. It returns no cursor: every run reads the whole history (§4.5). |
| collector | `github` | PRs, issues, releases via `gh`, of `repo` (`<owner>/<name>`) | a `fetch` script lists the newest `limit` (default 100) of each with bodies cut to 2000 characters: pull requests and issues are the most recently updated, newest first, at most 1,000 of each whatever `limit` says (GitHub search's ceiling); releases newest first, paged past GitHub's 100 per page; no comments, reviews or discussions. Its cursor is `{ "pulls", "issues", "published" }`: the newest `updatedAt` of the pull requests fetched, the same of the issues, and the newest release `published_at` fetched, each moved only by its own list and kept from the previous cursor when nothing newer came; with it, each search adds `updated:>=<its time>` and releases keep only those published after `published`, so an edit to an older release's notes waits for a full read, and when more than `limit` items changed since the cursor the older ones are skipped, as a full read lists no more than `limit` either; prompt + runner proposes into `done`, `blocked`, `decisions`, `people`; resolve via `gh`. It owns `gh-pr`, `gh-issue` and `gh-release`. Its fingerprint is the most recently updated issue or PR and a hash of the releases fetch lists (an edited release note moves no date). Limit: an edit to that newest issue or PR within the same second as its `updated_at` leaves the fingerprint as it was, so the collector is not due for it; the inclusive search reads it only when the next run is due for another reason. A removal carries the evidence that ended the fact (the issue closed, the PR merged or closed). |
| collector | `slack` | named channels and DMs, through an agent that has Slack access | prompt + runner; `min_interval = "1d"`; resolve through the runner. Params `channels` and `dms` (names or ids; an instance with both empty is a config error, exit 4, so no paid call runs with nothing to read), `lookback_days` (default 14). A full read covers the last `lookback_days` days; with `since`, a run reads from one day before `since.at`, but never earlier than `lookback_days` days before the run, plus the threads with new replies in that window (§4.5). So a gap between runs longer than `lookback_days` loses what came before it, as a full read does; the stability rules absorb what a run saw before. Scope `decisions`, `people`, `blocked`; references `slack:<channel-id>/<ts>`. It names no runner: its instance sets one that reaches Slack, such as a `claude` instance with `connectors = true` and `tools` naming the connector's read tools (§6.8). |
| check | `no-op`, `size-limits`, `secrets` | the diff | code, run as `rrecall builtin <id>` (§6.2). `no-op`: §5 "No rewording". `size-limits`: rejects an `add` or `update` whose `title` or `body` is longer than its param `title_max` (default 80) or `body_max` (default 600), counted in characters as §3.1 counts them; the reason gives the length and the limit. `secrets`: rejects a proposal whose `key`, `title`, `body`, `sources`, `evidence` or `reason` holds something that looks like a credential (common token and key formats with a mixed-case-and-digit body, private key blocks, a full Slack webhook path, a password in a URL, a bearer token, a random-looking literal right after a secret-named label such as `DB_PASSWORD=` or `the deploy token is:`); the reason names the field and the pattern, never the matched text, and no error message quotes an input value. Its scan is linear in the input. |
| judge | `support`, `removal`, `duplicate` | the diff and the resolved sources | prompt + runner, prompts compiled into the binary; the manifests name no runner, so the user config's default (or the instance's `runner`) runs them. `support`: rejects an `add` or `update` its resolved sources do not back, an `update` that keeps the meaning and changes only the wording, and an `add_source` whose source does not back the node. `removal`: rejects a `remove` whose evidence does not show the fact ended (closed, deleted, reversed). `duplicate`: rejects an `add` that restates a node in `nodes`, naming it. Each accepts the ops it does not check, and declares the ops it checks (§6.1 `ops`): `support` `add`, `update`, `add_source`; `removal` `remove`; `duplicate` `add`; so a batch with none of them calls no model for it (§6.5). |
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

- **Headless access to chat.** Answered in #9 (claude 2.1.316, Haiku; the discovery probes read no message): `claude -p` sees the claude.ai Slack connector only when the user setting source loads. `--safe-mode`, `--strict-mcp-config`, `--restricted` and `--setting-sources project` drop it, and `--bare` cannot use the login. With the user source alone a cold call wrote about 160k tokens of connector tool schemas ($0.16 on Haiku); with tool search on, about 10.5k ($0.018); `disableAllHooks` stopped the user's hooks and kept the connector. A fresh `CLAUDE_CONFIG_DIR` keeps the login (through `CLAUDE_SECURESTORAGE_CONFIG_DIR`) and drops the user's CLAUDE.md, settings, permission rules, hooks and local plugins; its connectors stay `pending` through a one-word turn unless connected blocking (`MCP_CONNECTION_NONBLOCKING=false`), and then all load ($0.013, about 5s). Hence the `claude` runner's `connectors` and `tools` params (§6.8). Through that final launch the runner listed the user's channels and read 3 messages of one ($0.005 on Haiku). Not tested: tool search on Sonnet or Opus. A collector with its own Slack token and a script stays the alternative for users without the connector.
- **Growth of `done` and `decisions`.** Facts there stay true, so I2 never removes them, and the branches grow. The granularity rules (§3.4) slow that; if they are not enough, a `merge` operation (several nodes into one summary, keeping all sources) is the likely next step. Decide after M3 shows the growth rate.
- **Node size.** 600 characters for `body` is a guess from the reference map; adjust after M3's live run.
- **Judge cost.** Judging every proposal with a model may cost more than collecting. Judging only updates and removals, or sampling adds, are options after M3 measures it. Decided in #26: a judge's `ops` (§6.1) skip it for a batch that holds none of the ops it checks, so `removal` costs nothing on a batch without a `remove`.
- **Sharing code with rreview.** M3 copies and adapts its runner; a shared crate is a later issue if both keep changing it.

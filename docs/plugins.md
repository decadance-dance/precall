# Writing rrecall plugins

Read when: you want rrecall to read a source it does not know (another tracker, a chat, a folder of notes), to judge proposals by your own rules, to render the map another way, or to run another agent.

rrecall's core knows nodes, the map store and one loop; every source, check, judge, view and agent runner is a plugin, the built-in ones included (VISION, principle 6). A plugin is a directory with a manifest, `rrecall-plugin.toml`, and, depending on its kind, a prompt, a program in any language, or both. It reads one JSON document on stdin and writes one on stdout. The simplest plugin is a manifest and a prompt, with no code at all.

This guide is a walk through; [SPEC.md §6](SPEC.md#6-plugin-protocol) holds the full rules, and each section below links to the part it summarizes. Every JSON example here is checked against the code by `tests/plugin_guide.rs`, so what you see is what rrecall sends and accepts.

## Contents

- [Start from the example](#start-from-the-example)
- [Kinds, and declarative or command](#kinds-and-declarative-or-command)
- [The manifest](#the-manifest)
- [Instances](#instances)
- [Where plugins come from, and trust](#where-plugins-come-from-and-trust)
- [The process contract](#the-process-contract)
- [Collector I/O](#collector-io): fingerprint, fetch, collect, resolve
- [Check and judge I/O](#check-and-judge-io)
- [Runner I/O](#runner-io)
- [Hook I/O](#hook-io)
- [View I/O](#view-io)
- [What the core guarantees, and what a plugin cannot do](#what-the-core-guarantees-and-what-a-plugin-cannot-do)
- [Testing a plugin](#testing-a-plugin)

## Start from the example

[`examples/meeting-notes/`](../examples/meeting-notes/) is a complete collector for an invented source: a folder of meeting notes, one Markdown file per meeting, named `YYYY-MM-DD-<slug>.md`, with `## Decisions`, `## Blocked` and `## Owners` sections ([`examples/meeting-notes-data/`](../examples/meeting-notes-data/) has two). It is a manifest and two prompts, no code:

<!-- file: examples/meeting-notes/rrecall-plugin.toml -->
```toml
id = "meeting-notes"
name = "Meeting notes in a fixed format"
version = "0.1.0"
min_rrecall = "0.1.0"
kind = "collector"

[collector]
scope = ["decisions", "blocked", "people"]
source_kinds = ["meeting"]
min_interval = "1d"
prompt = "prompt.md"
resolve_prompt = "resolve.md"

[params]
path = ""
```

- `scope` names the branches it may propose into; `source_kinds` the references it owns (`meeting:<path>#L<line>`): only nodes citing a `meeting` source are its to change.
- A folder of notes has no cheap fingerprint without code, so `min_interval = "1d"` makes it due at most once a day instead.
- [`prompt.md`](../examples/meeting-notes/prompt.md) tells the agent what to read and how to propose; [`resolve.md`](../examples/meeting-notes/resolve.md) tells it how to fetch a cited line back for the judges.
- `path` is a param: every instance sets its own folder.
- After its first run it reads only the notes dated on or after its last settled run (`since`, [spec §4.5](SPEC.md#45-incremental-collection)); a note written up late under an older date waits for `rrecall run --force`.

Try it on a trial project. In a checkout of rrecall, install the plugin: `plugin add` shows the manifest, the prompts and the runner they use, and asks before it copies anything into your user plugin directory:

<!-- step: install -->
```sh
rrecall plugin add examples/meeting-notes    # in the checkout; shows the plugin, answer y
```

Then leave the checkout and create a map store for the trial project `lantern`. Run `init` outside any git repository: inside one, `init` also enables collectors for that repository and its GitHub `origin`, and the run would read them too. `init` prints where the store is (`~/.local/share/rrecall/lantern` by default):

<!-- step: init -->
```sh
cd ~                    # out of the checkout, outside any git repository
rrecall init lantern
```

Enable the plugin for the project: append an instance to the store's `rrecall.toml`, after the instances `init` wrote, with the absolute path of your notes folder (the example's `examples/meeting-notes-data/` will do):

<!-- step: enable -->
```toml
[[plugins]]
id = "meeting-notes"
params = { path = "/home/me/meetings" }
```

Then check and run it. Outside the project's repository, rrecall finds the project only by name, so name it once for the shell (or pass `--project lantern` to each command):

<!-- step: run -->
```sh
export RRECALL_PROJECT=lantern
rrecall doctor                      # every enabled instance can run: the runner is logged in
rrecall run                         # collects, judges, commits what was accepted
rrecall show decisions --depth 1    # the decisions found, each with its meeting:<path>#L<line> source
```

The prompt names no runner, so the user config's default runner (`claude` after `rrecall init`) runs it; an instance can name another with `runner = "..."`. `tests/plugin_guide.rs` runs these commands as written, with a fake `claude` first on `PATH`.

## Kinds, and declarative or command

| Kind | What it does | Declarative (a prompt) | Command (a program) |
| --- | --- | --- | --- |
| `collector` | reads a source, proposes changes to the map | yes | yes |
| `check` | accepts or rejects proposals by fixed rules, no model | no | yes |
| `judge` | accepts or rejects proposals with a model | yes | yes |
| `view` | renders the map for people or other tools | no | yes |
| `runner` | runs an agent for declarative plugins | no | yes |
| `hook` | observes the run, or denies it or its commit | no | yes |

A **declarative** plugin names a prompt file; a runner (an agent such as `claude -p` or `codex exec`) reads the prompt, the input JSON and the output schema and answers with the output JSON. You write no code, the agent reads the source with its own read-only tools, and the core decodes its answer as strictly as any program's ([spec §6.8](SPEC.md#68-runner-contract-and-what-reaches-a-model)). Choose it when an agent can reach the source (files on disk, a chat through a connector).

A **command** plugin names an argv; the core runs that program and talks to it over stdin and stdout. Choose it when the work is mechanical (a check, a view), cheap and exact (a script that lists the tracker's items), or needs an API the agent has no tool for.

The two mix: a declarative collector may name a `fetch` program that gives its agent the source data, a `fingerprint` program that says when the source changed, and a `resolve` program instead of a `resolve_prompt`. The built-in `github` collector is built that way.

## The manifest

Full rules: [spec §6.1](SPEC.md#61-unit-and-manifest). The short version:

- Top level: `id` (kebab-case), `name`, `version` and `min_rrecall` (`x.y.z`), `kind`, optional `requires` (binaries `rrecall doctor` checks on `PATH`), and `[params]` with each param's default. Unknown keys are an error at every level.
- Exactly one table, named by the kind. A collector has `scope` and `source_kinds`; `fingerprint` or `min_interval`; `command` or `prompt`; optionally `fetch` (with `prompt`), `resolve` or `resolve_prompt`, `runner` and `model` (with a prompt), and `timeout`.
- An argv is a list of strings, never a shell line. Its program is an absolute path, a path with a `/` relative to the plugin directory (`./collect.py`), or a bare name looked up on `PATH`. `{name}` in an argv entry or a `source_kinds` entry is replaced by the instance's param.
- Durations are `30s`, `10m`, `2h`, `1d`. `timeout` defaults to 10m (30s for a hook).

A command collector for an invented tracker, with a fingerprint, a resolve program and a param:

<!-- manifest: tickets -->
```toml
id = "tickets"
name = "Tickets from the team tracker"
version = "0.1.0"
min_rrecall = "0.1.0"
kind = "collector"
requires = ["python3"]

[collector]
scope = ["blocked", "done"]
source_kinds = ["ticket"]
fingerprint = ["./fingerprint.py", "{board}"]
command = ["./collect.py"]
resolve = ["./resolve.py"]
timeout = "5m"

[params]
board = ""
```

A judge needs only a prompt:

<!-- manifest: house-style -->
```toml
id = "house-style"
name = "Titles name the thing, not the meeting"
version = "0.1.0"
min_rrecall = "0.1.0"
kind = "judge"

[judge]
prompt = "judge.md"
```

A hook names its events:

<!-- manifest: release-freeze -->
```toml
id = "release-freeze"
name = "No commits to decisions during a release freeze"
version = "0.1.0"
min_rrecall = "0.1.0"
kind = "hook"

[hook]
events = ["commit.before"]
command = ["./freeze.sh"]
timeout = "10s"
```

## Instances

The store's `rrecall.toml` enables plugins as `[[plugins]]` entries, in run order ([spec §6.2](SPEC.md#62-discovery-and-order), [§9](SPEC.md#9-configuration)). Each entry is an **instance**: its `id` is what the lock, the run report, the commit message and `rrecall run --only` name. Without `use`, the instance id is the plugin id; with `use`, one plugin runs several times with its own params, as the built-in `files` collector does for a repository and a folder of notes:

<!-- config: instances -->
```toml
[[plugins]]
id = "repo"
use = "files"
params = { path = "/home/me/src/lantern", git = true, preset = "codebase" }

[[plugins]]
id = "notes"
use = "files"
params = { path = "/home/me/notes/lantern", preset = "notes", ref_kind = "note" }
```

Two instances may not own one source kind, so a plugin meant to run twice takes its kind from a param: `files` declares `source_kinds = ["{ref_kind}"]`. An instance may also set `runner`, `model` and `timeout`, and a collector `allowed_runners`: the runner instances that may receive its source text ([spec §6.8](SPEC.md#68-runner-contract-and-what-reaches-a-model)).

## Where plugins come from, and trust

Plugins are discovered in four places, a later one replacing an earlier one with the same id ([spec §6.2](SPEC.md#62-discovery-and-order)): built into the binary, the user plugin directory (`$XDG_CONFIG_HOME/rrecall/plugins/`, by default `~/.config/rrecall/plugins/`), a project repository's `.rrecall/plugins/`, and the map store's `plugins/`. Discovery only makes a plugin available; an instance in the store config enables it.

- `rrecall plugin add <git-url|path> [--ref <ref>]` installs into the user directory. It shows the manifest, every argv and every prompt with its runner, and installs only on a `y`; the source and content hash go into the user config ([spec §6.7](SPEC.md#67-installing-and-trusting)). There is no update in place: remove the directory and its `[[installed]]` record, then add again.
- A repository's `.rrecall/plugins/` is never loaded until you run `rrecall plugin trust <dir>`, and trust is per content hash: an edit needs trust again.
- `rrecall plugin list` shows every discovered plugin, where it came from, its content hash and the instances that use it.

Plugins run with your permissions. rrecall is not a sandbox; the preview before `add` and `trust` is the safety step.

## The process contract

Full rules: [spec §6.3](SPEC.md#63-process-contract).

- The argv runs in the plugin's directory, as its own process group, without a shell. A script needs a `#!` line naming an interpreter by absolute path (`#!/bin/sh`, `#!/usr/bin/env python3`). At the timeout the whole group is killed.
- stdin: one JSON document. stdout: one JSON document (a fingerprint prints raw text). stderr: your logs. A non-zero exit, a timeout or more than 64 MiB of stdout is a failure. After a non-zero exit the error quotes your last stderr line; after a timeout it names the stderr log; after an oversized stdout it gives the byte count and the cap (`printed 67108865 bytes, more than 67108864`), and the stderr log is in its usual place.
- Both streams are kept, with credentials redacted, in the store's `runs/<run-id>/<instance>.<entrypoint>.stdout` and `.stderr` (`.<round>` appended from round 2).
- Environment: rrecall's own, plus `RRECALL_BIN`, `RRECALL_PROJECT`, `RRECALL_STORE`, `RRECALL_PLUGIN_DIR`, `RRECALL_STATE_DIR` (a persistent directory of the instance's own, for caches), `RRECALL_RUN_ID`, and each param of the instance as `RRECALL_PARAM_<NAME>` (upper case, `-` as `_`; a string as it is, any other value as JSON). `RRECALL_PARAM_*` variables in rrecall's own environment are not passed on, so a plugin sees only its instance's params.
- To read the map, call the CLI: `"$RRECALL_BIN" show <id> --json`, `find`, `ls`. There is no other API, and no way to write the map but proposals.
- Your output is untrusted input to the core, and it is read strictly: one JSON document, no key twice at any level. The typed outputs (collect, resolve, check and judge, runner, hook) are also one object with no unknown fields and no `null` for an absent field (leave it out; only an unresolved text may be `null`). One item that does not decode rejects the whole output and names the plugin and the item. A fetch output is free-form, see below.

## Collector I/O

A run calls a collector's entrypoints in this order: `fingerprint` (every run, cheap), then, when it is due, `fetch` (once), `collect` (once per round) and, when judges need the cited text, `resolve`. Full rules: [spec §4.1](SPEC.md#41-steps), [§4.5](SPEC.md#45-incremental-collection), [§6.4](SPEC.md#64-collector-input-and-output).

### Fingerprint

Says whether the source changed since the last settled run. It gets the instance's identity and params, and prints anything cheap and deterministic as raw text (a commit id, the newest update time, a hash of a folder). The core hashes that output with the plugin's content hash, the params and the runner and model; when the hash is the lock's, the collector is skipped and no model runs (I1).

<!-- wire: fingerprint-input -->
```json
{
  "project": "lantern",
  "run_id": "2026-10-14T090000Z",
  "params": { "board": "LAN" }
}
```

A source without a cheap fingerprint declares `min_interval` instead, as the example does.

### Fetch

Optional, for a declarative collector: a program that reads the source for the agent, when the agent cannot (an API it has no tool for). It runs once per run and gets `since`, where the last settled run stopped (`null` for a full read: the first run, `--force`, or after the plugin, its params, runner or model changed):

<!-- wire: fetch-input -->
```json
{
  "project": "lantern",
  "run_id": "2026-10-14T090000Z",
  "params": { "board": "LAN" },
  "since": { "at": "2026-10-13T09:00:00Z", "cursor": "2026-10-13T08:55:02Z" }
}
```

Its output is any one JSON document (no key twice), passed to the agent as it is, as `fetched`; it need not be an object. When it is an object, a top-level `cursor` is where this read stopped: the core keeps it in the lock once the run settles and hands it back as `since.cursor` next time. A `cursor` that is `null` or absent means none, and a settled run without one clears the stored cursor. The cursor rules, for a fetch and for a command collector's collect output alike ([spec §4.5](SPEC.md#45-incremental-collection)):

- any JSON value, at most 4 KiB as JSON, whose numbers are integers that fit a signed or unsigned 64-bit integer, so it comes back exactly as given; a fraction, an exponent or a larger integer fails the output. An exact position beyond that goes in a string.
- only the fetch, or round 1's collect, counts: a cursor returned in round 2 or later is not kept.
- a cursor the `secrets` scanner flags as a credential is not kept, and the run report warns, since redacting it would corrupt it.

<!-- wire: fetch-output -->
```json
{
  "tickets": [
    { "id": "LAN-41", "title": "Renew the staging certificate", "state": "closed", "updated": "2026-10-13T16:20:00Z" }
  ],
  "cursor": "2026-10-13T16:20:00Z"
}
```

### Collect

The collector proper. A command collector gets this on stdin; a declarative one's agent gets it as the prompt's `## Input`. `scope` is the branches it fills with their question and granularity, `nodes` the current nodes there, `rejections` (from round 2 on) its own proposals that were sent back, each with the verdicts that rejected or revised it (the `core` for an invariant or a conflict), and `fetched` the fetch output when it has a `fetch`. Round 2 of the meeting-notes collector, after the `support` judge asked for a fix:

<!-- wire: collect-input -->
```json
{
  "project": "lantern",
  "run_id": "2026-10-14T090000Z",
  "round": 2,
  "scope": [
    {
      "id": "decisions",
      "title": "Decisions",
      "question": "Why was a decision made?",
      "granularity": "one node per decision, with its reason, date and source"
    }
  ],
  "nodes": [
    {
      "id": "ingest-queue-stays-on-sqlite",
      "parent": "decisions",
      "title": "Ingest queue stays on SQLite",
      "body": "The ingest queue stays on SQLite until it holds more than a million rows; Postgres would add an operator the team does not have.",
      "sources": ["meeting:/home/me/meetings/2026-10-06-weekly-sync.md#L5"],
      "date": "2026-10-06",
      "order": 0
    }
  ],
  "rejections": [
    {
      "proposal": {
        "key": "exporter",
        "op": "add",
        "parent": "decisions",
        "title": "Exporter ships as its own binary",
        "body": "The exporter ships as a separate binary to keep release builds fast.",
        "sources": ["meeting:/home/me/meetings/2026-10-06-weekly-sync.md#L6"],
        "date": "2026-10-06",
        "reason": "decided in the weekly sync"
      },
      "verdicts": [
        {
          "by": "support",
          "verdict": "revise",
          "reason": "the note does not say the builds are the reason",
          "instruction": "drop the reason the note does not state"
        }
      ]
    }
  ],
  "params": { "path": "/home/me/meetings" },
  "since": { "at": "2026-10-13T09:00:00Z", "cursor": null }
}
```

The output is proposals: `add`, `update`, `move`, `remove` or `add_source`, each with a `key` unique in the output, its `sources` and a one-line `reason` for the commit message. A new node's parent is an existing `id` (`parent`) or another proposal's `key` (`parent_key`); the core assigns ids. A `remove` needs `evidence`, the reference that shows the fact ended. Which fields each op takes and needs: [spec §6.4](SPEC.md#64-collector-input-and-output). A command collector may add a top-level `cursor` (a declarative one returns it from its `fetch`):

<!-- wire: collect-output -->
```json
{
  "proposals": [
    {
      "key": "exporter-stream",
      "op": "add",
      "parent": "done",
      "title": "Exporter streams changes",
      "body": "The nightly full export was replaced by an exporter that streams changes.",
      "sources": ["ticket:LAN-38"],
      "state": "done",
      "date": "2026-10-12",
      "reason": "LAN-38 closed"
    },
    {
      "key": "exporter-stream-docs",
      "op": "add",
      "parent_key": "exporter-stream",
      "title": "Streaming export documented",
      "body": "The operator guide describes the streaming exporter and its retry rules.",
      "sources": ["ticket:LAN-39"],
      "reason": "LAN-39 closed"
    },
    {
      "key": "certificate",
      "op": "remove",
      "id": "staging-certificate-renewal",
      "sources": [],
      "evidence": "ticket:LAN-41",
      "reason": "LAN-41 closed: the staging certificate was renewed"
    }
  ],
  "cursor": "2026-10-13T16:20:00Z"
}
```

In round 2 and later, return only proposals whose `key` is in `rejections`, fixed as the verdicts ask; a key sent back that you leave out is withdrawn, and a new key is ignored (the report says so). Accepted proposals stay accepted and are not shown to you again; their verdicts are in the run report ([spec §4.1](SPEC.md#41-steps), step 8).

Return `{"proposals": []}` when nothing changed. A node you did not see this time is not a removal; leave it out of your output and it stays. Keep the wording of facts that did not change: the `no-op` check and the `support` judge reject a reworded update.

### Resolve

Judges see the text each proposal cites, fetched back through the collector that owns the reference kind: its `resolve` program, or its `resolve_prompt` through its runner. The input is the references only, no params, so a reference must carry what it takes to find its text (the meeting-notes example puts the note's absolute path in it):

<!-- wire: resolve-input -->
```json
{
  "refs": [
    "meeting:/home/me/meetings/2026-10-06-weekly-sync.md#L6",
    "meeting:/home/me/meetings/2026-09-29-weekly-sync.md#L4"
  ]
}
```

Map every reference you can read to its text, and the others to `null` (or leave them out); a reference you were not asked for fails the output. A judge cannot accept a proposal whose every cited reference is unresolved.

<!-- wire: resolve-output -->
```json
{
  "texts": {
    "meeting:/home/me/meetings/2026-10-06-weekly-sync.md#L6": "- Ship the lantern exporter as a separate binary.",
    "meeting:/home/me/meetings/2026-09-29-weekly-sync.md#L4": null
  }
}
```

## Check and judge I/O

First the core validates each proposal against the invariants and rejects violations itself; the proposals that pass go to every enabled check, then what the checks accepted goes to every enabled judge ([spec §4.1](SPEC.md#41-steps), [§6.5](SPEC.md#65-check-and-judge-input-and-output)). Each proposal comes with its `key` (`<collector instance>/<proposal key>`) and the node it changes (`current`). `nodes` holds the current nodes of the branches the batch's adds and moves land in, so a judge can spot a duplicate, and `map_ids` every id in the map, branches included.

A check gets no resolved text: the proposal keeps its own `sources`, and the item has no `sources` of its own:

<!-- wire: check-input -->
```json
{
  "proposals": [
    {
      "key": "meeting-notes/queue",
      "proposal": {
        "key": "queue",
        "op": "update",
        "id": "ingest-queue-stays-on-sqlite",
        "body": "The ingest queue stays on SQLite until it holds more than a million rows.",
        "sources": ["meeting:/home/me/meetings/2026-10-06-weekly-sync.md#L5"],
        "reason": "shorter"
      },
      "current": {
        "id": "ingest-queue-stays-on-sqlite",
        "parent": "decisions",
        "title": "Ingest queue stays on SQLite",
        "body": "The ingest queue stays on SQLite until it holds more than a million rows; Postgres would add an operator the team does not have.",
        "sources": ["meeting:/home/me/meetings/2026-10-06-weekly-sync.md#L5"],
        "date": "2026-10-06",
        "order": 0
      }
    }
  ],
  "nodes": [],
  "map_ids": ["blocked", "decisions", "done", "how-it-works", "ingest-queue-stays-on-sqlite", "layout", "people"],
  "params": { "body_max": 600 }
}
```

A judge gets the same, plus `sources` per item: the resolved text of each reference the proposal cites, `null` where it did not resolve:

<!-- wire: judge-input -->
```json
{
  "proposals": [
    {
      "key": "meeting-notes/exporter",
      "proposal": {
        "key": "exporter",
        "op": "add",
        "parent": "decisions",
        "title": "Exporter ships as its own binary",
        "body": "The weekly sync decided to ship the exporter as a separate binary.",
        "sources": ["meeting:/home/me/meetings/2026-10-06-weekly-sync.md#L6"],
        "date": "2026-10-06",
        "reason": "decided in the weekly sync"
      },
      "sources": {
        "meeting:/home/me/meetings/2026-10-06-weekly-sync.md#L6": "- Ship the lantern exporter as a separate binary."
      }
    },
    {
      "key": "meeting-notes/queue",
      "proposal": {
        "key": "queue",
        "op": "update",
        "id": "ingest-queue-stays-on-sqlite",
        "body": "The ingest queue moves to Postgres.",
        "sources": ["meeting:/home/me/meetings/2026-10-13-weekly-sync.md#L5"],
        "reason": "reversed in the weekly sync"
      },
      "current": {
        "id": "ingest-queue-stays-on-sqlite",
        "parent": "decisions",
        "title": "Ingest queue stays on SQLite",
        "body": "The ingest queue stays on SQLite until it holds more than a million rows; Postgres would add an operator the team does not have.",
        "sources": ["meeting:/home/me/meetings/2026-10-06-weekly-sync.md#L5"],
        "date": "2026-10-06",
        "order": 0
      },
      "sources": {
        "meeting:/home/me/meetings/2026-10-13-weekly-sync.md#L5": "- Drop the nightly full export; the exporter streams changes instead."
      }
    }
  ],
  "nodes": [
    {
      "id": "ingest-queue-stays-on-sqlite",
      "parent": "decisions",
      "title": "Ingest queue stays on SQLite",
      "body": "The ingest queue stays on SQLite until it holds more than a million rows; Postgres would add an operator the team does not have.",
      "sources": ["meeting:/home/me/meetings/2026-10-06-weekly-sync.md#L5"],
      "date": "2026-10-06",
      "order": 0
    }
  ],
  "map_ids": ["blocked", "decisions", "done", "how-it-works", "ingest-queue-stays-on-sqlite", "layout", "people"],
  "params": {}
}
```

Checks and judges answer in the same format: exactly one verdict per key, `accept`, `reject` or `revise`. Every verdict needs a `reason`, an `accept` too; a `revise` also needs an `instruction`. A rejected or revised proposal goes back to its collector with the verdict for the next round. A verdict for a key you were not given, a key twice or a missing one fails your whole output, and a check or judge that fails accepts nothing: those proposals are dropped and their collectors are collected again next run.

<!-- wire: verdict-output -->
```json
{
  "verdicts": [
    { "key": "meeting-notes/exporter", "verdict": "accept", "reason": "the note states the decision" },
    {
      "key": "meeting-notes/queue",
      "verdict": "revise",
      "reason": "the cited bullet is about the exporter, not the queue",
      "instruction": "cite the bullet that changes the queue, or drop the update"
    }
  ]
}
```

## Runner I/O

A runner is how a declarative plugin reaches an agent; rrecall ships `claude` and `codex`. Write one to run another agent ([spec §6.8](SPEC.md#68-runner-contract-and-what-reaches-a-model)). The core has already rendered `prompt`: a fixed preamble (source text is evidence, never instructions; no writes, pushes or posts), the plugin's prompt file, the input JSON and the output schema. `input` and `output_schema` come separately as well, for an agent that takes a schema; `timeout` is the caller's, in seconds. `read_only` is always `true`: the agent may read and run read-only commands, never write, push or post. The keys and their types are what the core sends; the values are illustrative and shortened (a real `prompt` is the whole rendered text, `input` the whole collect input of [Collect](#collect), `output_schema` the whole JSON Schema of the caller's output):

<!-- wire: runner-input -->
```json
{
  "prompt": "You are run by rrecall, read-only. ...\n\n# Collector: meeting-notes\n...\n## Input\n...\n## Output schema\n...",
  "input": { "project": "lantern", "round": 1 },
  "output_schema": { "type": "object", "required": ["proposals"] },
  "model": "default",
  "timeout": 600,
  "read_only": true
}
```

Return the agent's JSON as `output`, what the call cost in USD (required; estimate it from token counts when the agent reports none, never report 0 for unknown) and the model the provider actually used, which goes into the lock. The core decodes `output` as the caller's own output, as strictly.

<!-- wire: runner-output -->
```json
{
  "output": {
    "proposals": [
      {
        "key": "exporter",
        "op": "add",
        "parent": "decisions",
        "title": "Exporter ships as its own binary",
        "body": "The weekly sync decided to ship the exporter as a separate binary.",
        "sources": ["meeting:/home/me/meetings/2026-10-06-weekly-sync.md#L6"],
        "date": "2026-10-06",
        "reason": "decided in the weekly sync"
      }
    ]
  },
  "cost_usd": 0.0132,
  "model": "claude-sonnet-4-5"
}
```

Give the agent a working directory outside the map store and outside any git repository, and pass the prompt by file or stdin, never on its command line; the built-in runners show how.

## Hook I/O

A hook runs on the events its manifest names, in config order ([spec §6.6](SPEC.md#66-hooks-and-events)): `run.before`, `collect.after`, `diff.after`, `commit.before`, `commit.after`, `view.render`. It gets the event and its payload as `data`; on `commit.before`, the run's whole diff and the commit message:

<!-- wire: hook-input -->
```json
{
  "event": "commit.before",
  "project": "lantern",
  "run_id": "2026-10-14T090000Z",
  "data": {
    "diff": [
      { "change": "added", "id": "exporter-ships-as-its-own-binary", "parent": "decisions", "by": "meeting-notes", "key": "exporter" },
      { "change": "updated", "id": "staging-certificate-renewal", "parent": "blocked", "fields": ["body", "state"], "by": "tickets", "key": "certificate" }
    ],
    "message": "run 2026-10-14T09:00Z: +1 ~1 -0\n\nadded   decisions/exporter-ships-as-its-own-binary  by meeting-notes  decided in the weekly sync [meeting:/home/me/meetings/2026-10-06-weekly-sync.md#L6]\nupdated blocked/staging-certificate-renewal (body, state)  by tickets  LAN-41 closed [ticket:LAN-41]"
  }
}
```

Only `run.before` and `commit.before` can deny, and there a hook fails closed: print `{}` to allow; `{"deny": "<reason>"}`, a non-zero exit, a timeout or any other output stops the run before its commit (exit 4). The tracked map and lock stay as they were; the run report and the hook's logs under `runs/<run-id>/` are still written. On the other events the output is ignored and a failure is only reported.

<!-- wire: hook-output -->
```json
{ "deny": "release freeze until 2026-10-20: no changes to decisions" }
```

## View I/O

A view runs after the commit and renders the map for people or tools ([spec §8](SPEC.md#8-views)). It gets the instance's identity and params, reads the map through `"$RRECALL_BIN" ls --json` and `show --json`, and writes its files under `$RRECALL_STORE/views/`. Its stdout is ignored; a failure is reported and does not change the run's exit code.

<!-- wire: view-input -->
```json
{
  "project": "lantern",
  "run_id": "2026-10-14T090000Z",
  "params": { "title": "Lantern map" }
}
```

## What the core guarantees, and what a plugin cannot do

Three layers decide what reaches the map, and they promise different things.

**The core, always.** The stability rules are core code; no plugin and no config turns them off ([spec §5](SPEC.md#5-stability-rules)). Whatever your collector returns:

- **It cannot remove what it left out.** A node missing from your output stays. A `remove` without `evidence`, or with evidence that is not a well-formed reference of a kind an enabled collector owns, is rejected (I2). The core checks that evidence is there and well-formed, not that it shows the fact ended: that is the `removal` judge's job, below.
- **It cannot lose the user's work.** A node with a user `note`, or with children not moved or removed in the same run, is never removed by a run (I3). The proposal schema has no `note` field (I7).
- **It cannot choose or change ids.** The core assigns them on `add`; you refer to existing nodes by id (I4).
- **It cannot change the skeleton.** Branches come only from the config and `rrecall import` (I5); your `scope` must name existing branches.
- **It cannot touch nodes it does not own.** You may update, move or remove only nodes that cite a source of one of your `source_kinds`; on any other node only `add_source`. An `update` replaces only your own sources (I8).
- **It cannot win a conflict by order.** Two proposals on one node in one round both go back with the other's proposal (I9).
- **Its failures are not forgotten.** A collector that fails, or whose proposals a failing check or judge dropped, stays unsettled and is collected again (I10).
- **It cannot write the map directly.** Proposals are the only way in. Every node names its source.
- **It does not decide what rrecall sends to a model.** `allowed_runners` on a collector keeps its nodes, resolved texts and the feedback about them out of every other runner's input, judges included ([spec §6.8](SPEC.md#68-runner-contract-and-what-reaches-a-model)).

**Checks and judges, when enabled.** They are plugins the store config lists, so what they enforce holds only while they are enabled. `init` enables the built-in checks `no-op` (no rewording), `size-limits` and `secrets`, and the judges `support` (the cited text backs the claim, and an update changes meaning, not wording), `removal` (the evidence shows the fact ended: closed, deleted, reversed) and `duplicate` (an add does not restate a node) ([spec §10](SPEC.md#10-built-in-plugins-for-the-first-release)). A proposal is accepted only when every enabled check and judge accepts it.

**The machine, never.** rrecall is not a sandbox. A plugin runs with the user's permissions and can read and write what they can; that is why `plugin add` and `plugin trust` show everything first. A runner's agent is read-only, but its read tools can still open any file the user can, the map store included: `allowed_runners` filters what rrecall puts in a runner's input, not what that agent reads on its own. Keep source material you do not want an agent to read out of its reach, not just out of its input.

## Testing a plugin

- `rrecall doctor` checks every enabled instance: binaries on `PATH`, programs, the runner's login, and a collector's access by running its fingerprint.
- `rrecall run --only <instance>` runs one collector; `--force` reads everything again, with `since` `null`.
- `runs/<run-id>/report.md` lists every proposal with its verdicts per round; `runs/<run-id>/` also keeps each entrypoint's stdout and stderr.
- A program is a JSON filter: feed it an input from this guide and read what it prints.
- For an end-to-end test without a model, put a fake `claude` first on `PATH` that prints canned output, as `tests/plugin_guide.rs` does with `tests/fixtures/bin/claude`.

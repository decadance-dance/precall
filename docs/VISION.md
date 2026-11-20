# precall: vision

## What it is

precall keeps a map of a software project: what lives where, how it works, who owns what, what was done, what is blocked, and why decisions were made. It builds the map from the project's own sources (the repository, its tracker, its chat, the maintainer's notes, past agent sessions) and keeps it as a versioned document in its own git repository. People read the map as an HTML page. Coding agents query it from the command line, one branch at a time.

## Why it exists

A coding agent starts every session knowing nothing about the project beyond the files it reads. The knowledge it needs is spread across places it rarely looks: the reason for a design sits in a chat thread, the owner of a component is a name in the tracker, the blocker on a task is a sentence in someone's notes. Instruction files such as `AGENTS.md` carry the rules but not the state of the project, and they go stale.

One afternoon of work showed the value: four agents swept a real multi-source work project (code and specs, tracker, GitHub, chat, notes, 400+ past sessions) and produced a mind map of about 300 nodes that a person could read in minutes and an agent could navigate. It also showed the problem. The map was a one-off. Running the same sweep again would produce a different document: other wording, other grouping, items silently missing. Nobody can review a document that changes everywhere on every run, and nobody can trust one that drops facts that are still true.

precall makes that sweep repeatable and its output stable, reviewable and queryable.

## Who it serves

- **The maintainer** of one or more projects who works with coding agents. They want an overview they can trust, and a weekly diff that reads like a changelog of the project.
- **Coding agents** in the middle of a task. They ask seven kinds of questions: where something lives, how something works, who is responsible for what, what was done, what was worked on lately, what is blocked, and why a decision was made.
- **Other people with other setups.** One person has GitHub issues and Slack, another has Jira and Notion, a third has only a repository and notes. Each assembles the map from their own sources and wants their own summaries.

## Principles

1. **The map is a living, versioned document, not a report.** The first run creates it along a fixed skeleton. Every later run changes it in place, and every run that changes it is a git commit, so `git diff` shows what changed in the project, `git blame` shows when and why a line appeared, and a person can annotate any node.
2. **Stability is the product.** A run over unchanged sources leaves the map byte for byte as it was, because nothing is collected again. When sources do change, the map changes only where they did. A node disappears only with evidence that it is no longer true (the task was closed, the file was deleted, the decision was reversed). "Not seen this time" is never evidence. Rewording a node without a change in meaning is a defect, not an improvement.
3. **Every node names its source.** A claim without a source does not enter the map.
4. **Back pressure, not trust.** Collectors propose changes; checks and judges accept or reject each one; a rejected change goes back to its collector with the reason, for a bounded number of rounds; what still fails is reported, not committed.
5. **Fail closed.** A check that crashes or times out rejects. A source that cannot be reached keeps its nodes untouched and is reported as unreachable.
6. **A small core; everything else is a plugin.** In the spirit of Pi: the core knows nodes, the store, collectors, checks and views, and runs one loop. Every source, check, judge, view and agent runner, including the ones that ship by default, is a plugin a user can replace or drop. In the spirit of herdr: a plugin is a program in any language that reads JSON and writes JSON, and the precall CLI is its API. A simple plugin is a manifest and a prompt, with no code. One exception, on purpose: the stability rules (no removal without evidence, stable ids, the user's notes, no silent conflicts) are core code, so no configuration can lose what principle 2 promises. Plugins add judgement on top; they cannot take those rules away.
7. **Agent-agnostic.** Claude Code, Codex, Pi or any other agent can run a collector or a judge; the core does not depend on one.
8. **Local and private.** A map holds what its sources hold: chat messages, names, internal decisions. precall publishes and stores nothing off the machine: no upload, no default remote, no telemetry. Collectors and judges do send source text to the model provider of the agent that runs them, the same exposure as any coding-agent session on that machine; the user can limit, per source, which agents may receive its content.
9. **Cheap when nothing changed.** A source whose fingerprint did not change is not collected again, so a run over a quiet week costs almost nothing.

## Non-goals

- **Not a code index.** Symbol search, call graphs and "find the function" belong to LSP servers and code-search tools. The map says which part of the project does what and where it lives, at the level a newcomer needs.
- **Not a live dashboard.** The map records state as of the last run and says where the live state is. An agent that needs the current status of a task asks the tracker.
- **Not a wiki to write in.** People annotate nodes; they do not author the map by hand. (Seeding a new map from an existing outline is allowed.)
- **Not a hosted service** and not a shared team server. One person runs it on their machine for their projects.
- **Not a cross-project knowledge base.** One map per project.

## What would make it a failure even if it works

- A weekly run produces a diff no one can read, because wording and order moved.
- An item that is still true disappears, and nobody notices.
- precall puts private content anywhere other than the user's machine and the model providers the user chose.
- The core grows features for one person's setup instead of growing plugins.
- An agent has to load the whole map to answer one question.

## What success looks like

- Two consecutive runs over unchanged sources: an empty diff and no model calls. Even when forced to collect everything again, at most a few percent of nodes change.
- A weekly run: a diff of a few dozen lines that the maintainer reads in two minutes and recognizes as what happened that week.
- An agent answers "where does X live", "who owns Y", "why did we choose Z" with one `precall` call of a few hundred tokens.
- Someone with a different tracker and chat writes one plugin, a manifest and a prompt, and gets their map without touching the core.

## Prior art it borrows from

- **DVC**: data versioned next to git; a pipeline of stages whose inputs are fingerprinted in a lock file, so an unchanged stage does not run again.
- **Pi coding agent**: a minimal core; extensions for everything else; per-event rules for what a handler may do; skills that are just a Markdown file.
- **herdr**: a plugin is a directory with a manifest and any executable; the tool's own CLI is the plugin API; context passed as JSON.
- **Orca plugins**: a strict manifest where an unknown key is an error; hooks that can pass, change or deny; an explicit choice of fail-open or fail-closed per hook; plugin versions pinned by content hash in a lock file.
- **Code wikis (DeepWiki, Code Wiki) and NotebookLM mind maps**: they show the value of a generated overview, but they see only code or only uploaded files, and they regenerate rather than evolve.

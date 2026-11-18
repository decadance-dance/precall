# rrecall

`rrecall` keeps a versioned map of a software project and answers questions about it: what lives where, how it works, who owns what, what was done lately, what is blocked and why decisions were made.

A run sends collectors over the project's sources (the repo, its tracker, its chat, your notes), merges what they find into the current map, and lets checks and judges send rejected changes back for rework before anything is committed. The map lives in its own git repository, so every change shows in `git diff` and `git blame`. People read it as an HTML page; coding agents query one branch at a time (`rrecall show decisions`).

Status: design. The product and the plan: [docs/VISION.md](docs/VISION.md) and [docs/SPEC.md](docs/SPEC.md). Contributing: [AGENTS.md](AGENTS.md).

License: [Apache-2.0](LICENSE).

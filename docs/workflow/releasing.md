# Releases and hotfixes
Read when: cutting a release, shipping a hotfix, bumping the version, or tagging `main`.
TL;DR: A `release-<ver>` branch comes off `develop`, a `hotfix-<ver>` branch off `main`. Each gets its own release issue, a PR into `main`, a plain `X.Y.Z` annotated tag on `main` whose message is the release notes and whose push publishes the GitHub Release with the binaries and the Homebrew formula, and a PR back into `develop`. Every release and hotfix bumps `version` in `Cargo.toml` (and so `Cargo.lock`). Branch basics and the task-branch rules: [git-flow.md](git-flow.md). A closed issue stays closed after a release: new work gets a new issue.

## Release branch (`release-*`)
### Do
- Cut it when `develop` has collected enough, usually a finished milestone: `git checkout -b release-1.2.0 origin/develop`.
- Limit it to small bug fixes, the version bump and the release notes. New work keeps going to `develop` in parallel.
- Give the release its own issue (`Release 1.2.0`). Its checklist holds only what is done before the PR into `main` merges: version bumped in `Cargo.toml` and `Cargo.lock`; shipped issues marked; notes ready; `ci-ok` green and the cross-model review done on the draft PR. The steps after the merge (tag, GitHub Release, Homebrew tap, PR into `develop`, deleting the branch) go in the issue as notes.
- Bump the version in its own commit: edit `version` in `Cargo.toml`, then run `bin/check`, which rewrites `Cargo.lock` with the same number; commit both.
- List what ships by the branch's contents, not by dates: work merged into `develop` after the cut does not ship. The PRs are the merge commits on the branch since the last tag (for the first release, all of them: drop `<last tag>..`):
  ```
  git log --first-parent --merges --format=%H <last tag>..release-1.2.0 |
    while read c; do gh api repos/decadance-dance/rrecall/commits/$c/pulls --jq '.[] | "#\(.number) \(.title) @\(.user.login)"'; done
  ```
  The shipped issues are the ones those PRs closed (`gh pr view <pr> -R decadance-dance/rrecall --json closingIssuesReferences --jq '.closingIssuesReferences[].number'`). This PR list and these issues are the notes' input.
- Mark the shipped issues: each of them, and the release issue itself, get the milestone `<ver>` (`gh issue edit <n> --milestone 1.2.0`; create it once with `gh api repos/decadance-dance/rrecall/milestones -f title=1.2.0`).
- Draft the release notes: write the whole body into a local file outside the repo, `RELEASE-<ver>.md`, by the template in [release-notes.md](release-notes.md), and keep its path in `NOTES` (`NOTES=<dir outside the repo>/RELEASE-1.2.0.md`). The maintainer edits it. Never commit it.
- Open a draft PR into `main` (`gh pr create --draft --base main --title "Release 1.2.0" --body "Refs #<release issue>"`). When `ci-ok` is green and the cross-model review is done, tick the last item and mark it ready (`gh pr ready`).
- After that PR merges, the maintainer tags `main` and pushes the tag; the push publishes the GitHub Release (`.github/workflows/release.yml`: title = the version, body = the tag message body as written; `--cleanup=verbatim` keeps its `##` headings):
  ```
  git checkout main && git pull
  git tag -a 1.2.0 --cleanup=verbatim -F "$NOTES"
  git push origin 1.2.0
  ```
  The workflow builds the binaries first (macOS arm64 and x86_64, Linux x86_64 and arm64), then publishes the Release with `rrecall-<ver>-<target>.tar.gz` for each, `SHA256SUMS`, and `rrecall.rb`: the Homebrew formula for this version, written by `bin/formula` from those checksums. Check the published Release (`gh release view 1.2.0`); a failed workflow run (a failed build, a lightweight tag, a tag that differs from `version` in `Cargo.toml`) leaves none.
- Then the maintainer updates the Homebrew tap, the repository `decadance-dance/homebrew-tap` (`brew install decadance-dance/tap/rrecall`): its `Formula/rrecall.rb` becomes the Release's `rrecall.rb`, which carries the new version, the URLs and the sha256 of each binary.
  ```
  cd <tap checkout> && git pull
  gh release download 1.2.0 -R decadance-dance/rrecall -p rrecall.rb -O Formula/rrecall.rb --clobber
  git commit -am "rrecall 1.2.0" && git push
  brew update && brew upgrade rrecall && brew test rrecall   # brew install decadance-dance/tap/rrecall the first time
  ```
- Then open a PR from the same branch into `develop` (`gh pr create --base develop --head release-1.2.0 --title "Release 1.2.0 into develop" --body "Closes #<release issue>"`). If it conflicts, branch `merge/1.2.0` off `develop`, merge `release-1.2.0` into it, resolve, and PR `merge/1.2.0` into `develop` with the same title.
- Delete the release branch after both PRs have merged.

### Don't
- Don't add new features in a release branch.
- Don't skip the PR back into `develop`, or the fixes made during the release are lost in the next one.
- Don't tag with a `v` prefix, a suffix (`1.2.0-rc.1`) or a lightweight tag: plain annotated `X.Y.Z` tags only. The release workflow fails on a lightweight tag.
- Don't publish the GitHub Release by hand: the tag push does it, and a hand-made one makes the workflow fail.

## Hotfix branch (`hotfix-*`)
### Do
- Use it only when something broke for users and the next release cannot wait. It is the only branch that comes off `main`: `git checkout -b hotfix-1.2.1 origin/main`.
- Give it its own issue (`Hotfix 1.2.1`) with the release issue's checklist: version bumped in `Cargo.toml` and `Cargo.lock`; the hotfix issue marked with milestone `1.2.1`; notes ready; `ci-ok` green and the cross-model review done on the draft PR into `main`. The post-merge steps go in the issue as notes. Mark only the hotfix issue with the milestone: the closed issues waiting on `develop` ship with the next release. The notes' input is the hotfix issue and the hotfix PR into `main`, not `develop`'s history.
- Finish it like a release: version bump, a PR into `main`, the tag and the GitHub Release on `main`, then a PR from the same branch into `develop`.

### Don't
- Don't use a hotfix for planned work or features.
- Don't forget the PR into `develop`.

## Release numbers
### Do
- Use `MAJOR.MINOR.PATCH` (for example `1.4.2`):
  - patch +0.0.1 when a `hotfix-*` ships, or a `release-*` ships without new features (only fixes or docs);
  - minor +0.1.0 when a `release-*` ships with features;
  - major +1.0.0 when a release breaks users: a CLI command or exit code that no longer works as before, a map that the new version cannot read without a rebuild, or a plugin protocol change that breaks existing plugins.
- Stay on `0.x` until rrecall is a map people rely on; `1.0.0` is that release. While on `0.x`, a breaking change bumps the minor, as Cargo reads `0.x` versions.
- The first release is `0.1.0` (#15).
- Keep the tag and `version` in `Cargo.toml` equal: the release workflow fails when they differ.

## Cheat sheet
```
# release a version
git checkout -b release-1.2.0 origin/develop
# release issue #<r>; milestone 1.2.0 on it and on every issue closed by a PR merged on the branch since the last tag
# version bump in Cargo.toml + bin/check (Cargo.lock); NOTES=<outside the repo>/RELEASE-1.2.0.md drafted and edited (never committed)
git push -u origin release-1.2.0 && gh pr create --draft --base main --title "Release 1.2.0" --body "Refs #<r>"
# ci-ok green, cross-model review done: tick the checklist, then gh pr ready
# maintainer:
gh pr merge <pr> -R decadance-dance/rrecall --merge
git checkout main && git pull && git tag -a 1.2.0 --cleanup=verbatim -F "$NOTES" && git push origin 1.2.0   # the push builds the binaries and publishes the Release
# in the tap checkout: gh release download 1.2.0 -R decadance-dance/rrecall -p rrecall.rb -O Formula/rrecall.rb --clobber, commit, push
gh pr create --base develop --head release-1.2.0 --title "Release 1.2.0 into develop" --body "Closes #<r>"
gh pr merge <develop pr> -R decadance-dance/rrecall --merge
git push origin --delete release-1.2.0 && git branch -d release-1.2.0

# urgently fix a release
git checkout -b hotfix-1.2.1 origin/main
# hotfix issue, milestone 1.2.1 alone (develop's closed issues wait for the next release); notes from the hotfix PR into main
# same finish as a release: version bump, PR into main, tag and GitHub Release on main, PR into develop, delete the branch
```

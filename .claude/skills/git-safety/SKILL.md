---
name: git-safety
description: What each git operation in this repo destroys or publishes, and the safe way to do the things that tempt you into the destructive ones — undoing an experiment, moving a file, setting work aside. Covers why `git checkout -- <file>` has no undo, what is untracked here and why that makes `git clean` and `git stash` dangerous, and the fact that pushing to main publishes signed packages to a live apt repository. Use BEFORE any git command that discards, moves, resets, stashes, cleans, commits or pushes.
---

# git in this repo: what each command costs

## The only line that matters

**Committed work is recoverable. Uncommitted work is not.**

`git reflog` finds any commit that ever existed, including ones on branches you
deleted and rebases you regret. It records nothing about the working tree. A
file's uncommitted edits exist in exactly one place, and every command below
overwrites that place with no copy kept and no confirmation asked.

So the question before any git command is never "is this command dangerous" —
it is **"what in the working tree is not yet in a commit"**:

```sh
git status --porcelain          # ' M' = edited, '??' = untracked, 'A '/'R ' = staged
git log --oneline -3            # the operator commits as work lands; check what is already safe
```

That second line is not optional here. Work gets committed between turns, so
"my changes are uncommitted" can be stale by the time you act on it.

## Commands that destroy, and what to do instead

| Command | Destroys | Instead |
|---|---|---|
| `git checkout -- <file>`, `git restore <file>` | every uncommitted edit to that file | copy to the scratchpad first, or reverse the edit with Edit |
| `git reset --hard` | every uncommitted edit, everywhere | `git stash -u`, or commit to a scratch branch |
| `git clean -fd` | every untracked file | look at `git status --porcelain \| grep '??'` first |
| `git stash` (no `-u`) | nothing — but **leaves untracked files behind**, so it does not mean "set my work aside" | `git stash -u` |
| `git checkout <branch>` | nothing directly, but carries or blocks uncommitted work | commit or stash first, deliberately |

### `git checkout -- <file>` is the one that actually bites

It reads like "undo my last experiment" and means "discard everything in this
file since the last commit". Those are the same thing only when the experiment
is the sole change in the file, which is rarely true mid-task: a file being
experimented on is usually a file being worked on.

This has cost real work in this repo twice, in one session, both times while
verifying that a test fails without its fix — `cli.rs` lost a new subcommand and
its privilege-table arm, `properties.rs` lost a managed-key expansion and two
tests. In both cases the *experiment* was correctly reverted and everything else
in the file went with it.

### The safe way to prove a test is not vacuous

Verifying that a guard actually guards means breaking it on purpose and watching
the test fail. Do it without git:

```sh
cp crates/mc-common/src/properties.rs "$SCRATCHPAD/properties.rs.bak"
# break the thing on purpose, run the test, confirm it FAILS
cp "$SCRATCHPAD/properties.rs.bak" crates/mc-common/src/properties.rs
```

Better still for a small change: make the mutation with Edit and reverse it with
Edit. You know exactly what you changed, so the inverse is exact, and nothing
else in the file is ever at risk.

## What is untracked here is usually a whole feature

Adding a package to this repo means a new `crates/<name>/` **and** a new
`packages/<name>/`, neither of which git knows about until they are added. A
`git clean -fd` during that work removes the entire package — sources,
manifest, control file — and `git stash` without `-u` silently leaves it behind
while appearing to have tidied up.

Check before assuming the working tree is only edits:

```sh
git status --porcelain | grep '??'
```

## `git mv` for moves, and what it does to `git status`

Moving a file with `git mv` keeps the rename detectable, so history follows the
file into its new crate. Writing the new file and deleting the old one produces
an unrelated add and delete instead.

It also **stages** the move. A file that has been `git mv`'d shows nothing in
`git status` once its content matches the index — which is not the same as
"unchanged since HEAD". `git ls-files <dir>` tells you what git is tracking;
`git diff --cached` tells you what is staged.

## Pushing to `main` publishes

`.github/workflows/publish.yml` triggers on push to `main` and on `v*` tags,
for any change under `mc/**` or `apt/**`. That run signs the built `.deb` files
and indexes them into the live apt repository. Pull-request runs test and build
but publish nothing — every publishing step is gated on
`github.event_name != 'pull_request'`.

Two consequences follow, and both are irreversible:

- **reprepro regenerates its trees every run.** A package published with the
  wrong contents cannot be withdrawn, only superseded by a higher version.
- **A push without a version bump republishes the old version number with new
  contents.** Anyone who installed the earlier build is pinned to it forever,
  because apt only upgrades on a higher version.

On a feature branch, versions are held at the published number and bumped once
in a single commit before merge — so the bump and the push are the same
decision, made once, deliberately.

## Committing

- **Only when asked.** Finishing a change is not a request to commit it.
- **Never directly on `main`** — branch first.
- Interactive flags (`git rebase -i`, `git add -i`) do not work in this
  environment.
- Commit messages end with the `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`
  trailer; PR bodies end with the Claude Code line.

## Before you run it

1. `git status --porcelain` — what is edited, what is untracked.
2. `git log --oneline -3` — what is already safe in a commit.
3. If the command appears in the destroy table, use the alternative.
4. If it pushes to `main`, it publishes. Confirm that is intended.

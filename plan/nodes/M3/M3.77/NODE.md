---
id: M3.77
title: "Fix the annotated-tag check: actions/checkout rewrites the local tag ref to the commit"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: [.github/workflows/packages.yml]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-23T23:39"
updated: "2026-11-23T23:40"
---

# Problem

The stable `Packages` run on tag `v0.1.0` (run 45281353711) failed in `version` with:

    v0.1.0 is a lightweight tag; next: git tag -a --cleanup=verbatim v0.1.0

The tag **is** annotated: `gh api repos/.../git/ref/tags/v0.1.0` reports `object.type: tag`, and `git ls-remote origin 'refs/tags/v0.1.0' 'refs/tags/v0.1.0^{}'` returns both the tag object and its peeled commit.

Root cause: `actions/checkout@v4` checks out a tag by force-updating the **local** ref:

    git fetch ... +refs/tags/*:refs/tags/*          # annotated tag arrives
    git fetch --no-tags ... +<sha>:refs/tags/v0.1.0 # checkout replaces it with a lightweight tag
     t [tag update] 87d941d -> v0.1.0

So `git cat-file -t "$REF_NAME"` in the runner is always `commit`, for every annotated tag. The check can never pass.

# Change

In `.github/workflows/packages.yml` replace **both** occurrences of

    [ "$(git cat-file -t "$REF_NAME" 2>/dev/null || true)" = tag ] ||

with a remote-annotation check that survives checkout's local rewrite:

    # actions/checkout force-updates the local tag ref to the commit, so the local
    # ref is always lightweight. Ask the remote for the peeled entry instead: it
    # exists only for annotated tags.
    [ -n "$(git ls-remote origin "refs/tags/$REF_NAME^{}")" ] ||

Keep the failure message and the push / workflow_dispatch tag branches otherwise identical.

# Acceptance

- `uv run --no-project python -c 'import yaml;yaml.safe_load(open(".github/workflows/packages.yml"))'` OK; `actionlint` if present.
- `git ls-remote origin 'refs/tags/v0.1.0^{}'` is non-empty for the current tag (proves the new predicate passes).
- No other workflow asserts a tag type.
- Commit `[M3.77] ...`; `M3.77 -> done`.

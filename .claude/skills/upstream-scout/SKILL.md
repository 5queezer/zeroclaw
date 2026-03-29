---
name: upstream-scout
description: "Find, evaluate, and port ignored or mistreated PRs and issues from upstream ZeroClaw to Hrafn. Use when someone says 'find upstream PRs to port', 'what's worth cherry-picking from zeroclaw', 'scout upstream', 'check zeroclaw for ignored issues', or wants to identify contributions that were closed without comment, re-submitted by maintainers, or left without review. Also use when porting a specific PR and needing attribution guidance."
---

# Upstream Scout

Find valuable contributions that upstream ZeroClaw ignored, closed without comment, or re-submitted under maintainer names. Port them to Hrafn with proper attribution and cross-references.

## Upstream repo

`zeroclaw-labs/zeroclaw` on GitHub.

## Phase 1: Discovery

Use `gh` CLI to search for candidates. Start with PRs, issues later.

### Search queries

```bash
# PRs closed without merge, no comments from maintainers
gh pr list -R zeroclaw-labs/zeroclaw \
  --state closed --json number,title,author,closedAt,comments,labels,additions,deletions \
  --limit 100 | jq '[.[] | select(.comments == 0)]'

# PRs open for >14 days without review
gh pr list -R zeroclaw-labs/zeroclaw \
  --state open --json number,title,author,createdAt,reviewDecision,labels \
  --limit 100 | jq '[.[] | select(.reviewDecision == null)]'

# PRs by known mistreated contributors (add handles as discovered)
gh pr list -R zeroclaw-labs/zeroclaw \
  --state closed --author 5queezer --json number,title,state,mergedAt,closedAt
gh pr list -R zeroclaw-labs/zeroclaw \
  --state closed --author creke --json number,title,state,mergedAt,closedAt

# Issues with high engagement but no maintainer response
gh issue list -R zeroclaw-labs/zeroclaw \
  --state open --json number,title,comments,reactionGroups,createdAt \
  --limit 100 | jq '[.[] | select(.comments > 3)]'
```

### Known maintainer handles (do not credit these as community contributors)

- theonlyhennygod (lead)
- JordanTheJet (code owner)
- SimianAstronaut7 (code owner / collaborator)

## Phase 2: Evaluation

For each candidate, score on two axes:

### Axis 1: User impact (1-5)

| Score | Meaning |
|-------|---------|
| 5 | Security fix or crash prevention |
| 4 | Gap-creating feature (differentiates Hrafn from other claw implementations) |
| 3 | Quality-of-life improvement for existing users |
| 2 | Nice to have, minor improvement |
| 1 | Cosmetic or niche |

### Axis 2: Community signal (1-5)

| Score | Meaning |
|-------|---------|
| 5 | PR closed without comment AND re-submitted by maintainer under their name |
| 4 | PR closed without comment, contributor had tests + CI green |
| 3 | PR open >30 days, no review, contributor still active |
| 2 | Issue with >5 upvotes, no maintainer response |
| 1 | Standard closed PR with explanation |

### Priority matrix

- **Port first:** Impact >= 4 OR Community >= 4
- **Port second:** Impact >= 3 AND Community >= 2
- **Port for goodwill:** Impact < 3 AND Community >= 4
- **Skip:** Impact < 3 AND Community < 2

### Output format

For each candidate, produce:

```
PR #NNNN: <title>
Author: @handle
Impact: N/5 -- <reason>
Community: N/5 -- <reason>
Priority: Port first | Port second | Goodwill | Skip
Port method: cherry-pick | rewrite | adapt
Notes: <any context>
```

## Phase 3: Porting

### Method selection

| Situation | Method |
|-----------|--------|
| PR applies cleanly to Hrafn's current codebase | `git cherry-pick` (preserve author) |
| PR has merge conflicts but logic is sound | Rebase onto current main, preserve author |
| PR concept is good but implementation needs rework | Rewrite, use `Co-authored-by:` for original author |
| Only the idea/approach is useful, code is different | New implementation, credit in commit message body |

### Attribution rules (mandatory)

1. **Always preserve the original git author** when cherry-picking or rebasing. Never use `--reset-author`.
2. **Co-authored-by** when the port involves significant rewriting but the original contributor's design/approach is used:
   ```
   Co-Authored-By: Original Author <email@example.com>
   ```
3. **Commit message must reference the upstream PR:**
   ```
   feat(a2a): add outbound task delegation

   Ported from zeroclaw-labs/zeroclaw#4166 by @5queezer.
   Original PR was closed without review.
   ```
4. **CONTRIBUTORS.md entry** for every ported contributor (if file exists).

### Branch naming

```
port/zc-NNNN-short-description
```

### PR template additions for ports

Use the `PR: Port` label. In the PR description, add:

```markdown
## Upstream reference

- Original PR: zeroclaw-labs/zeroclaw#NNNN by @author
- Status: Closed without comment / Reverted and re-submitted / Open without review
- Changes from original: <what was adapted for Hrafn>
```

## Phase 4: Cross-reference (post-merge)

After the port PR is merged in Hrafn, comment on the **original upstream PR** (not the issue, the PR itself):

### Comment template

```markdown
Hi @{author} -- your work from this PR has been ported to
[Hrafn](https://github.com/5queezer/hrafn), a community-driven fork
with modular architecture and transparent governance.

See: https://github.com/5queezer/hrafn/pull/NN

Your original authorship is preserved in the git history. Thank you
for the contribution. If you'd like to contribute directly to Hrafn,
see our [CONTRIBUTING.md](https://github.com/5queezer/hrafn/blob/main/CONTRIBUTING.md).
```

### Rules for cross-referencing

- **Only comment on PRs where the contributor was demonstrably mistreated** (closed without comment, re-submitted by maintainer, ignored >30 days).
- **Never comment on PRs that were closed with a valid explanation.**
- **Never trash-talk ZeroClaw.** State facts: "closed without comment", "re-submitted as #NNNN." Let readers draw their own conclusions.
- **One comment per PR.** No follow-ups, no arguments.
- **Tone: sachlich.** Factual, grateful, inviting. Not promotional.

## Phase 5: Author Correction

Identify commits in Hrafn's history where a maintainer re-submitted
a community contributor's work and fix the git authorship.

### Detection

Run the deterministic detection script:

```bash
# Full run (git + GitHub API for author resolution)
./scripts/detect-resubmits.sh --cache-dir /tmp/resubmit-cache

# Local-only (git grep, no API — fast, partial results)
./scripts/detect-resubmits.sh --local

# Scan a specific branch (default: master)
./scripts/detect-resubmits.sh --branch main
```

Output is pipe-delimited TSV:
```
commit|maintainer|original_author|original_email|original_pr|has_coauthor|pattern
```

The script finds maintainer commits whose body contains attribution
patterns (`Supersedes #N`, `Based on #N`, `Adopted from #N`,
`Original work by @handle`, `merge ... PR #N`). It excludes
self-supersedes and "merge conflicts from PR" false positives.

**Do not use LLM prompts for detection.** The script is deterministic —
same repo state produces the same output every run. Adding detection
patterns requires editing the script, not the prompt.

### Generating the filter-repo script

After running detection, use the TSV output to generate the
`git filter-repo --commit-callback`. The LLM's role is limited to:

1. Reading the TSV output
2. Filling in missing emails for `handle:`-only entries (from other
   rows or via `gh api users/{handle}`)
3. Generating the Python callback with one entry per commit hash

```bash
git filter-repo --commit-callback '
import re

# Generated from: ./scripts/detect-resubmits.sh
# Each entry: (commit_hash_prefix, original_author, original_email, co_author_trailer)
fixes = {
    b"COMMIT_HASH_PREFIX": {
        "author_name": b"Original Author",
        "author_email": b"original@email.com",
        "co_author": b"Re-Submitter <re-submitter@email.com>",
    },
}

for prefix, fix in fixes.items():
    if commit.original_id.startswith(prefix):
        commit.author_name = fix["author_name"]
        commit.author_email = fix["author_email"]
        trailer = b"Co-Authored-By: " + fix["co_author"]
        if trailer not in commit.message:
            commit.message = commit.message.rstrip() + b"\n\n" + trailer + b"\n"
        break
' --force
```

### Rules

- **Run before M4 (Community Launch).** After external forks exist, rewriting history is a breaking change.
- **Backup first.** `git clone --mirror` before any filter-repo operation.
- **Verify after.** `git log --all --format="%H %an <%ae> %s" | grep -i "original-author"` to confirm fixes applied.
- **Force-push once.** Batch all corrections into a single rewrite, not multiple force-pushes.
- **Script is source of truth.** If a commit is not in the script output, it does not get rewritten. No ad-hoc LLM discovery.

## Limitations

- This skill searches PRs first, issues later (to avoid overloading the CLI).
- GitHub API rate limits apply. Use `--limit` flags and paginate if needed.
- The skill cannot access private repos or deleted PRs.
- Attribution requires the original contributor's git email. If unavailable, use their GitHub handle with `@handle` in the commit message body.

## When NOT to use this skill

- For porting OpenClaw plugins (use the OC Bridge workflow instead).
- For features that don't exist upstream (just build them).
- For upstream PRs that were closed with a valid technical explanation.

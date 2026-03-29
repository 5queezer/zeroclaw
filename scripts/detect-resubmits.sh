#!/usr/bin/env bash
# detect-resubmits.sh — find commits where maintainers re-submitted community work
#
# Deterministic: same repo state + same upstream API = same output, every time.
# The LLM has no role in detection. It only consumes the output.
#
# Usage:
#   ./scripts/detect-resubmits.sh              # full run (git + GitHub API)
#   ./scripts/detect-resubmits.sh --local      # git-only, no API calls
#   ./scripts/detect-resubmits.sh --cache-dir /tmp/resubmit-cache
#
# Output: TSV to stdout
#   commit|maintainer|original_author|original_email|original_pr|has_coauthor|pattern
#
# Exit codes: 0 = found results, 1 = error, 2 = no results

set -uo pipefail

# --- Config -----------------------------------------------------------

UPSTREAM_REPO="zeroclaw-labs/zeroclaw"

MAINTAINERS=(
  "theonlyhennygod"
  "SimianAstronaut7"
  "JordanTheJet"
)

# --- Args -------------------------------------------------------------

LOCAL_ONLY=false
CACHE_DIR=""
BRANCH="master"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --local)      LOCAL_ONLY=true; shift ;;
    --cache-dir)  CACHE_DIR="$2"; shift 2 ;;
    --branch)     BRANCH="$2"; shift 2 ;;
    --all)        BRANCH=""; shift ;;
    -h|--help)
      sed -n '2,/^$/p' "$0" | sed 's/^# \?//'
      exit 0
      ;;
    *) echo "Unknown option: $1" >&2; exit 1 ;;
  esac
done

[[ -n "$CACHE_DIR" ]] && mkdir -p "$CACHE_DIR"

# --- Helpers ----------------------------------------------------------

gh_pr_author() {
  local pr="$1"

  if [[ -n "$CACHE_DIR" ]] && [[ -f "$CACHE_DIR/pr-${pr}.txt" ]]; then
    cat "$CACHE_DIR/pr-${pr}.txt"
    return
  fi

  local result=""
  result=$(gh pr view "$pr" -R "$UPSTREAM_REPO" \
    --json author,commits \
    --jq '
      .author.login as $login |
      (.commits[0].authors[0].email // "") as $email |
      "\($login)|\($email)"
    ' 2>/dev/null) || true

  if [[ -z "$result" ]]; then
    result=$(gh issue view "$pr" -R "$UPSTREAM_REPO" \
      --json author --jq '.author.login + "|"' 2>/dev/null) || true
  fi

  if [[ -n "$CACHE_DIR" ]] && [[ -n "$result" ]]; then
    echo "$result" > "$CACHE_DIR/pr-${pr}.txt"
  fi

  echo "$result"
}

is_maintainer() {
  local login="$1"
  for m in "${MAINTAINERS[@]}"; do
    [[ "${login,,}" == "${m,,}" ]] && return 0
  done
  return 1
}

# --- Phase 1: git-only detection -------------------------------------
#
# Attribution patterns — exhaustive list. Adding a pattern here is the
# ONLY way to expand detection scope. No LLM guesswork.
#
#   Supersedes #N
#   Based on (PR )?#N (by @handle)?
#   Adopted from #N (by @handle)?
#   Fixups over original PR #N
#   Original work by @?handle
#   merge .* PR #N (in subject — but NOT "merge conflicts from PR")

# Combined grep pattern (case-insensitive)
BODY_GREP='supersedes #[0-9]|based on (pr )?#[0-9]|adopted from #[0-9]|fixups over original pr #[0-9]|original work by'

# Separate pattern for the "merge ... PR #N" case (needs negative filter)
# Note: .*pr (no space before pr) to match both " PR" and "(PR"
MERGE_PR_GREP='merge .*pr #[0-9]'
MERGE_PR_EXCLUDE='merge conflicts'

PHASE1=$(mktemp)
trap 'rm -f "$PHASE1"' EXIT

# Use process substitution to avoid subshell
while read -r hash; do
  body=$(git show -s --format="%B" "$hash" 2>/dev/null) || continue
  author=$(git show -s --format="%an" "$hash" 2>/dev/null) || continue

  # Does the body match any attribution pattern?
  has_body_match=false
  has_merge_match=false
  echo "$body" | grep -qiE "$BODY_GREP" && has_body_match=true
  if echo "$body" | grep -qiE "$MERGE_PR_GREP"; then
    # Only count "merge ... PR #N" if it's NOT "merge conflicts from PR"
    if ! echo "$body" | grep -qiE "$MERGE_PR_EXCLUDE.*pr #[0-9]"; then
      has_merge_match=true
    fi
  fi
  [[ "$has_body_match" == "false" && "$has_merge_match" == "false" ]] && continue

  # Extract referenced PR numbers
  pr_numbers=$(echo "$body" \
    | grep -oiE '(supersedes|based on( pr)?|adopted from|fixups over original pr) #[0-9]+' \
    | grep -oE '[0-9]+' \
    | sort -u) || true

  # Add PR numbers from valid "merge ... PR #N" patterns (excluding "merge conflicts")
  if [[ "$has_merge_match" == "true" ]]; then
    merge_prs=$(echo "$body" \
      | grep -iE "$MERGE_PR_GREP" \
      | grep -viE "$MERGE_PR_EXCLUDE" \
      | grep -oiE 'pr #[0-9]+' \
      | grep -oE '[0-9]+' \
      | sort -u) || true
    pr_numbers=$(echo "$pr_numbers $merge_prs" | tr ' ' '\n' | sort -u | grep -v '^$') || true
  fi

  # Extract @handles from "Original work by @handle" or "Original work by handle"
  orig_handles=$(echo "$body" \
    | grep -oiE 'original work by @?[A-Za-z0-9_-]+' \
    | sed 's/.*[Bb]y @\?//' \
    | sort -u) || true

  # Extract handles from "by @handle" in Based-on / Adopted-from lines
  inline_handles=$(echo "$body" \
    | grep -oiE '(based on|adopted from) #[0-9]+ by @[A-Za-z0-9_-]+' \
    | grep -oE '@[A-Za-z0-9_-]+' \
    | tr -d '@' \
    | sort -u) || true

  # Count non-Claude Co-authored-by lines
  coauthor_total=$(echo "$body" | grep -ciE 'co-authored-by:' || true)
  coauthor_claude=$(echo "$body" | grep -ciE 'co-authored-by:.*claude' || true)
  human_coauthor=$((coauthor_total - coauthor_claude))

  matched_pattern=$(echo "$body" | grep -oiE "$BODY_GREP" | head -1) || true

  # Emit one line per PR reference
  for pr in $pr_numbers; do
    echo "${hash}|${author}|pr:${pr}|${human_coauthor}|${matched_pattern}"
  done

  # Emit one line per handle reference
  all_handles=$(echo "$orig_handles $inline_handles" | tr ' ' '\n' | sort -u | grep -v '^$') || true
  for handle in $all_handles; do
    echo "${hash}|${author}|handle:${handle}|${human_coauthor}|${matched_pattern}"
  done

done < <(
  git_log_args=(--format="%H"
    --author="${MAINTAINERS[0]}"
    --author="${MAINTAINERS[1]}"
    --author="${MAINTAINERS[2]}")
  if [[ -n "$BRANCH" ]]; then
    git log "${git_log_args[@]}" "$BRANCH"
  else
    git log "${git_log_args[@]}" --all
  fi
) > "$PHASE1"

# Deduplicate — prefer pr: lines over handle: lines.
# Reverse-sort on field 3 puts pr: before handle: (p > h), then awk deduplicates
# on (hash, ref) while preserving that order.
sort -t'|' -k1,1 -k3,3r "$PHASE1" \
  | awk -F'|' '!seen[$1,$3]++' > "${PHASE1}.dedup"
mv "${PHASE1}.dedup" "$PHASE1"

phase1_count=$(wc -l < "$PHASE1" | tr -d ' ')

if [[ "$phase1_count" -eq 0 ]]; then
  echo "No attribution patterns found in maintainer commits." >&2
  exit 2
fi

echo "# Phase 1: ${phase1_count} candidate lines (git-only)" >&2

# --- Local-only output ------------------------------------------------

if [[ "$LOCAL_ONLY" == "true" ]]; then
  echo "# commit|maintainer|reference|human_coauthors|pattern" >&2
  cat "$PHASE1"
  exit 0
fi

# --- Phase 2: GitHub API resolution -----------------------------------

echo "# Phase 2: resolving original authors via GitHub API..." >&2
echo "commit|maintainer|original_author|original_email|original_pr|has_coauthor|pattern"

declare -A SEEN

while IFS='|' read -r hash maintainer ref coauthor pattern; do
  ref_type="${ref%%:*}"
  ref_value="${ref#*:}"

  original_login=""
  original_email=""
  original_pr=""

  if [[ "$ref_type" == "pr" ]]; then
    original_pr="$ref_value"
    author_info=$(gh_pr_author "$ref_value")
    if [[ -n "$author_info" ]]; then
      original_login="${author_info%%|*}"
      original_email="${author_info#*|}"
    fi
  elif [[ "$ref_type" == "handle" ]]; then
    original_login="$ref_value"
  fi

  # Skip unresolved
  [[ -z "$original_login" ]] && continue

  # Skip self-supersedes
  is_maintainer "$original_login" && continue

  # Dedup by (hash, login)
  dedup_key="${hash}:${original_login}"
  [[ -n "${SEEN[$dedup_key]+x}" ]] && continue
  SEEN[$dedup_key]=1

  # Check if original contributor has Co-authored-by credit
  has_coauthor="no"
  if [[ "$coauthor" -gt 0 ]]; then
    body=$(git show -s --format="%B" "$hash" 2>/dev/null) || true
    if echo "$body" | grep -qiE "co-authored-by:.*${original_login}"; then
      has_coauthor="credited-as-coauthor"
    else
      has_coauthor="other-coauthor"
    fi
  fi

  echo "${hash}|${maintainer}|${original_login}|${original_email}|${original_pr}|${has_coauthor}|${pattern}"

done < "$PHASE1"

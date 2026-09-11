#!/usr/bin/env bash
# Pre-push guard: blocks pushing proprietary content to GitHub
# Install: cp .github/hooks/pre-push-proprietary-guard.sh .git/hooks/pre-push && chmod +x .git/hooks/pre-push

REMOTE="$1"

# Only guard pushes to GitHub
if [[ "$REMOTE" != *"github"* ]]; then
  exit 0
fi

PROPRIETARY_PATTERNS=(
  "docs/enterprise/"
  "docs/contracts/billing-"
  "docs/contracts/settlement-"
  "docs/contracts/org-"
  "docs/contracts/team-server-"
  "docs/contracts/hosted-personal-"
  "docs/contracts/email-digest-"
  "docs/contracts/device-overview-"
  "docs/contracts/ccp-session-"
  "docs/contracts/frozen-hashes"
  "docs/contracts/compliance-report-"
  "docs/business/"
  "docs/adrs/SYSTEM-INVENTORY"
)

VIOLATIONS=""
while read LOCAL_REF LOCAL_SHA REMOTE_REF REMOTE_SHA; do
  if [ "$LOCAL_SHA" = "0000000000000000000000000000000000000000" ]; then
    continue
  fi
  
  if [ "$REMOTE_SHA" = "0000000000000000000000000000000000000000" ]; then
    # New branch on the remote: every file in the pushed tree is newly
    # published. The previous fallback compared the working tree against HEAD,
    # which is empty on a clean checkout — the guard passed by construction.
    FILES=$(git ls-tree -r --name-only "$LOCAL_SHA" 2>/dev/null || true)
  else
    FILES=$(git diff --name-only "$REMOTE_SHA..$LOCAL_SHA" 2>/dev/null || true)
  fi
  
  for pattern in "${PROPRIETARY_PATTERNS[@]}"; do
    MATCHES=$(echo "$FILES" | grep "$pattern" || true)
    if [ -n "$MATCHES" ]; then
      VIOLATIONS="$VIOLATIONS\n  $MATCHES"
    fi
  done
done

if [ -n "$VIOLATIONS" ]; then
  echo "🚫 BLOCKED: Proprietary content detected in push to GitHub!"
  echo -e "   Files:$VIOLATIONS"
  echo ""
  echo "   These files belong in lean-ctx-enterprise (GitLab only)."
  echo "   To bypass (dangerous): SKIP_PROPRIETARY_GUARD=1 git push ..."
  exit 1
fi

exit 0

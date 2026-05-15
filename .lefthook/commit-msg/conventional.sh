#!/bin/sh
# Conventional Commits subject-line gate (run by lefthook's commit-msg hook).
#
# $1 = path to the commit message file (git passes COMMIT_EDITMSG). We only
# validate the first line (the subject). ASCII-only output so the message is
# safe in any terminal / git GUI on Windows + WSL + macOS + Linux.
#
# Why a script file and not an inline lefthook `run:` — lefthook mangles a
# multi-line shell `case` passed through a YAML block scalar; a real script
# sidesteps all the quoting/templating ambiguity. POSIX sh, no Node/commitlint
# dependency (matches the project's lean ethos).

msg=$(head -n1 "$1")

# Exempt the messages git / rebase generate automatically.
case "$msg" in
  "Merge "* | "Revert "* | "fixup! "* | "squash! "* | "amend! "*)
    exit 0
    ;;
esac

if printf '%s' "$msg" | grep -qE '^(feat|fix|chore|docs|style|refactor|perf|test|build|ci|revert)(\([^)]+\))?!?: .+'; then
  exit 0
fi

{
  echo ""
  echo "ERROR: commit message is not Conventional Commits."
  echo "  Format: <type>(optional-scope)(!)?: <subject>"
  echo "  Types:  feat fix chore docs style refactor perf test build ci revert"
  echo "  Got:    $msg"
  echo "  (Merge / Revert / fixup! / squash! / amend! commits are exempt.)"
} >&2
exit 1

#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The agent skill (ruling R8) is authored in this repo and mirrored
# into each agent system's skill tree. The mirrors are outside version
# control, so this script is the only thing that keeps them honest.
#
#   scripts/install-skill.sh          install/refresh the mirrors
#   scripts/install-skill.sh --check  verify only; exit 4 on drift
#
# The Hermes mirror is NOT touched: skills sync there only on Ben's
# manual ping (filing cabinet §2).
set -uo pipefail

project_directory="$(cd "$(dirname "$0")/.." && pwd)"
source_file="${project_directory}/docs/skill/SKILL.md"
mirrors=(
  "${HOME}/.claude/skills/sysmon/SKILL.md"
  "${HOME}/.agents/skills/sysmon/SKILL.md"
)

[ -f "${source_file}" ] || {
  printf '%s\n' "install-skill: no source at ${source_file}" >&2
  printf '%s\n' "fix: the skill is authored at docs/skill/SKILL.md — restore it from git" >&2
  exit 3
}

check_only=0
[ "${1:-}" = "--check" ] && check_only=1

drifted=()
for mirror in "${mirrors[@]}"; do
  if ! cmp -s "${source_file}" "${mirror}" 2>/dev/null; then
    drifted+=("${mirror}")
  fi
done

if [ "${check_only}" = "1" ]; then
  if [ "${#drifted[@]}" = "0" ]; then
    printf '\033[32m✓ skill mirrors match docs/skill/SKILL.md\033[0m\n'
    exit 0
  fi
  printf '\033[31m✗ skill mirrors drifted from docs/skill/SKILL.md:\033[0m\n' >&2
  printf '    %s\n' "${drifted[@]}" >&2
  printf '%s\n' "fix: run scripts/install-skill.sh (or copy the repo's version over them)" >&2
  exit 4
fi

for mirror in "${mirrors[@]}"; do
  mkdir -p "$(dirname "${mirror}")"
  cp "${source_file}" "${mirror}"
  printf '  → %s\n' "${mirror}"
done
printf '\033[32m✓ skill installed from docs/skill/SKILL.md\033[0m\n'
printf '%s\n' "note: the Hermes mirror is left alone — skills sync there on Ben's ping only."

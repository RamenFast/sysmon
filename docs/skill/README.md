# The agent skill lives here

`SKILL.md` in this directory is the **source of truth** for sysmon's
agent skill (workspace standard ruling R8). The copies under
`~/.claude/skills/sysmon/` and `~/.agents/skills/sysmon/` are
installed mirrors.

Why the repo owns it: the mirrors sit outside version control, so
nothing noticed when they drifted. In the v3.0.2 round they told
agents to drive the control socket with `socat`, which is not
installed on this machine — the repo's own docs had the same bug,
but only the repo's copy was ever reviewed.

Edit `SKILL.md` here, then install the mirrors:

```bash
scripts/install-skill.sh          # copies to the Claude + agents trees
scripts/install-skill.sh --check  # verifies they match; exit 4 if not
```

`scripts/conformance.sh` runs the check (C16), so a drifted mirror
fails the release gate.

The Hermes mirror (`~/.hermes/skills/station/sysmon/`) is deliberately
**not** installed by this script: skills sync to Hermes only on Ben's
manual ping (filing cabinet §2). `concourse doctor` warns that it is
missing, which is the honest state, not a defect to paper over.

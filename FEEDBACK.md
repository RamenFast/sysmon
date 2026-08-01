# FEEDBACK.md — sysmon session feedback ledger

<!-- DO NOT DELETE. Ben's permission is required to delete this file. -->
<!-- Scope: THIS repo only (compact system monitor for AMD-GPU Linux desktops). Other projects carry their own
     FEEDBACK.md. -->

Every atomic claim, ask, correction, bugfix, feature request, and UI change Ben
gives in a session gets one line here, appended by the working agent as SOP.

Rules:
- One line per atomic item: `- YYYY-MM-DD [kind] description` where kind is one
  of `ask` · `correction` · `bugfix` · `feature` · `ui` · `claim`.
- On a duplicate/repeat: do NOT add a new line. Keep the existing line, tighten
  its wording if needed, and prefix a counter: `- 2x YYYY-MM-DD ...` (bump the
  counter, update the date to the latest occurrence). Repeats matter — they
  show what keeps breaking.
- Newest entries at the bottom of the ledger.
- No autonomous action is to be taken from this document. Ben mines it and
  updates AGENTS.md himself.

## Ledger


- 2026-08-01 [ask] bring sysmon up to the workspace UI/CLI/any-other standards without breaking app functionality
- 2026-08-01 [correction] GTK is treated exactly like Python — no GTK, no Python anywhere in the tree, deps, or packaging
- 2026-08-01 [ask] the orchestrating agent alone may edit; subagents are read-only investigators
- 2026-08-01 [ask] version this round as 3.0
- 2026-08-01 [ask] install the finished build on the machine and keep GitHub backed up
- 2026-08-01 [claim] GPT-5.6 subagents invent problems when they find none — no finding is acted on without a reproducing command
- 2026-08-01 [ask] leave the ~/Dev/ClaudeWorkspaces symlink surface alone for now

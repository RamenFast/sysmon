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
- 2026-09-05 [ask] Compress this project's agent context without information loss. Exclude Nexus's home and exclusive files.
- 2026-10-01 [ask] complete statistics accuracy check: "idk if I can fully trust the statistics"
- 2026-10-01 [claim] it says weird things about RAM usage
- 2026-10-01 [feature] temperatures in °F and °C, togglable, with both at once as an option; turn both on for Ben
- 2026-10-01 [correction] GB not GiB ("big memory psyop"); decimal everywhere, honest usable vs installed labels, hover note OK
- 2026-10-01 [ui] make the sensors section easier to read
- 2026-10-01 [feature] deeper inspection of processes
- 2026-10-01 [ui] nice integration between the Overview and the Processes tab
- 2026-10-01 [bugfix] right-click a process on the main page: the submenu's actions must act on the entry that was right-clicked
- 2026-10-01 [ui] cute custom symbols for the relevant parts of the app ("chef's touch")
- 2026-10-01 [ask] reduce code complexity
- 2026-10-01 [ask] take care of GitHub, make it look and sound good; test the application; fully autonomous tonight, root available
- 2026-10-01 [ask] up to 2 subagents at high reasoning (Opus 5.5 recommended)
- 2026-10-01 [ask] if hardware sensors need a boot flag or reboot, install what's possible and leave the rest documented for the next instance
- 2026-10-01 [claim] "if this application needs 100 dependencies to gather the correct hardware information, that's what we do"

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
- 2026-10-01 [ask] a new design pass: compact, efficient, all relevant info in beautiful charts/graphs, in the spirit of the original Windows XP Task Manager with your own coat of paint; no GTK
- 2026-10-01 [decision] Performance becomes the startup page; Overview and Processes stay as tabs
- 2026-10-01 [decision] graphs always on a dark instrument field, tinted per theme, even on light themes
- 2026-10-01 [decision] CPU graph: a toggle between combined and per-thread that works at every window size
- 2026-10-01 [context] the CPU cooler is known to be underpowered; a high Tctl is expected, not a fault. Show it as data next to the clock, never as a warning
- 2026-10-01 [ask] full creative and technical authority for the 3.2 round; report at the end
- 2026-10-09 [claim] "why does it say 135? I have a 128gb kit" — the Usable 135.0 GB (decimal MemTotal) read as more than the sticks
- 2026-10-09 [feature] add a copy button to SysMon (system specs to the clipboard)
- 2026-10-09 [feature] copy specs for each pop-out window too ("for individual windows too")
- 2026-10-09 [claim] the copied spec sheet is too thin to share: "i need more than that to share... please?"
- 2026-10-09 [feature] yes to SysMon 3.4: a fuller shareable spec sheet (CPU cache and clocks, GPU link and driver stack, display, every physical drive, network, audio)
- 2026-10-10 [bugfix] per-process GPU memory is wrong: "Was really hoping those usage statistics were working… wanted to see how much of the GPU power I was using and how much memory" (ghostty 620 GB, sway 1.98 TB of VRAM on a 32 GB card)
- 2026-10-10 [context] purpose: see how much GPU and VRAM a game (Arc Raiders) uses while Nexus's local model stays loaded, so the model can stay open while he plays
- 2026-10-10 [feature] a copy button in each pop-out window that copies its specs plus recent usage history, back as far as settings allow; the copied statistics must be easy to share, accurate and organized
- 2026-10-10 [feature] a usage-history logging system built into sysmon (sensors, CPU, GPU, …), with a tooling surface Nexus can read ("a tooling surface for you to see")
- 2026-10-10 [decision] yes to releasing 3.4.0 with these additions
- 2026-10-10 [ask] how much disk space does the history use, is it a database (should it be), how much CPU/RAM, can it be more performant, can it be backed up / cleared, does it have the slider
- 2026-10-10 [feature] yes to Clear and Export buttons for the usage history
- 2026-10-10 [decision] old history must not auto-delete: "id want to see what happens when we keep collecting this info over a long time period, maybe after 7 days it compresses"; worried about SSD bloat and output that is hard to navigate, so the long-term tier must be dense and its growth visible
- 2026-10-10 [decision] yes to releasing 3.5.0 with the tiered history

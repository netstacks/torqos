# Handoff: the agents work (ADR-051), now on main

The resident agent, torq-mcp, the agent's applet and window, and the approval of an agent's use of a secret
(ADR-051 steps 1 to 5; ADR-052 step 4) are on `main`. This note used to describe the parked `work/agents`
branch, its commits and its rebase; all of that is done and is gone from here. What is built is in ADR-051's and
ADR-052's "As built" and "Amended" sections in docs/DECISIONS.md and KNOWN-GAPS 6zzk to 6zzo and 6zzs. Nothing is
released: no version has been bumped for it.

A full `nix flake check` (every check, all VM tests) passed on .52 for 880c7ff (2026-11-12), 5c9c84c
(2026-11-13, the agents merge), 1121957 (2026-11-14, the approval flow: the new `tests/secrets.nix` and
`tests/desktop.nix` subtests ran) and 4f077d9 (2026-11-14, main at that time).

## Never run

- The real Claude Code: only a stand-in MCP client. Untested is whether it takes a user-scope server from
  `~/.claude.json`, whether it follows a symlinked skill directory, and how its sign-in behaves as the agent account.
- Any agent UI on a real machine (the applet, the window, the approval dialog, the Agent page, Super+Shift+A, the
  launcher, crash notifications). The VM screenshots don't show the applet or window drawn.
- `coredumpctl info` with the crash watcher's matches was run by hand against a real entry on 2026-11-14, and
  `cli/src/agent/crash.rs` has a test from that real entry's format. The watcher's loop and the notification
  are still unseen on a desktop.

## Not built

- "Start Claude Code" from the agent's window (it needs a folder picker and a terminal).
- Codex and opencode.
- The screen grant (ADR-051 6: `screen_capture`/`screen_watch` through the portal).
- The rest of torqcli's `show` tree as tools (phase 2).

## Still open by design

See KNOWN-GAPS 6zq, 6zzk, 6zzo and 6zzs.

- Root can be the resident.
- A kernel before 6.5 taints everything.
- A remote https model is believed; a plain-http or open-socket one on this machine is tainted.
- `torq doctor` and `torq agent propose` have no session trust and cannot use a keyed model; `torq ask` is the way.

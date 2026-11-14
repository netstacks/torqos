# Handoff: `work/agents` (ADR-051 phase 1), parked 2026-11-11

Parked by the owner on 2026-11-11 while the desktop and secrets go out as 0.4.0-rc1. Agent work waits for that
base. The branch is local only; the owner chose not to push it.

- **Last known good:** `eef8e57`, tagged `park/agents-known-good` (local tag). Every cargo suite, `checks.agent-assertions`
  and the VM tests agent-service, agent-mcp, agent-proposals, agents-per-person, doctor, agent-run, agent and
  desktop-agent passed there, on the laptop, throttled.
- **After it:**
  - `10f22cc` and `83d3fac`: finished fixes from the second review.
  - The WIP commit: the rest of that batch as it stood when stopped. It is untested beyond cargo.
  - This note.
- **Cargo at the WIP tip (2026-11-11, `nix develop`, `-j 8`):** all pass — broker 268, torqcli 370, agent 62, wire 6.
  The settings crate (libcosmic) was **not** built, and **no** VM test was run after `eef8e57`.
- **Based on** `21d91fa` (main, 2026-11-12), rebased from `2ad0c75`: below, To resume 1. The hashes above are the parked branch's; rebased, they are new (the commits' subjects are unchanged).

## Done, independently reviewed and fixed

These are described in ADR-051's "As built" sections in docs/DECISIONS.md and KNOWN-GAPS 6zzk–6zzo.

- **Step 1:** an agent account per person (`torq-agent-<person>`, `torq.agent.people`, the broker's `agent.agents`
  map, `--agent` added by the socket, `torq agent run` by `SUDO_UID`).
- **Step 2:** the resident `torq-agent@<person>` service on `/run/torq-agent/<person>.sock`, the torq-wire
  `agent` protocol, `torq ask`, memory, and shares by freezer (`--hold-frozen`).
- **Step 3:** the agent's applet (`org.torqos.Agent`, in the default panel), its window (`torq-agent-ui`) and
  the Agent page.
- **Decline:** `torq-broker decline`, `request system decline <n>`, the buttons, at most 100 not done per
  agent, and a failed apply that still waits.
- **Step 4:** Super+Shift+A, the launcher's `ask`, the Ask the agent buttons, crash help through
  systemd-coredump, and the terminal's `ask`.
- **Step 5:**
  - `torq-mcp`: MCP over stdio, with the doctor's closed and tiered tools and `propose_change`.
  - Claude Code as a tool: `torq.agent.tools`, gated by unfree, in the agent's own profile, with `--link` in
    the sandbox writing `~/.claude.json` and the skill link.
  - The broker's resident rule: SO_PEERPIDFD plus the unit's cgroup, plus the resident's own uid.
  - The model-endpoint taint: plain http and an open socket chain are tainted.
  - The key by LoadCredential.
  - `torq-resident-<person>`, the owner's choice on 2026-11-11, amending decision 3. The person sees one agent.

## Open: the second review's findings

Done in `10f22cc`/`83d3fac`, unit-tested only:
- (2) https to this host is tainted.
- (3) model_taint fails closed when passwd cannot be read.
- (5) the resident has its own session and not-done budgets.
- (7) decline verifies and reconciles the ledger first.
- (6) `machine::within` joins its readers by the deadline.

Partly done in the WIP commit, untested:
- (1) done: the handoff question is capped at 200 characters, cut with "…", tested; ADR-051 says so.
- (8) done: the crash entry is read with its boot id too (coredumpctl takes no cursor), tested and mutation-checked.
- (4) the apiKeyFile path is normalised, and its source is checked at unit start (root-owned, not readable by
  group or other).
- (12) done: the Delegate assertion also covers the `torq-agent@` template (mutation-checked); its messages say the uids are the
  first defence and the cgroup and ptrace guards second.
- (9) done, type-checked only (settings has no tests): `open_window` is off the update path (it froze the UI for up to 10 s), its failure said; `cosmic-edit`/`xdg-open` are
  reaped in tunnels.rs, and neither starting is said.
- (10) done: crash offers wait with a timeout (expiry + 30 s), so three unclosed notifications don't stop them for good; `heard()`/`give_up_at()` tested and mutation-checked; a failed open said to the person. The watcher's loop is type-checked only.

Not started:
- (11) done: `--service-type=exec` makes a window that cannot start fail systemd-run, said where pressed; the activation token is passed from a crash notification's click (tested, mutation-checked); the applets', Settings' and the launcher's buttons pass none yet (KNOWN-GAPS 6zzn).
- **Doc corrections:** done (2026-11-12): ADR-051's step 4 note (the programs waited for, the environment,
  coredumpctl's matches); the comments in crash.rs and agent_ui.rs; Decline said to be narrower than the broker on
  purpose (`Proposal.yours`, 6zzn); 6zzo on what /proc still shows (cmdline and status, never a task's text).
- **Test gaps,** checked 2026-11-12 by breaking each guard and running the broker's suites:
  - each is now noticed: `trusted_by`'s caller check (trust_applies_only_what_the_agent_itself_sent); the socket's
    `resident_account` checks (the_resident_account_is_the_persons_one_agent, residency_takes_the_uid_and_the_unit);
    `model_taint`'s real passwd (three tests); `resident_of` and the residents in the model's judgement
    (the_brokers_model_judgement_and_its_residents); the 100 boundary, the decline check's order and the model
    judged again at a trusted apply (the second review's commits); crash.rs's checks (finding 8); `notify()`'s
    (finding 10);
  - still not: window.rs's `p.yours` gates and its `Failed` arm (the settings crate has no tests).

To finish the batch, settle the WIP's items, then re-run:
- the cargo suites and agent-assertions;
- agent-service, agent-mcp, agent-proposals, agents-per-person, doctor, agent-run, agent and desktop-agent;
- desktop and tunnels, because settings changed.

## Never run

- A full `nix flake check` of work/agents, on .52 or anywhere.
- The real Claude Code: only a stand-in MCP client. Untested is whether it takes a user-scope server from
  `~/.claude.json`, whether it follows a symlinked skill directory, and how its sign-in behaves as the agent account.
- Any agent UI on a real machine (the applet, the window, the Agent page, Super+Shift+A, the launcher, crash
  notifications). The VM screenshots don't show the applet or window drawn.

## Not built

- "Start Claude Code" from the agent's window (it needs a folder picker and a terminal).
- Codex and opencode.
- The screen grant (ADR-051 6: `screen_capture`/`screen_watch` through the portal).
- The rest of torqcli's `show` tree as tools (phase 2).

## Still open by design

See KNOWN-GAPS 6zq, 6zzk and 6zzo.

- Root can be the resident.
- A kernel before 6.5 taints everything.
- An https model on this machine is now tainted (WIP), but a remote TLS model is believed.
- `torq doctor` and `torq agent propose` have no session trust and cannot use a keyed model; `torq ask` is the way.

## To resume

1. **Rebased onto main `21d91fa` (2026-11-12)**, as `work/agents-rebase` (the parked `work/agents` and its tag are
   kept as they were). Before replaying, the branch's own KNOWN-GAPS entries were renamed in every commit,
   messages included: 6zv–6zz became 6zzk–6zzo, since main had taken 6zv–6zzj meanwhile. Main's org move came in
   with it, `torq.releases.url` included: its default is now torqosorg's releases, and the old address is a
   `former` one that the next update moves a machine off (modules/releases.nix).
   - Conflicts, all resolved by keeping both sides, except for these:
     - modules/agent-run.sh takes main's fix of the set-id sweep (it runs as the agent, through `setpriv`) with
       `"$agent"`, and keeps the branch's one sandbox.
     - CLAUDE.md is main's throughout. The branch's edits to it wait for the owner's word.
     - The guide's paragraph saying another program can look untainted was dropped, because step 5 makes every
       program but the resident agent start tainted.
   - Only the tip was compiled. Commits from "one agent account per person" to the rebase fix (fa9e6da) do not build
     the broker binary: `connected()` (secrets, main) calls `socket::caller` with the old three arguments. Left there,
     not moved into each commit: a bisect in that range needs the same one-line change, with that commit's arguments.
   - **Settled (the approval design's part 1, 2026-11-12):** `torq-agent` in a person's rules is their own agent,
     both accounts; system rules name each agent's account. The two broker tests pass again. What it leaves:
     no agent can use a secret until approval is built (KNOWN-GAPS 6zzs). (Built 2026-11-13: ADR-051 and ADR-052,
     amended that day.)
2. Finish the second review's batch, as above.
3. Run the full check on .52, then land as a fast-forward with the owner (the lead session's plan).

Heavy work goes to .52 by default. On the laptop it runs throttled: one job, `nice -n 10 … --max-jobs 1 --cores 8`,
≥ 8 GB available to start, stop under 5 GB, no fat-LTO cosmic builds.

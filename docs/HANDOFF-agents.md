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
- **Based on** `2ad0c75` (= main as of 2026-11-11).

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
- (1) the handoff question is capped to what the one-line box shows. ADR-051 still says "the box shows all of it".
- (8) the crash entry is read by cursor or boot id, not by matching fields across boots.
- (4) the apiKeyFile path is normalised, and its source is checked at unit start (root-owned, not readable by
  group or other).
- (12) the Delegate assertion also covers the `torq-agent@` template. Its messages should say the uids are the
  first defence and the cgroup and ptrace guards second.
- (9) `open_window` is off the update path (it froze the UI for up to 10 s), and `cosmic-edit`/`xdg-open` are
  reaped in tunnels.rs.
- (10) crash offers wait with a timeout, so three unclosed notifications don't stop them for good.

Not started:
- (11) a launch failure after `systemd-run` is silent (handoff.rs), and no activation token is passed.
- **Doc corrections:**
  - In the ADR's step 4 fix note: "reads exactly that entry", "the programs started are waited for", and "with the
    session's environment" (it is the user manager's).
  - Comments: crash.rs:1-3 (COREDUMP_UID) and agent_ui.rs:221.
  - Decline is offered more narrowly than the broker allows: not for a proposal wholly in the person's folder, and
    not to an administrator. Offer it where the broker allows, or say it is conservative; fix the comment on
    `Proposal.yours`.
  - 6zzo: the agent can still read the resident's `/proc/<pid>/cmdline` and `status` (no hidepid).
- **Test gaps,** each a mutation nobody would notice:
  - `trusted_by`'s caller check (session.rs) and `who.resident_account &&` (serve.rs);
  - `Broker::model_taint`'s real passwd and residents set, and `Broker::resident_of`;
  - the 100 boundary exactly;
  - the decline check's order in `schedule` (at.rs);
  - window.rs's `p.yours` gates and the `Failed` arm;
  - crash.rs's `_TRANSPORT`, `.service` and COREDUMP_UID checks, each with its own forged case;
  - `notify()`'s id, key and deadline;
  - the model judged only at session start: re-judge it at apply, or say why the start is enough.

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

1. Rebase onto main once secrets has landed. That brings in the org move (`d7ec76d`: links, docs, release.yml,
   templates, branding, user-docs and torqcli's hints moved to torqosorg; `torq.releases.url` deliberately not moved,
   KNOWN-GAPS 6v).
   - The last rebase (onto 2ad0c75) had no conflicts.
   - **Expect a conflict in modules/agent-run.sh.** The branch rewrites it heavily (per-person accounts, `--link` in the
     sandbox, `/run/credentials` hidden, `--hold-frozen`), and main is fixing its chmod race separately.
   - The race is not fixed here: after the sandbox ends, root's `find … -exec chmod ug-s {} +` can be pointed by
     another process of that uid at a file of its choosing through a symlink swap (a denial of service, such as
     sudo's wrapper losing its bits). The branch only changed `-user torq-agent` to `-user "$agent"`; take main's fix.
2. Finish the second review's batch, as above.
3. Run the full check on .52, then land as a fast-forward with the owner (the lead session's plan).

Heavy work goes to .52 by default. On the laptop it runs throttled: one job, `nice -n 10 … --max-jobs 1 --cores 8`,
≥ 8 GB available to start, stop under 5 GB, no fat-LTO cosmic builds.

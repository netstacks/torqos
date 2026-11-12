# Design: approving what a person's agent does (ADR-051 and ADR-052 step 4)

**Status:** proposed, 2026-11-12, for the reviewer and the owner. Not decided, not built. Once the owner says
yes, it becomes an amendment to ADR-051 and ADR-052 in docs/DECISIONS.md, and this file goes.

**Fixed points (the owner's words):**
- "one agent per user".
- "whatever user is running the agent is the one that should be approving the tool calls for the agent,
  especially for secrets".
- They "need to be able to auto-approve or blanket approve, bypass permissions … for a regular user to an agent".
- "the agent should still be fenced off from making any changes to the system itself".
- A waiting use times out after 2 minutes. It is approved in the dialog or at a terminal.

**What exists:**
- Each person has one agent, which the broker sees as two accounts: `torq-agent-<p>` and `torq-resident-<p>`.
  - The broker judges and records both as `torq-agent-<p>`; `data.caller` says which one called.
  - Only the resident starts a session untainted. Every program run as `torq-agent-<p>` starts tainted
    (`not-the-resident-agent`), coding tools included.
- Secrets name a single `torq-agent`.
- Every agent use that needs a person is refused today ("approving is not built yet").
- Session trust (ADR-042 7): `request system agent trust <1–60>` takes a password. It covers only untainted
  proposals in the person's own folder, never the system tier, and is ledgered.

## 1. Who a rule's `actors` names

- **A person's own secrets** (`users/<p>/secrets.json`): the word `torq-agent` means *the person's own agent,
  both of its accounts*, and nothing else. A use is the agent's when the broker's map says the caller serves
  `<p>`: `torq-agent-<p>` or `torq-resident-<p>`, the same test it already uses for proposals. Another person's
  agent never matches, whatever its file says. The text of the rules doesn't change; only what it means is now
  exact.
  - Why both accounts: the person sees one agent (ADR-051 3, as amended). The difference that matters, the
    resident against a coding tool, is already taint (part 3), not a second name.
- **System secrets** (`secrets/system.json`, written by an administrator): the bare word `torq-agent` is
  refused, because it no longer says whose agent. The rule names the agents it allows one by one:
  `actors [ torq-agent-alice ]`.
  - Why: a system credential is the administrator's to lend. "Every agent" would hand it to agents of people
    added later, without anyone deciding to.
  - Naming a person (`actors [ sam ]`) stays as it is: sam's own use.
- **Rules that say `torq-agent` today:** a person's need no change. A system rule with it fails to parse and
  says what to write instead. There is no install base, so nothing is migrated by itself (the owner, 2026-11-12).

## 2. A use that needs a person

**Who is asked:** the person the agent serves (`<p>`), and only them. That holds for system secrets too: the
administrator decided the agent may use it at all, and the person running the agent approves each use. An
administrator is not asked in `<p>`'s place. The agent's accounts can never answer, because the broker checks
that the answer comes from `<p>`'s own uid (`SO_PEERCRED`).

**When a person is asked** (no grant covers the use, part 3):
- the rule says `each-use`;
- or it says `first-use`, and this is the first use in the session;
- or the use names no session;
- or the session is tainted (with part 3's exception).

**How:**
1. The daemon's `secret-use` waits at the broker, with no lock held, for at most **2 minutes**. The agent's
   HTTPS request or SSH signature waits with it.
2. The broker keeps the waiting use in its state directory under a number.
3. The panel applet shows it to `<p>`: a notification that opens a dialog with the secret's name, the
   operation, the host, whether the session read untrusted text, and the task's reason. The buttons are
   **Allow once**, **Allow for…** (part 3) and **Deny**.
4. At a terminal, `show system secrets waiting` and `request system secret approve <n>` (or `deny <n>`) do the
   same.
5. The broker takes the first answer. If no one answers within 2 minutes, the use is refused.

**What the dialog can be trusted to show:**
- First, what the broker knows for itself: the secret, the operation, the host, which account called
  (`torq-resident-<p>` or `torq-agent-<p>`, as the agent or as a coding tool), and whether the session is
  tainted.
- Below that, under a line saying it is the agent's text: the task's reason and anything else the agent sent.
  It is cut to one line of at most 200 characters, its control and bidirectional characters are taken out, and
  it is shown as plain text, never as markup.
- An injection can choose the words in that second part. It cannot change the first part, and the buttons act
  only on what the first part says.

**Flooding.** An injected agent could bury the person in dialogs until they click Allow, or fill the broker's
store of waiting uses. The limits:
- At most **3 uses wait per agent** at once. A fourth is refused at once: "3 uses are waiting for you
  already". The refusal is ledgered once per session, as refusals are now.
- **One dialog at a time.** It shows how many others wait ("2 more waiting"), and the next appears only after
  this one is answered or times out. Never more than 3 waiting, so never more than 3 dialogs in a row.
- **At most 10 asks per agent per session.** After that, every use that would ask is refused for the rest of
  the session, and the refusal says so.
- **Deny, and don't ask again this session:** an answer that refuses this secret's uses at once for the rest of
  the session, ledgered as `denied` with `for: session`.

**What is recorded:**
- The ledger gets each asked use once: who asked, which secret, the host, the session, and whether it was
  `approved`, `denied` or `timed-out`, with who answered and where (the dialog or the terminal). The value is
  never recorded.
- The daemon's own log records every use, as now (ADR-052 5).
- A use that no one is asked about is ledgered only as now: the agent's first use in each session.

A one-off answer needs no password, because the person's own secrets are already theirs to change without one
(`secret-set` is `Gate::Open`). A grant, which outlives the moment, does need one (part 3).

## 3. Blanket approval ("auto-approve, blanket, bypass")

**What a person can switch on, for their own agent only:**

| Scope | How | How long |
|---|---|---|
| One secret, this session | the dialog's "Allow for this session", or `request system secret allow <name> session` | until the session ends (at most its 8 hours) |
| One secret, for a time | "Allow for…" or `request system secret allow <name> for <minutes>` | 1 to 480 minutes |
| All their own secrets, for a time | `request system secret allow all for <minutes>` | 1 to 480 minutes |
| One secret, always | set the rule's `approve never`: a commit to their own rules, as today | until they change it; **not in a tainted session** (below) |

**The gap between "always" and taint, plainly:** `approve never` still asks in a tainted session. Under the
recommendation below, the only thing that holds under taint is a `tainted-too` grant, of at most 480 minutes.
A person using a coding tool, which is always tainted, would grant again every 8 hours at best.

- **Seen:** `show system secrets allowed` (each grant, its scope, its end), and later the Secrets page.
- **Switched off:** `request system secret allow revoke [<name>|all]`, the page's button, or at its end. Taking
  a grant away needs no password.
- **Granting** asks the person's own password, as `agent trust` does. Why: a grant outlives the dialog, and
  this keeps a program running as the person from granting quietly on their behalf.
- **Ledger:** the same entries as session trust, with what each covers: `trust_granted` with
  `covers: secret <name>|all-secrets`, plus `trust_revoked` and `trust_expired`. Each use under a grant names it
  in its entry, as an apply under trust does.

**What a grant never covers:**
- Anything that changes the system. That stays a proposal through the broker, applied by a person (system
  tier: their password, always). A grant covers secret uses, and nothing else.
- A system secret whose rule doesn't name this agent: a grant can't widen a rule. `secrets` stays on the
  agent's deny list (ADR-052 2).
- Another person's secrets, or another person's agent.
- A host or operation the rule doesn't name, or one not on ADR-052 3's closed list.
- `secret show` (reading a value back), and setting or deleting one.

**Taint, the owner's to decide:**
- **Recommended (amends ADR-052 4):** a grant holds in a tainted session only when it says so. A grant on
  **one named secret** can be made with `tainted-too`. The dialog shows that in red, the ledger records it, and
  `all` can never be tainted-too.
  - Why: every coding tool is tainted from its first request. Without this, "bypass" never reaches Claude Code's
    secret uses, which is what the owner asked for.
  - What still holds: the value goes only to the rule's hosts, and it is never in the agent's hands. A prompt
    injection can spend the credential only at those hosts, and only as far as the credential reaches there.
    So it stays on the person to keep the credential narrow (ADR-052 3).
- **Alternative 1 (ADR-052 4 as accepted):** taint always asks, whatever the grant. That is safer against
  injection, but under it a coding tool's secret uses can never be blanket-approved.
- **Alternative 2, standing:** a per-secret `approve never-even-tainted` in the person's own rules: a commit to
  their rules, asking their password, and shown in red wherever the secret is listed (`show system secrets`,
  the Secrets page, the dialog's history). It holds until they change it, so there is no regrant every 8 hours.
  It is never available for `all`. The host binding still holds, as above.

## 4. One approval mechanism, in two ADRs

- **ADR-051 owns the mechanism** (approving an agent generally):
  - the grant (who, what it covers, the scope, the end, the password, the ledger kinds);
  - the waiting ask (a number, the person's uid answers, the applet's dialog and the terminal's command);
  - the rule that the agent's accounts never answer.
- What uses it: session trust (proposals), secret grants, and later screen grants (ADR-051 6). For the
  resident's tool calls: the observe tools run without asking; `propose_change` is a proposal; a screen tool
  asks through `torq-agent-ui`.
- A coding tool's own permission prompts are its own, in the terminal of the person who started it. Its
  "skip permissions" mode stays allowed, because everything it changes in the system is only proposed.
- **ADR-052 uses it** for secret uses only: when a use asks (part 2), what a secret grant may cover (part 3),
  and the 2-minute wait.
- So one `Grant` type and one ledger family, `trust_*` with `covers`. Today's session trust becomes
  `covers: proposals` and is otherwise unchanged. Nothing is approval machinery of its own.

## 5. Build order, and the tests that change

1. **What `actors` means** (part 1): wire's parser and `may_use`.
   - Changed: wire's person and system rule tests, the `may_use` cases in broker/src/secrets.rs, and
     broker/tests/secrets.rs's two failing tests (their agent becomes `torq-agent-alice`, with a resident case
     added).
   - New: another person's agent never matches.
2. **The grant, generalised:** session trust gets `covers`; the commands get `allow`, `show … allowed` and
   `revoke`.
   - Changed: the trust tests in trust_loop and session.rs (the entries gain `covers`), and agent-proposals' own
     check of the ledger.
3. **The waiting ask:** the broker's waiting store, `secret-use`'s 2-minute wait, approve and deny at the
   socket (the person's uid only), the ledger entries.
   - Each "approving is not built yet" refusal becomes a wait.
   - New: the agent's accounts can't answer, a timeout refuses, the first answer wins.
   - New: the flooding limits at their exact numbers (3 waiting, 10 per session), "don't ask again", and the
     dialog's facts kept apart from the agent's text.
4. **The terminal's commands, then the applet's dialog:** grammar and completion tests; the desktop VM test
   gains an approval, done with the dialog in the VM.
5. **Taint as the owner chooses** (part 3), with a mutation test either way.
6. **The Secrets page shows and ends grants** (with the Settings stream). Docs in the same items: ADR-051 and
   ADR-052 amended, the guide, security.md, and KNOWN-GAPS.
   - tests/secrets.nix gains a use that asks, is approved and is recorded, and one that times out.

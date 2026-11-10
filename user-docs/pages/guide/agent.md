# The agent

Each person has an agent of their own: an account, `torq-agent-<you>` (alice's is `torq-agent-alice`),
its home `/var/lib/torq-agent-<you>`, which no other agent can read. It never changes your machine: it
*proposes* changes, and you apply them. It proposes packages only for you, never for anyone else, and
settings only as the owner's list `torq agent mayPropose settings` allows (none, by default). Who has
an agent is `torq agent people` (the owner, by default); `torq agent run` says so plainly if you have none.

!!! warning "Experimental"
    The agent's account, its proposals and its session trust exist and are tested, but no model
    ships with TorqOS, and no real model has ever been tried with it: the tests use a scripted
    stand-in. Whether a given model uses the tools well is unknown. Doctor mode is off by default.
    By default the agent may propose only the owner's own list of packages.

## Turn doctor mode on

Doctor mode is the part of the agent that answers questions. It is off until the owner turns it on,
as a recorded system change (`set torq agent doctor enable true`), and it needs a model to ask:

- a model file on this machine (`torq agent model url` and `hash`, which Nix fetches when the change is
  built, or `torq agent model path` for a file already on the disk), served by `llama-server`
  that runs from boot once on and holds the model in memory; or
- a remote endpoint (`torq agent doctor endpoint`). Then what the agent reads (logs, the
  configuration, which can contain a Wi-Fi password) leaves the machine, and it says so each time
  it starts.

The system journal is off limits to the agent unless `torq agent doctor journal true` is set. Turning
that on gives the whole agent account, and so every program run as it (Claude Code included), read
access to every service's log, so it is off by default whatever the model.

## Ask it

These are commands of your login shell, not of the `torqcli` prompt:

```text
alice@machine:~$ torq doctor "why is my wifi flaky?"        # reads and explains; proposes nothing
alice@machine:~$ torq agent propose "install htop for me"   # may also propose one change
```

Only an administrator can run them. Both run as your agent, in a sandbox, for that one question.
`torq agent propose` ends with a proposal number, or nothing.

## Ask your agent

Where doctor mode is on, your agent is resident: it keeps your conversation (its last few turns) and
answers from your terminal, on a server as on a desktop.

```text
alice@torq> torq ask "why is my wifi flaky?"
alice@torq> torq ask "install htop for me"            # ends with "Proposed #12" if it proposes
alice@torq> torq ask --forget                         # it forgets the conversation
alice@torq> request system agent forget                # the same, in torqcli (the Agent page's Forget)
```

The answer comes as it is written; what the agent read and called is shown on the way (standard
error), and each change it proposes by its number. **Ctrl-C** stops it — the whole task. One task at
a time. Only you can reach your agent: its socket is yours, and the agent answers no one else, root
included. What it remembers is kept where no program run as your agent (`torq agent run`) can read
or change it, and an answer from a session that read untrusted text is not kept.

## On the desktop: the panel, its window, and the Agent page

**In the panel**, beside TorqOS's own mark, is your agent's: a speech bubble, quiet while it is idle,
in the accent colour while it works, in the warning colour when something went wrong. Click it: it
says how the agent stands (idle and how much it remembers, working, not set up for you, not
reachable) and which model it asks — the local one, or **REMOTE** and where, when what it reads leaves
the machine. Type a question and press Enter: the answer comes below as it is written. Each change it
proposes is listed with **Review** (TorqOS Settings, at Proposals); **Stop** ends a task while it works,
wherever you asked it; **Open** opens its window. Looking does not start your agent; asking does.

**Its window**, *TorqOS Agent* in the menu, keeps the conversation of this sitting: each question, its
answer, the tools it called, and — said plainly — when its session read untrusted text (*tainted*),
so that what it proposed is marked so. **Stop**, **Forget…** (it asks first: your agent then forgets
what it remembers), and **Review** for each proposal. There is one window for you: opening it again
brings back the one that is open.

**The Agent page** of TorqOS Settings shows what your agent is: your agent account and whether its
service is on this machine, the model (local, or REMOTE with its endpoint), whether an API key is
configured (only yes or no — never the key), how many turns it remembers with **Forget…**, and buttons
to its window and to Proposals. Choosing the model is a change to the system like any other: the
page's buttons put the line in your draft (`set torq agent doctor endpoint …`, `set torq agent doctor
model …`, `delete torq agent doctor endpoint` for the local model again, `set torq agent people …` — which
adds a person, except the first time: until the list is written its default is you, the owner, and the first
`set` replaces that, so name yourself too), and
you commit it from *Your Changes* — recorded, undoable, with your approval. Entering the key itself is
not on this page yet: it comes with TorqOS's secrets; until then an administrator puts its file in
place (`torq agent doctor apiKeyFile`).

**Decline.** Beside **Review**, wherever a proposal waits for you — TorqOS Settings' Proposals page, the
agent's applet, its window — **Decline** turns it down: it shows you the proposal and asks first, and a
declined proposal is never applied (`request system decline <n>` in torqcli does the same).

### Ask from anywhere

- **Super+A** opens the agent's window. (It takes Super+A from COSMIC's App Library, which stays on the dock
  and in the launcher; change it in COSMIC Settings → Keyboard → Shortcuts.)
- **The launcher** (Super+/): type `ask why is my wifi slow?` and press Enter.
- **The terminal**: `ask why is my wifi slow?` (the same as `torq ask …`).
- **Ask the agent** beside an alarm, under a command that failed, in a setting's side panel and on your
  uncommitted changes in TorqOS Settings; on an alarm in TorqOS's applet; on a tunnel that will not come up in
  the SSH tunnels applet.
- **A crash**: when one of your programs crashes, a notification offers **Ask the agent about this crash**.
  Nothing about the crash is read or sent unless you click it.

Whatever these attach (the alarm, the error, the crash's summary) is shown above the question before it is
sent, and you can remove it. You press Ask. It is text anyone could have put there, so the agent's session is
marked tainted when it reads it, and so is anything it then proposes.

Every one of these is your own program: it talks only to your own agent, runs nothing as root, and
holds no secret.

## What it read: taint

Logs, device names, the ledger and configuration values are text anyone could have written, and a
line in a log can carry instructions meant for the agent. So each time the agent reads such text, it
tells the broker first, and the broker marks the agent's session **tainted** — for good: nothing
the agent can do clears it. A proposal from a tainted session is marked so, and so is a proposal
from a program run as the agent that keeps no session with the broker (Claude Code under
`torq agent run`, for example): TorqOS cannot see what such a program read.

This is a limit to know about. The broker holds only what it is told: TorqOS's own doctor reports
every read before the model sees it, but another program run as the agent can open a session and
never report, and its proposals then look untainted. What bounds that is the rest of the design:
trust covers only your own folder, lasts at most 60 minutes, needs your password, is recorded, and
never covers a system change.

## Apply a proposal

```text
alice@torq> show system proposals
alice@torq> request system apply 42
```

`request system apply` shows what the change does (as git writes it, not as the agent describes it)
and asks. For a tainted proposal it says what the session read, and you answer for that step alone.
A system change asks your password.

A change the agent proposes to the network or SSH, a service's unit or a file in `/etc`, the boot, the hardware, the console or the display could cut you
off. It is applied as a **commit confirmed**: it rolls itself back in 10 minutes unless you confirm
it (`configure`, then `commit`) — so if it cuts you off, you get your machine back by waiting.

## Decline a proposal

```text
alice@torq> request system decline 42
```

Not wanted? Decline it: it is never applied after, by anything — not by you, a commit at, or your trust — and it
no longer waits in `show system proposals`. Your agent's proposals, and anything wholly in your own folder, are
yours to decline without a password; anyone else's needs an administrator's password. The agent cannot decline
for you. It stays in the ledger as declined; to have it after all, ask for it again.

## Trust it for a while

```text
alice@torq> request system agent trust 30        # asks your password
alice@torq> request system agent trust revoke
```

For the next 30 minutes (at most 60), your agent's proposals **in your own folder** — your packages —
apply as soon as it makes them, without asking you. Never a system change, never a proposal from a
tainted session, never someone else's folder, and never another person's agent. Each one is its own change in the ledger, marked with
your trust, and `request system revert <n>` undoes it. While it holds, `show system alarms` says so.
Only you can give trust for your folder, and you can end it at any time.

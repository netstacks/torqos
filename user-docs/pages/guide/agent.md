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

Only an administrator can run them. Both run as your agent, in a sandbox, for that one question. `torq agent propose` ends with a
proposal number, or nothing. They run as any program of your agent's does (through `torq agent run`), not
as your resident agent, so what they propose is always marked tainted and waits for you: your trust (below)
never applies it. For that, ask your resident agent (`torq ask`).

## Ask your agent

Where doctor mode is on, your agent is resident: it keeps your conversation (its last few turns) and
answers from your terminal, on a server as on a desktop.

```text
alice@torq> torq ask "why is my wifi flaky?"
alice@torq> torq ask "install htop for me"            # ends with "Proposed #12" if it proposes
alice@torq> torq ask --forget                         # it forgets the conversation, and every task's record
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

**Decline.** Beside **Review**, wherever one of your agent's proposals waits for you — TorqOS Settings'
Proposals page, the agent's applet, its window — **Decline** turns it down: it shows you the proposal and
asks first, and a declined proposal is never applied (`request system decline <n>` in torqcli does the same,
also for other changes waiting for you, which may ask an administrator's password). A proposal whose apply
failed waits again, marked "its apply failed once — apply it again, or decline it".

### Ask from anywhere

- **Super+Shift+A** opens the agent's window (Super+A stays COSMIC's App Library; change either in COSMIC
  Settings → Keyboard → Shortcuts).
- **The launcher** (Super+/): type `ask why is my wifi slow?` and press Enter: the window opens with the
  question in its box, and you press Ask.
- **The terminal**: `ask` on its own, then type the question and press Enter — no quoting needed. With the
  question on the same line, quote it as you would any text the shell reads: `ask "why is $HOME full?"`.
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
the agent can do clears it. A proposal from a tainted session is marked so.

Only your **resident agent** — the one `torq ask`, the panel and its window talk to — starts with a
clean session: the broker checks, from the kernel, that the request comes from its own account and from inside
its service. It runs as an account of its own, `torq-resident-<you>`, hidden: what it remembers, its working
files and its model key are that account's, so nothing run as `torq-agent-<you>` can change or read them. Any
program run as your agent account (a coding tool such as Claude Code, `torq doctor`, `torq agent propose`,
anything under `torq agent run`) is tainted from its first request, because TorqOS cannot see what such a
program read; `request system apply` says so (`not-the-resident-agent`).

Your resident agent's sessions start clean only if its model is one no program on this machine could stand
in for: the local model TorqOS runs (llama-server, on a socket only its service writes) or a model reached over
`https`. A model on one of this machine's network ports (`http://127.0.0.1:11434`, a local Ollama) is not: any
account here can listen on another of the machine's addresses and answer in its place, so every session with
it is tainted (`local-tcp-model`) and your trust never applies what it proposes. To have a local server
trusted, give it a unix socket in a directory only root (or its own service) writes.

A remote model's key goes in a file only root reads, named by `torq agent doctor apiKeyFile`:

```text
alice@torq> sudo install -D -o root -g root -m 600 key.txt /var/lib/torq/secrets/model.key
alice@torq# set torq agent doctor apiKeyFile /var/lib/torq/secrets/model.key
```

Only your resident agent gets it (as its service's credential): no program run as your agent — Claude Code
included — can read it, and so `torq doctor` and `torq agent propose` cannot use a keyed model; they say so.
Ask with `torq ask`.

If the key file is missing, or is not root's alone, your resident agent does not start, and `torq ask` says
it did not answer. The reason is in its service's journal, which an administrator reads:

```text
alice@torq> sudo journalctl -u torq-agent@alice.service
… torq-agent: the model's key, /var/lib/torq/secrets/model.key, is missing …
```

## Claude Code, as your agent

Claude Code can run as your agent instead of as you: in your agent's account, seeing only the folder you
share with it, and changing the system only by proposing. An administrator chooses it (it is unfree, so
allowed first):

```text
alice@torq# set torq unfree packages claude-code
alice@torq# set torq agent tools claude-code
alice@torq# commit
```

(or the Agent page: **Allow Claude Code**, then **Install Claude Code for the agents**, then Your Changes).
Then start it in a folder of yours:

```text
alice@torq> torq agent run --share ~/projects/site -- claude
```

The first time, sign in there (your Claude subscription or an API key): the sign-in is kept in your agent's
home, never in yours. Each time it starts, TorqOS links its own tools into Claude Code — an MCP server named
`torqos` (read the machine's alarms, ledger, configuration, logs and hardware; propose a change) and a skill
on how this machine is configured — and leaves your other Claude Code settings as they were. Everything it
proposes, through those tools or with `torqcli` in its shell, is marked tainted and waits for you; your
trust never applies it. Its own "skip permissions" mode is safe to use for that reason: there is nothing it
can apply. The Agent page says whether it is installed for you, and how to start it. When it uses one of
your secrets you are asked each time, unless you allowed that ([When it uses a secret](#when-it-uses-a-secret)).

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
for you. It stays in the ledger as declined; to have it after all, ask for it again. A proposal whose apply
failed still waits (marked so in `show system proposals`): apply it again, or decline it.

## Trust it for a while

```text
alice@torq> request system agent trust 30        # asks your password
alice@torq> request system agent trust revoke
```

For the next 30 minutes (at most 60), your agent's proposals **in your own folder** — your packages —
apply as soon as it makes them, without asking you. Never a system change, never a proposal from a
tainted session — so only your resident agent's, never Claude Code's or `torq agent propose`'s — never someone else's folder, and never another person's agent. Each one is its own change in the ledger, marked with
your trust, and `request system revert <n>` undoes it. While it holds, `show system alarms` says so.
Only you can give trust for your folder, and you can end it at any time.

## When it uses a secret

Your agent can use a token or an SSH key that it never reads: TorqOS sends the request or signs for it
(see [Secrets](../reference/security.md#secrets-for-the-agent-and-programs)). You say which secrets it
may use, and when it must ask you first.

When a use asks, it waits two minutes for you, and only for you. On the desktop a notification says
"Your agent asks to use a secret"; click it, or click TorqOS in the panel, and answer there: **Allow
once**, **Allow for…**, **Deny**, or (for a use that belongs to a session) **Deny, and don't ask again this session**. The panel shows what
TorqOS knows first (which secret, which host, whether your agent or a coding tool asked), and the
agent's own words below, marked as the agent's. At a terminal, `show system secrets waiting` lists
what waits, and `request system secret approve <n>` or `deny <n>` answers it. With no answer the use
is refused.

A coding tool such as Claude Code asks every time, because TorqOS cannot see what it read. To let it
use one secret without asking for a while, see
[Letting your agent use a secret without asking](../reference/security.md#letting-your-agent-use-a-secret-without-asking):
it asks your password, and that page says what you are giving.

The panel's dialog has not been tried on a real machine yet. Your resident agent (the one `torq ask`
talks to) has no tool that uses a secret yet: today the uses come from coding tools and other programs
you run as your agent.

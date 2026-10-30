# Vision

**A laptop that is better than a MacBook Pro — and is mine, and only mine.**

## One sentence

The first laptop OS where a deeply capable AI can manage the whole system while the owner can
see, approve and undo everything it does — on a machine whose security is rooted in the owner's
own keys, not Microsoft's or Apple's.

## What "better" means here

| Goal | What we do |
|---|---|
| Trustworthy deep AI | AI can fix drivers, firmware, config, performance — through typed tools, a permission broker, and a tamper-evident ledger. Every change is a reviewable, reversible commit. |
| Everything reversible | The whole system is a declarative config in a local git repo. Every change (user, AI, update) is a commit + a bootable NixOS generation. |
| Owner-rooted security | Per-machine Secure Boot keys, Microsoft removed by default, measured boot, TPM2 + PIN LUKS. Set up for normal people by a guided "Secure this laptop" screen. |
| Private by default | Local models first. Cloud models are opt-in with a visible indicator and every call recorded. |
| Taste and refinement | Omarchy's spirit: opinionated, beautiful, one-step theming — but touchpad/touch-first, with measured smoothness targets. |
| Reproducible | Restore the config repo on a new laptop, rebuild, get the identical system. |
| Fun dev platform | It can break — because any break is undone in seconds. |

## Explicit non-goals (for now)

- Servers. This is a laptop/dev platform.
- Broad hardware support. One reference laptop first, then a short list.
- Running macOS apps (not legally/technically viable).
- Being minimal. We are opinionated and complete.

## Relationship to Omarchy

Omarchy 4 ("Quattro") is already agent-first: pre-wired coding-agent CLIs, a crash watcher that
briefs the agent, an experimental system-customization skill, agent usage tracking. TorqOS's
difference is **trust and control**:

- Omarchy's recovery for a bad agent edit is "reinstall configs"; ours is per-change undo.
- Omarchy agents act with the user's full powers; ours go through a broker with tiers.
- Omarchy tracks agent *usage*; we keep a tamper-evident ledger of every *action*.
- Omarchy's agents are mostly cloud CLIs; ours is local-first.
- Owner-key boot security is core to us and not Omarchy's focus.

We borrow Omarchy's ideas freely (crash watcher, agent skill, theming, curated defaults) and aim
to do each with a trust layer underneath.

# Kestrel

A laptop OS built on NixOS: **owner-rooted security**, **deep AI that is auditable and
reversible**, and **Omarchy-level taste** — touchpad-first.

The AI never changes the system directly. It proposes changes to the machine's config repo; a
small Rust broker applies them as commits that become bootable NixOS generations, records every
step in a tamper-evident ledger, and can undo any change in one command.

- Start here: [`docs/VISION.md`](docs/VISION.md) → [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
- Security: [`docs/SECURITY-MODEL.md`](docs/SECURITY-MODEL.md)
- AI layer: [`docs/AI-LAYER.md`](docs/AI-LAYER.md)
- Getting a dev VM running: [`docs/DEV-SETUP.md`](docs/DEV-SETUP.md)
- What's next: [`docs/ROADMAP.md`](docs/ROADMAP.md) · Why: [`docs/DECISIONS.md`](docs/DECISIONS.md)
- Working with Claude Code: [`CLAUDE.md`](CLAUDE.md)

Status: early scaffold (milestone M1, "the trust loop").

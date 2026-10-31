# The configuration is the device (proposal)

Status: **proposed** (2026-10-31), for discussion before anything is built. Becomes ADR-018 once
agreed.

## The idea

As on a router, one configuration defines the whole machine: what software it runs, every system
setting, every service, every user and what they may do. You change the machine only by changing
the configuration (`configure`, `set …`, `show | compare`, `commit`), so the machine is always
exactly what its configuration says, every change is recorded, and any change can be undone.

What the configuration does **not** hold is data: people's files and what applications save for
themselves (home folders), and what services accumulate (databases, container images, logs in
`/var`). A commit or a rollback never touches data.

| | Owned by | Changed with | Survives commit / rollback |
|---|---|---|---|
| Everything outside home folders: software, `/etc`, services, users, groups, boot | the configuration | torqcli (`commit`) | it *is* the configuration |
| Home folders: files, app settings saved by apps | each user | anything they like | never touched |
| Service data: databases, containers, logs | the services | the services | never touched |

## The tree: NixOS's own settings, one to one

Every NixOS setting is a path in the tree, dots becoming spaces. NixOS describes all of its
settings (name, type, default, description) in machine-readable form, so the tree is generated
from the machine's own NixOS version — TorqOS's settings included — and `?`, Tab, help and type
checking work for all of them, with nothing written by hand per service.

```
virtualisation.docker.enable = true;                        virtualisation docker enable true
services.httpd.enable = true;                               services httpd enable true
services.httpd.virtualHosts."example.com".forceSSL = true;  services httpd virtualHosts example.com forceSSL true
environment.systemPackages = [ pkgs.htop ];                 environment systemPackages [ htop ]
networking.firewall.allowedTCPPorts = [ 80 443 ];           networking firewall allowedTCPPorts [ 80 443 ]
```

| NixOS type | In the tree |
|---|---|
| true / false | `… enable true` (a bare `… enable` is `true`) |
| string, number, path, one of a list | a value; `?` lists the choices |
| list of values | `[ a b c ]`; `set` adds, `delete` removes one |
| named entries (`virtualHosts."example.com"`, `users.users.viper`) | list entries, as `users viper` is today |
| a package | its name (`htop`), checked against this machine's nixpkgs |
| multi-line text (`extraConfig`) | a quoted value, or a file (below) |
| Nix code (functions, expressions) | not a value: a `config` file (below) |

So any NixOS guide or answer applies directly; nothing new to learn beyond the translation.

**Names and quoting.** Nix quotes `"example.com"` only because dots separate its setting names; in
the tree spaces do, so `example.com` is one word, and under a level of named entries (`virtualHosts`,
`users`) any word is an entry name. Quotes are for spaces and the shell's own characters
(`"My Site"`, a value with `|` or `;`), shell-style, as torqcli reads them today.

**No variables, at first.** Every value is literal, so `show | compare` and an approval prompt say
exactly what changes — nobody approves a puzzle, least of all an AI's proposal. When repetition
hurts, Junos-style `groups` (a block applied in many places, `apply-groups`, with
`| display inheritance` to show the result expanded) cover most of what variables are used for.
Computed values and references between settings belong in a `config` file: plain Nix.

## Files for what is not a value

At any point in the tree, `config <file>` attaches an ordinary NixOS module for that part:

```
systemd services myapp config ./custom_apps/myapp.nix
services httpd config ./services/httpd.nix
```

- The file is plain NixOS: everything NixOS can do works, including what the tree cannot express.
- It must be inside the config repo, so it is versioned, ledgered and rolled back with the rest. A
  path outside the repo would make the machine depend on something nothing tracks: refused.
- Humans edit it through the candidate: `edit services httpd config` opens it in your editor, and
  `show | compare` shows its raw diff before `commit` — the same flexibility, plus undo.
- `show configuration` names every attached file, so nothing that defines the machine is hidden.

## TorqOS's layer on top

**One tree, all generated.** NixOS already has top-level `system.*`, `services.*` and `users.*`, the
names TorqOS's first hand-made tree used. So nothing is hand-made: the whole tree is generated from
the machine's NixOS options, and TorqOS's own features are NixOS options under `torq.*` (as
`torq.user` and `torq.dev.enable` already are), shown as `torq …`. Today's names become their NixOS
equivalents (`system host-name` → `networking hostName`, `system packages` → `environment
systemPackages`, `services ssh` → `services openssh …` with TorqOS's defaults); a few short
aliases may stay where they help. NixOS's documentation file describes types in words, so the
generator reads NixOS's type objects and writes its own structured file (measured: 24,603 settings
on this machine, generated in ~6 s; the documentation file is 12 MB).


A few TorqOS branches add safety and ease where raw NixOS is awkward; they set NixOS settings
underneath and show what they set (`| display detail`).

```
system host-name torq-utm                     (networking hostName)
user cwdavis applications [ htop cowsay ]     (that user's own programs)
user cwdavis group [ admin ]
user group admin system read-write            (may approve system changes)
user group users1 system read                 (sees the configuration; commits only their own folder)
user group users1 applications [ htop ]
```

- **Applications vs services** are told apart from NixOS itself: a service is a NixOS `services.*`
  (or `virtualisation.*`, `systemd.services.*`) module that runs for the machine; an application
  is a package a person runs. `?` offers each where it belongs.
- **TorqOS defaults stay defaults** (SSH keys only, no root login, firewall on): an explicit
  setting wins, as in NixOS, but `commit` says plainly when one weakens a TorqOS default ("this
  allows password logins over SSH").
- An application's own settings are either its state (what it saves when you click, as today) or,
  per user and per application, a declared file (`user cwdavis applications htop config …`, using
  home-manager, NixOS's per-user tool).

## Humans free, the AI fenced — permanently

- The AI runs as **its own account**, sandboxed, without `sudo`. It cannot write the configuration
  or any system file; it can only *propose* a change to the broker.
- The broker learns who is calling from the kernel (not from anything the caller says) and applies
  a policy to AI proposals: whole areas it may never propose (users, groups, permissions, boot,
  security, the broker and its policy), and a person approves every one it may.
- Humans keep full power, root included; nothing here limits them. The read-only system files are
  about determinism (the machine always matches its configuration), not about the AI.
- This closes KNOWN-GAPS #4 (today, anything running in your terminal acts as you). Planned for M3;
  nothing above has to wait for it.

## What the config repo becomes

Data files for the tree, `flake.lock`, and any attached `config` files. `flake.nix`, `machine.nix`
and `system.nix` become fixed TorqOS files nobody edits. Hardware (`hardware-configuration.nix`,
really data: disks, kernel modules) becomes a `hardware` branch, detected at install. Software
sources (the flake's inputs) become `system software …`, like choosing a firmware train. Always
deterministic: the same configuration and the same lock build the same system.

This machine today, moved over without changing what it does:

```
torq.user = "viper";              →  user viper group [ admin ]      (and the machine's owner)
torq.dev.enable = true;           →  torq dev enable true            (visible, and flagged)
services.openssh.enable = true;   →  services openssh enable true    (flagged: allows passwords)
torq.desktop.enable = true;       →  torq desktop enable true
```

## Open questions

1. **Size.** NixOS has ~20,000 settings; the generated description is tens of MB. Measure the
   cost of building it with the system and of torqcli reading it.
2. **How far the AI's fences go**, area by area.
3. **Today's `services ssh` catalog**: keep as a TorqOS shortcut, or fold into `services openssh`
   with TorqOS's defaults and warnings.
4. **Unmanaged mode**: whether anyone needs to edit the repo outside torqcli at all, given
   `config` files and `edit`.

## How it would be built (each step reviewed, tested, shipped through the broker)

1. Generate the tree's schema from the machine's NixOS options, installed with the system; read it
   in torqcli (help, completion, types). **Built (S1):** modules/nixos-schema.nix writes
   /etc/torq/nixos-schema.json (all 24,603 settings here, 4.4 MB, ~9 s more per system build);
   cli/src/nixos.rs reads it (17 ms), resolves paths with named entries, checks values against
   NixOS's types, offers what may come next. Not yet editable in `configure` (S3).
2. One generic module applies the tree's values to NixOS; values stored as data in the repo.
   **Built (S2):** `nixos/<top-level>.json` in the config repo, shaped like the options
   (`nixos/virtualisation.json`: `{ "docker": { "enable": true } }`); modules/nixos-settings.nix
   gives them to NixOS as they are (package names become packages), built by `mkSystem` from the
   path itself; NixOS's types check every value. All system tier at the broker.
3. `config <file>` and `edit`, through the candidate and `commit`.
4. The TorqOS layer: users, groups, applications, permissions; TorqOS-default warnings at commit.
5. `hardware` and `system software` branches; fixed `flake.nix`/`machine.nix`; migrate this machine.
6. `save` / `load override|merge|set`: a saved configuration recreates a device.

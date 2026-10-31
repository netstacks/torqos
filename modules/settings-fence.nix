# What the configuration tree may never change (docs/CONFIG-IS-THE-DEVICE.md). Enforced when the
# system is evaluated (modules/nixos-settings.nix), so a configuration whose tree breaks it does not
# build, whoever proposed it; exported in /etc/torq/nixos-schema.json for torqcli to say so early.
#
# Two rules:
#  1. The tree is data. A key starting with `_` (NixOS's `_type` turns a set into mkMerge, mkForce,
#     mkIf…; `_module` is the module system's own) is refused anywhere, lists included.
#  2. The broker's own inputs are not the tree's: the commands and paths it runs as root, what its
#     settings file and PATH are built from, its units, its files in /etc (by where they land, not by
#     their key), and plumbing that could swap any package.
#
# This is not the boundary for the AI. Settings that run code as root (a service's script, sudo
# rules, wrappers, tmpfiles, binary caches, groups) are ordinary system-tier power when a person
# approves them; what an agent may propose at all is a default-deny allow-list at the broker, keyed on
# its identity (KNOWN-GAPS #4, M3). Changing what is fenced here is a change to TorqOS itself, or a
# reviewed .nix file.
#
# A pattern is a path; a segment ending in `*` matches any entry name starting with what precedes it.
{ lib }:

rec {
  patterns = [
    [ "torq" "broker" ]
    [ "torq" "packages" ]
    [ "torq" "cli" "package" ]
    [ "torq" "schema" ]
    [ "torq" "nixosSchema" ]
    [ "nixpkgs" ]
    [ "nix" "package" ]
    [ "systemd" "package" ]
    [ "system" "build" ]
    [ "environment" "etc" "torq/*" ]
    [ "environment" "etc" "torq" ]
    [ "systemd" "services" "torq-*" ]
    [ "systemd" "timers" "torq-*" ]
    [ "systemd" "units" "torq-*" ]
  ];

  # The settings TorqOS keeps in files of its own (torqcli's whole.rs MAP): one setting, one place —
  # set in nixos/ too, the tree would show one value and NixOS build another.
  elsewhere = [
    [ "networking" "hostName" ]
    [ "time" "timeZone" ]
    [ "torq" "cli" "style" ]
    [ "torq" "cli" "userStyles" ]
    [ "torq" "services" "enabled" ]
  ];

  segmentMatches = pat: seg:
    if lib.hasSuffix "*" pat then lib.hasPrefix (lib.removeSuffix "*" pat) seg else pat == seg;

  # Whether `path` (a list of words) is, or is below, a fenced setting.
  fenced = path: builtins.any (pat:
    builtins.length path >= builtins.length pat
    && builtins.all lib.id (lib.zipListsWith segmentMatches pat (lib.take (builtins.length pat) path))
  ) patterns;

  # Where an /etc entry lands: its `target`, else its name, without a leading `/` or `./`.
  etcTarget = name: e:
    let
      t = if builtins.isAttrs e && builtins.isString (e.target or null) then e.target else name;
      strip = s: if lib.hasPrefix "/" s then strip (lib.removePrefix "/" s) else if lib.hasPrefix "./" s then strip (lib.removePrefix "./" s) else s;
    in strip t;

  # The first problem in a value set under `path`: a key starting with `_`, a fenced setting, or an
  # /etc entry landing in torq/. null if none.
  problem = path: v:
    let
      here = lib.concatStringsSep "." path;
      underscore = lib.findFirst (k: lib.hasPrefix "_" k) null (if builtins.isAttrs v then builtins.attrNames v else [ ]);
      etc = path == [ "environment" "etc" ] && builtins.isAttrs v;
      landsInTorq = lib.findFirst (k: let t = etcTarget k v.${k}; in t == "torq" || lib.hasPrefix "torq/" t) null
        (if etc then builtins.attrNames v else [ ]);
      inner = if builtins.isAttrs v then lib.mapAttrsToList (k: x: problem (path ++ [ k ]) x) v
        else if builtins.isList v then lib.imap0 (i: x: problem (path ++ [ "[${toString i}]" ]) x) v
        else [ ];
    in
    if underscore != null then "${here}.${underscore}: a key starting with _ is a NixOS module directive (mkForce, mkMerge, …), not a setting: the tree holds data only"
    else if builtins.elem path elsewhere then "${here}: kept in its own file by TorqOS (system/host.json, system/cli.json, users/<name>/cli.json, services.json), not in nixos/"
    else if fenced path then "${here}: not settable in the configuration tree (it reaches the broker or TorqOS itself)"
    else if landsInTorq != null then "environment.etc.${landsInTorq}: lands in /etc/torq, the broker's: not settable in the configuration tree"
    else lib.findFirst (p: p != null) null inner;
}

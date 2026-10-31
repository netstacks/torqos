# The settings the configuration tree may never change (docs/CONFIG-IS-THE-DEVICE.md): what reaches
# the broker's own guard — the commands and paths it runs as root, the file it reads them from, its
# units — and plumbing (`nixpkgs` could swap any package, the broker's too). Enforced when the system
# is evaluated (modules/nixos-settings.nix), so whoever proposed a change, a configuration whose tree
# touches one of these does not build; exported in /etc/torq/nixos-schema.json for torqcli to say so
# early and show them read-only. Changing them is a change to TorqOS itself, or a reviewed .nix file.
#
# A pattern is a path; a segment ending in `*` matches any entry name starting with what precedes it.
{ lib }:

rec {
  patterns = [
    [ "torq" "broker" ]
    [ "torq" "packages" "configDir" ]
    [ "torq" "schema" ]
    [ "torq" "nixosSchema" ]
    [ "_module" ]
    [ "nixpkgs" ]
    [ "environment" "etc" "torq/*" ]
    [ "systemd" "services" "torq-*" ]
    [ "systemd" "timers" "torq-*" ]
  ];

  segmentMatches = pat: seg:
    if lib.hasSuffix "*" pat then lib.hasPrefix (lib.removeSuffix "*" pat) seg else pat == seg;

  # Whether `path` (a list of words) is, or is below, a fenced setting.
  fenced = path: builtins.any (pat:
    builtins.length path >= builtins.length pat
    && builtins.all lib.id (lib.zipListsWith segmentMatches pat (lib.take (builtins.length pat) path))
  ) patterns;

  # The first fenced path in a value set under `path`, or null.
  firstFenced = path: v:
    if fenced path then path
    else if builtins.isAttrs v then
      lib.findFirst (p: p != null) null (lib.mapAttrsToList (k: x: firstFenced (path ++ [ k ]) x) v)
    else null;
}

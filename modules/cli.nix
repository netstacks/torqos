# Which syntax `torqcli` speaks (ADR-016): `system cli style juniper|cisco` for the machine, and
# `users <name> cli style juniper|cisco` for one user, stored in the config repo as
#
#   system/cli.json          { "style": "cisco" }      (system tier)
#   users/<name>/cli.json    { "style": "juniper" }    (user tier: the broker's USER_LEAVES)
#
# Exactly that object, nothing else: the broker checks the user file's shape before accepting it
# (broker/src/policy.rs, user_leaf_ok) and this module is the authority at evaluation. The result is
# installed as /etc/torq/cli.json for torqcli to read: { "style": ..., "users": { <name>: ... } }.
# `switch cli` in the shell changes only the running session.
{ config, lib, ... }:

let
  cfg = config.torq.cli;
  dir = config.torq.packages.configDir;
  styles = [ "juniper" "cisco" ];

  readStyle = where: file:
    let v = lib.importJSON file; in
    if builtins.isAttrs v && lib.attrNames v == [ "style" ] && builtins.elem v.style styles then
      v.style
    else
      throw "torq: ${where} must be exactly { \"style\": \"juniper\" } or { \"style\": \"cisco\" }";

  systemFile = dir + "/system/cli.json";
  usersDir = dir + "/users";
  # users/<name>/ folders with a cli.json. That the folder names a real user, and holds only
  # allow-listed regular files, is checked in modules/packages.nix for every users/<name>/.
  userFiles =
    if dir != null && builtins.pathExists usersDir then
      lib.filterAttrs (name: type: type == "directory" && builtins.pathExists (usersDir + "/${name}/cli.json"))
        (builtins.readDir usersDir)
    else
      { };
in
{
  options.torq.cli = {
    style = lib.mkOption {
      type = lib.types.enum styles;
      default = "juniper";
      description = "The syntax torqcli speaks on this machine (system/cli.json).";
    };
    userStyles = lib.mkOption {
      type = lib.types.attrsOf (lib.types.enum styles);
      default = { };
      description = "Per-user torqcli syntax, overriding `style` (users/<name>/cli.json).";
    };
  };

  config = {
    torq.cli.style = lib.mkIf (dir != null && builtins.pathExists systemFile) (readStyle "system/cli.json" systemFile);
    torq.cli.userStyles = lib.mapAttrs (name: _: readStyle "users/${name}/cli.json" (usersDir + "/${name}/cli.json")) userFiles;
    environment.etc."torq/cli.json".text = builtins.toJSON { inherit (cfg) style; users = cfg.userStyles; };
  };
}

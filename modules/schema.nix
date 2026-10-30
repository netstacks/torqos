# The configuration tree's schema (ADR-016, docs/CLI-DESIGN.md): what torqcli knows about each
# setting, for parsing, `?` help, Tab completion and early errors. Each setting's module declares its
# own nodes next to the code that reads its file (`torq.schema.<path>`); the result is installed as
# /etc/torq/schema.json, so it always describes the *running* system.
#
# The schema is not the authority: the modules' checks are, at evaluation, whoever wrote the file.
#
# A node is keyed by its path in the tree, words separated by spaces, `<name>` for an entry of a
# list ("users <name> cli style"). Fields:
#   kind     container | list (entries named by the user, e.g. users) | leaf | leaf-list
#   help     one line for `?`
#   file     leaf, leaf-list: the config repo file holding it; `<name>` as in the path
#   key      leaf: its key in that file's JSON object (a leaf-list is the whole file, a JSON list)
#   tier     system | user (what the broker will call a change to it)
#   values   the allowed values, if a closed set: { value = "help"; } (completion and checking)
#   type     otherwise what a value must look like: package | host-name (checked by the module)
{ config, lib, ... }:

let
  node = lib.types.submodule {
    options = {
      kind = lib.mkOption { type = lib.types.enum [ "container" "list" "leaf" "leaf-list" ]; };
      help = lib.mkOption { type = lib.types.str; };
      file = lib.mkOption { type = lib.types.nullOr lib.types.str; default = null; };
      key = lib.mkOption { type = lib.types.nullOr lib.types.str; default = null; };
      tier = lib.mkOption { type = lib.types.enum [ "system" "user" ]; default = "system"; };
      values = lib.mkOption { type = lib.types.nullOr (lib.types.attrsOf lib.types.str); default = null; };
      type = lib.mkOption { type = lib.types.nullOr (lib.types.enum [ "package" "host-name" ]); default = null; };
    };
  };
  clean = lib.mapAttrs (_: n: lib.filterAttrs (_: v: v != null) n);
in
{
  options.torq.schema = lib.mkOption {
    type = lib.types.attrsOf node;
    default = { };
    description = "The configuration tree, for torqcli (installed as /etc/torq/schema.json).";
  };

  config = {
    # The containers no single setting owns.
    torq.schema = {
      system = { kind = "container"; help = "This machine"; };
      users = {
        kind = "list";
        help = "People with an account on this machine";
        # The machine's normal users: the only names `users <name>` may take.
        values = lib.mapAttrs (name: u: u.description) (lib.filterAttrs (_: u: u.isNormalUser) config.users.users);
      };
      "users <name>" = { kind = "container"; help = "One person's own settings"; tier = "user"; };
    };

    environment.etc."torq/schema.json".text = builtins.toJSON {
      version = 1;
      nodes = clean config.torq.schema;
    };
  };
}

# Passwords (docs/INSTALL.md; SECURITY-MODEL.md, "Accounts and passwords"): never in the config repo,
# which every user can read and which keeps its history forever (a hash there can be cracked
# offline). The configuration owns the accounts (users.mutableUsers = false); each person's password
# is a hash in a file only the system reads, /var/lib/torq/secrets/passwords/<user>, that their
# account points at. No file: no password — an account that logs in by key only, or not at all.
# First-boot setup writes the owner's; `request system password` changes one (ledgered, never the hash).
{ lib, ... }:

let
  dir = "/var/lib/torq/secrets/passwords";
in
{
  # Every person's account (isNormalUser) points at its file, unless the tree says otherwise.
  options.users.users = lib.mkOption {
    type = lib.types.attrsOf (lib.types.submodule ({ name, config, ... }: {
      config.hashedPasswordFile = lib.mkIf config.isNormalUser (lib.mkDefault "${dir}/${name}");
    }));
  };

  config = {
    users.mutableUsers = false;
    systemd.tmpfiles.rules = [
      "d /var/lib/torq/secrets 0700 root root -"
      "d ${dir} 0700 root root -"
    ];
  };
}

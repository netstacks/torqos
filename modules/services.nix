# Services, as data. Like packages (ADR-011): the config repo holds `services.json`, a JSON list
# of names from the catalog below (system tier), and `set services <name>` / `delete services <name>` in torqcli change it
# through the broker. Each catalog entry is a service *with TorqOS's defaults* — "SSH, safely",
# not every option the NixOS module has — and says what enabling it implies, because the diff a
# person approves is just `+ "name"`. Anything beyond the catalog is a system-tier .nix change.
{ config, lib, ... }:

let
  cfg = config.torq.services;
  dir = config.torq.packages.configDir;
  owner = if config.torq.user == null then "the owner" else config.torq.user;
  ssh = config.services.openssh;
  # SSH by password too (first-boot setup's choice, or `set torq ssh passwords true`): protected.
  passwords = config.torq.ssh.passwords;

  catalog = {
    ssh = {
      description = if passwords
        then "SSH server: passwords and keys (repeated wrong guesses blocked, 3 tries a connection), no root login, only ${owner} may log in; opens TCP 22 on every network"
        else "SSH server: keys only, no root login, only ${owner} may log in; opens TCP 22 on every network";
      config = {
        services.openssh = {
          enable = true;
          settings = {
            PasswordAuthentication = passwords;
            KbdInteractiveAuthentication = false;
            PermitRootLogin = "no";
            AllowUsers = [ owner ];
            MaxAuthTries = 3;
          };
        };
        # Password guessing: an address that keeps failing is blocked for a while.
        services.sshguard.enable = passwords;
      };
      # The catalog's promise must hold: a stronger definition elsewhere (lib.mkForce) would
      # otherwise silently undo it while `show services` still says "keys only".
      holds = ssh.settings.PasswordAuthentication == passwords
        && ssh.settings.KbdInteractiveAuthentication == false
        && ssh.settings.PermitRootLogin == "no"
        && ssh.settings.AllowUsers == [ owner ];
    };
    podman = {
      description = "Containers: rootless Podman for each user, with a `docker` command (no root API socket)";
      config = {
        virtualisation.podman = {
          enable = true;
          dockerCompat = true;
          defaultNetwork.settings.dns_enabled = true;
        };
        # The module also enables the rootful API socket (group `podman`, root-equivalent): rootless
        # use does not need it, so it stays off.
        systemd.sockets.podman.wantedBy = lib.mkForce [ ];
      };
      holds = true;
    };
    tailscale = {
      description = "Tailscale VPN (then `sudo tailscale up`); its own settings, e.g. Tailscale SSH, live outside the ledger";
      config.services.tailscale.enable = true;
      holds = true;
    };
    printing = {
      description = "Printing (CUPS, on localhost only)";
      config.services.printing.enable = true;
      holds = config.services.printing.listenAddresses == [ "localhost:631" ];
    };
    # Not here yet: fwupd. Firmware updates must go through the broker (SECURITY-MODEL.md), and
    # fwupd applies them itself via polkit (KNOWN-GAPS).
  };

  file = dir + "/services.json";
  fromFile = if dir != null && builtins.pathExists file then lib.importJSON file else [ ];
in
{
  options.torq.ssh.passwords = lib.mkOption {
    type = lib.types.bool;
    default = false;
    description = "The catalog's SSH accepts passwords as well as keys (repeated wrong guesses are blocked). Off: keys only.";
  };

  options.torq.services = {
    enabled = lib.mkOption {
      type = lib.types.listOf (lib.types.enum (lib.attrNames catalog));
      default = [ ];
      description = "Catalog services to enable. Read from the config repo's services.json only.";
    };
    catalog = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      readOnly = true;
      default = lib.mapAttrs (_: s: s.description) catalog;
      description = "The services TorqOS offers (torqcli: `show services`), with what each implies (read-only).";
    };
  };

  config = lib.mkMerge (
    [
      {
        torq.services.enabled = lib.mkIf (dir != null && builtins.pathExists file) fromFile;

        torq.schema.services = {
          kind = "leaf-list";
          help = "Services from the TorqOS catalog, each with TorqOS's defaults";
          file = "services.json";
          values = cfg.catalog;
        };

        assertions =
          [
            {
              # One source of truth: any other definition (added to the list, replacing it with
              # mkForce, or set where there is no file) would make `show services` disagree with
              # what is running. Compare the result, not the number of definitions.
              assertion = cfg.enabled == fromFile;
              message = "torq.services.enabled is set from services.json only (torqcli: `set services <name>`), not in Nix.";
            }
          ]
          ++ lib.mapAttrsToList (name: s: {
            assertion = !(lib.elem name cfg.enabled) || s.holds;
            message = "TorqOS service '${name}' is enabled, but another definition overrides its TorqOS defaults.";
          }) catalog;

        warnings = lib.optional (ssh.enable && !(lib.elem "ssh" cfg.enabled) && ssh.settings.PasswordAuthentication != false)
          "SSH is enabled outside TorqOS's service catalog, with password logins allowed. Use `set services ssh` in torqcli (keys only) instead of services.openssh.enable.";
      }
    ]
    ++ lib.mapAttrsToList (name: s: lib.mkIf (lib.elem name cfg.enabled) s.config) catalog
  );
}

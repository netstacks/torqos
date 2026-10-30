# Services, as data. Like packages (ADR-011): the config repo holds `services.json`, a JSON list
# of names from the catalog below (system tier), and `kestrel service enable|disable` changes it
# through the broker. Each catalog entry is a service *with Kestrel's defaults* — "SSH, safely",
# not every option the NixOS module has — and says what enabling it implies, because the diff a
# person approves is just `+ "name"`. Anything beyond the catalog is a system-tier .nix change.
{ config, lib, ... }:

let
  cfg = config.kestrel.services;
  dir = config.kestrel.packages.configDir;
  owner = config.kestrel.user;
  ssh = config.services.openssh;

  catalog = {
    ssh = {
      description = "SSH server: keys only, no root login, only ${owner} may log in; opens TCP 22 on every network";
      config.services.openssh = {
        enable = true;
        settings = {
          PasswordAuthentication = false;
          KbdInteractiveAuthentication = false;
          PermitRootLogin = "no";
          AllowUsers = [ owner ];
        };
      };
      # The catalog's promise must hold: a stronger definition elsewhere (lib.mkForce) would
      # otherwise silently undo it while `kestrel service list` still says "keys only".
      holds = ssh.settings.PasswordAuthentication == false
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
  options.kestrel.services = {
    enabled = lib.mkOption {
      type = lib.types.listOf (lib.types.enum (lib.attrNames catalog));
      default = [ ];
      description = "Catalog services to enable. Read from the config repo's services.json only.";
    };
    catalog = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      readOnly = true;
      default = lib.mapAttrs (_: s: s.description) catalog;
      description = "The services `kestrel service` offers, with what each implies (read-only).";
    };
  };

  config = lib.mkMerge (
    [
      {
        kestrel.services.enabled = lib.mkIf (dir != null && builtins.pathExists file) fromFile;

        assertions =
          [
            {
              # One source of truth: any other definition (added to the list, replacing it with
              # mkForce, or set where there is no file) would make `kestrel service` disagree with
              # what is running. Compare the result, not the number of definitions.
              assertion = cfg.enabled == fromFile;
              message = "kestrel.services.enabled is set from services.json only (`kestrel service enable`), not in Nix.";
            }
          ]
          ++ lib.mapAttrsToList (name: s: {
            assertion = !(lib.elem name cfg.enabled) || s.holds;
            message = "kestrel service '${name}' is enabled, but another definition overrides its Kestrel defaults.";
          }) catalog;

        warnings = lib.optional (ssh.enable && !(lib.elem "ssh" cfg.enabled) && ssh.settings.PasswordAuthentication != false)
          "SSH is enabled outside Kestrel's service catalog, with password logins allowed. Use `kestrel service enable ssh` (keys only) instead of services.openssh.enable.";
      }
    ]
    ++ lib.mapAttrsToList (name: s: lib.mkIf (lib.elem name cfg.enabled) s.config) catalog
  );
}

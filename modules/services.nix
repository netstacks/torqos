# Services, as data. Like packages (ADR-011): the config repo holds `services.json`, a JSON list
# of names from the catalog below (system tier), and `kestrel service enable|disable` changes it
# through the broker. Each catalog entry is a service *with Kestrel's defaults* — "SSH, safely",
# not every option the NixOS module has. Anything beyond the catalog is a system-tier .nix change.
{ config, lib, pkgs, ... }:

let
  cfg = config.kestrel.services;
  dir = config.kestrel.packages.configDir;

  catalog = {
    ssh = {
      description = "SSH server: keys only, no root login";
      config.services.openssh = {
        enable = true;
        settings = {
          PasswordAuthentication = false;
          KbdInteractiveAuthentication = false;
          PermitRootLogin = "no";
        };
      };
    };
    podman = {
      description = "Containers (rootless Podman, with a `docker` command)";
      config.virtualisation.podman = {
        enable = true;
        dockerCompat = true;
        defaultNetwork.settings.dns_enabled = true;
      };
    };
    tailscale = {
      description = "Tailscale VPN (then `sudo tailscale up` to log in)";
      config.services.tailscale.enable = true;
    };
    printing = {
      description = "Printing (CUPS)";
      config.services.printing.enable = true;
    };
    fwupd = {
      description = "Firmware updates (fwupd)";
      config.services.fwupd.enable = true;
    };
  };

  file = dir + "/services.json";
in
{
  options.kestrel.services = {
    enabled = lib.mkOption {
      type = lib.types.listOf (lib.types.enum (lib.attrNames catalog));
      default = [ ];
      description = "Catalog services to enable. Read from the config repo's services.json.";
    };
    catalog = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      readOnly = true;
      default = lib.mapAttrs (_: s: s.description) catalog;
      description = "The services `kestrel service` offers, with what each does (read-only).";
    };
  };

  config = lib.mkMerge (
    [
      {
        kestrel.services.enabled = lib.mkIf (dir != null && builtins.pathExists file) (lib.importJSON file);
      }
    ]
    ++ lib.mapAttrsToList (name: s: lib.mkIf (lib.elem name cfg.enabled) s.config) catalog
  );
}

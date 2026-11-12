# TorqOS's releases (docs/DECISIONS.md, ADR-028): where a machine updates from, and the key a
# release must be signed with. A machine's update moves it only to a commit of `url` that this key
# signed (`request system update`, torqcli's release.rs); the same key signs each release's
# checksums (docs/INSTALL.md). Not the tree's to change (settings-fence.nix): which key may decide
# what every update installs is TorqOS's own.
{ config, lib, ... }:

let
  cfg = config.torq.releases;
in
{
  options.torq.releases = {
    url = lib.mkOption {
      type = lib.types.strMatching "git\\+https://[^?]+\\?ref=[A-Za-z0-9._/-]+";
      default = "git+https://github.com/torqosorg/torqos-releases?ref=stable";
      description = "The releases, as a flake reference: a git repository over HTTPS, and the branch machines follow.";
    };
    former = lib.mkOption {
      type = lib.types.listOf (lib.types.strMatching "https://[^?]+");
      # Until 2026-11-11 the releases were netstacks/torqos-releases (ADR-028, amended 2026-11-12): a machine installed
      # from 0.3.2 or before still names that address. It is the same repository under its old name, so a
      # source there is the releases, its commits checked against the release key; `request system update`
      # moves such a source to `url`.
      default = [ "https://github.com/netstacks/torqos-releases" ];
      description = "Earlier addresses of the releases (https://…): a source at one is the releases too, checked against the release key, and moved to `url` by the next update.";
    };
    signers = lib.mkOption {
      type = lib.types.path;
      default = ../release/allowed_signers;
      description = "The keys a release may be signed with (OpenSSH's allowed-signers format, principal `torqos-release`).";
    };
  };

  config.environment.etc = {
    "torq/releases.json".text = builtins.toJSON { inherit (cfg) url former; };
    "torq/release-signers".source = cfg.signers;
  };
}

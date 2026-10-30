{ config, lib, pkgs, ... }:

let
  cfg = config.kestrel.broker;
in
{
  options.kestrel.broker = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "The Kestrel change broker (sole writer of the config repo).";
    };
    package = lib.mkOption {
      type = lib.types.package;
      description = "kestrel-broker package (set by the Kestrel flake).";
    };
    configRepo = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/kestrel/config";
    };
    stateDir = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/kestrel/state";
    };
    guardEtcNixos = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Replace /etc/nixos/configuration.nix and /etc/nixos/flake.nix with stubs that refuse to
        build. A plain `nixos-rebuild switch` would otherwise build them and replace the whole
        system with a stock NixOS one, outside the broker and the ledger. Files already there
        (or symlinks) are moved aside to `<name>.pre-kestrel` first, never overwritten. Turning
        this off, or rolling back past it, removes the stubs but does not move those files back.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    environment.variables = {
      KESTREL_CONFIG_REPO = cfg.configRepo;
      KESTREL_STATE_DIR = cfg.stateDir;
    };

    systemd.tmpfiles.rules = [
      "d /var/lib/kestrel 0755 root root -"
      "d ${cfg.stateDir} 0700 root root -"
    ];

    # A stray `nixos-rebuild switch` (a person, or another tool following stock NixOS habits) must
    # fail loudly instead of switching the machine away from its ledgered config.
    environment.etc = lib.mkIf cfg.guardEtcNixos (
      let
        # Deliberately does not name the moved-aside files: building any other config is the same
        # mistake.
        message = ''
          This machine is managed by Kestrel. Its configuration is ${cfg.configRepo}, and only
          kestrel-broker changes it, so that every change is ledgered and can be undone:
            kestrel pkg add <package>      (software)
            sudo kestrel-broker propose --diff change.diff --reason "..."   (anything else)
          Do not build /etc/nixos or any other configuration with nixos-rebuild: it would replace
          this whole system with a stock NixOS one, outside the ledger.
        '';
        # Real files, not the usual symlinks into /etc/static: nix copies a flake's directory to
        # the store, where an absolute symlink no longer resolves, and nixos-rebuild follows a
        # symlinked flake.nix to its own directory. Either way the message would never be seen.
        stub = text: { inherit text; mode = "0444"; };
      in
      {
        "nixos/configuration.nix" = stub ''
          # Managed by Kestrel (kestrel.broker.guardEtcNixos). See the message below.
          throw '''
          ${message}'''
        '';
        "nixos/flake.nix" = stub ''
          # Managed by Kestrel (kestrel.broker.guardEtcNixos). See the message below.
          {
            outputs = _: throw '''
          ${message}''';
          }
        '';
      }
    );
    # /etc activation replaces existing files silently: move the owner's files (or symlinks — the
    # link itself, so its target is kept) aside first. Our own stub is recognised by its marker.
    system.activationScripts.kestrel-guard-etc-nixos = lib.mkIf cfg.guardEtcNixos ''
      for f in configuration.nix flake.nix; do
        p=/etc/nixos/$f
        if { [ -e "$p" ] || [ -L "$p" ]; } && ! grep -qs 'kestrel.broker.guardEtcNixos' "$p"; then
          keep=$p.pre-kestrel
          { [ -e "$keep" ] || [ -L "$keep" ]; } && keep="$keep.$(date +%Y%m%d-%H%M%S)"
          mv "$p" "$keep"
        fi
      done
    '';
    system.activationScripts.etc.deps = lib.mkIf cfg.guardEtcNixos [ "kestrel-guard-etc-nixos" ];
    assertions = lib.mkIf cfg.guardEtcNixos [{
      # With a mutable /etc overlay, moving the old file aside leaves a whiteout that hides the stub.
      assertion = !(config.system.etc.overlay.enable or false);
      message = "kestrel.broker.guardEtcNixos does not support system.etc.overlay yet.";
    }];

    # Initialise repo + ledger once a config flake has been placed in the repo directory.
    # Runs exactly once: after that, only the broker writes to the repo. Re-running it on every
    # boot or switch would commit out-of-band edits as "baseline" with no ledger entry, and
    # during a broker apply it collides with the broker lock and fails the switch.
    systemd.services.kestrel-broker-init = {
      description = "Initialise Kestrel config repo and ledger";
      wantedBy = [ "multi-user.target" ];
      restartIfChanged = false;
      unitConfig.ConditionPathExists = [
        "${cfg.configRepo}/flake.nix"
        "!${cfg.configRepo}/.git"
      ];
      path = [ pkgs.git ];
      environment = {
        KESTREL_CONFIG_REPO = cfg.configRepo;
        KESTREL_STATE_DIR = cfg.stateDir;
      };
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = "${lib.getExe cfg.package} init";
        # TODO(M3): run the broker as a socket-activated daemon with tighter sandboxing.
        ProtectHome = true;
        PrivateTmp = true;
        NoNewPrivileges = true;
      };
    };
  };
}

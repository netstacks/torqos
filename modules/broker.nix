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
    configName = lib.mkOption {
      type = lib.types.str;
      default = config.networking.hostName;
      defaultText = lib.literalExpression "config.networking.hostName";
      description = ''
        The `nixosConfigurations.<name>` in the config repo that this machine is built from. The
        broker rebuilds `--flake <repo>#<name>` explicitly: without a name, nixos-rebuild picks one
        by the *running* hostname, which is not always this machine's configured one (after a
        switch from another configuration it stays the old one until reboot).
      '';
    };
    guardEtcNixos = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Replace /etc/nixos/configuration.nix and /etc/nixos/flake.nix with stubs that refuse to
        build. A plain `nixos-rebuild switch` would otherwise build them and replace the whole
        system with a stock NixOS one, outside the broker and the ledger. Files already there
        (or symlinks) are moved aside to `<name>.pre-kestrel` first, never overwritten. Turning
        this off, or rolling back past it, deletes whatever is at those two paths (NixOS cleans up
        the files it installed) and does not move the old files back: while the guard is on, keep
        nothing of your own there.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    # The broker with this machine's rebuild command baked in (a default, so it survives sudo's
    # environment reset; KESTREL_REBUILD_CMD still overrides it, see KNOWN-GAPS #4).
    environment.systemPackages = [
      (pkgs.symlinkJoin {
        name = "kestrel-broker-${cfg.configName}";
        paths = [ cfg.package ];
        nativeBuildInputs = [ pkgs.makeWrapper ];
        postBuild = ''
          wrapProgram $out/bin/kestrel-broker \
            --set-default KESTREL_REBUILD_CMD ${lib.escapeShellArg "nixos-rebuild switch --flake {repo}#${cfg.configName}"}
        '';
        meta.mainProgram = "kestrel-broker";
      })
    ];

    environment.variables = {
      KESTREL_CONFIG_REPO = cfg.configRepo;
      KESTREL_STATE_DIR = cfg.stateDir;
    };

    systemd.tmpfiles.rules = [
      "d /var/lib/kestrel 0755 root root -"
      "d ${cfg.stateDir} 0700 root root -"
      "d /run/kestrel 0755 root root -"
    ];

    # Drift from the ledger (the machine switched to another configuration, the config repo
    # changed behind the broker's back, an interrupted operation, a broken ledger) must not go
    # unnoticed: check at boot and every 15 minutes; problems go to /run/kestrel/drift (shown by
    # every new interactive shell) and to the journal at warning priority.
    # The unit never fails: it can run *during* a switch (the timer fires then), and a failed unit
    # fails the switch, which the broker treats as a failed rebuild and rolls back (ADR-008).
    # A monitor must not be able to undo the changes it watches.
    systemd.services.kestrel-status = {
      description = "Check that this machine is as its Kestrel ledger says";
      after = [ "kestrel-broker-init.service" ];
      wantedBy = [ "multi-user.target" ];
      restartIfChanged = false;
      path = [ pkgs.git ];
      environment = {
        KESTREL_CONFIG_REPO = cfg.configRepo;
        KESTREL_STATE_DIR = cfg.stateDir;
      };
      serviceConfig.Type = "oneshot";
      script = ''
        out=$(${lib.getExe cfg.package} status 2>&1) && ok=1 || ok=0
        case "$out" in
          *"not checked"*)  # an operation is running: keep any earlier finding as it is
            printf '%s\n' "$out"
            exit 0 ;;
        esac
        if [ $ok = 1 ]; then
          rm -f /run/kestrel/drift
          printf '%s\n' "$out"
        else
          printf '%s\n' "$out" | grep '^PROBLEM' > /run/kestrel/drift || printf 'PROBLEM: %s\n' "$out" > /run/kestrel/drift
          printf '%s\n' "$out" | sed 's/^/<4>/'  # journal priority: warning
        fi
      '';
    };
    systemd.timers.kestrel-status = {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnBootSec = "2min";
        OnUnitActiveSec = "15min";
      };
    };
    environment.interactiveShellInit = ''
      if [ -s /run/kestrel/drift ]; then
        echo "Kestrel: this machine is not as its ledger says (details: sudo kestrel-broker status):" >&2
        sed 's/^PROBLEM: /  - /' /run/kestrel/drift >&2
      fi
    '';

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
    # /etc activation replaces existing files silently: move the owner's files aside first.
    # A symlink is moved as a link, never opened (it could point at a FIFO or a hung mount, and this
    # runs at boot). A regular file is left in place only if it is byte-identical to our stub in the
    # running or the new generation; anything else — even a stub someone edited — is moved aside.
    system.activationScripts.kestrel-guard-etc-nixos = lib.mkIf cfg.guardEtcNixos ''
      for f in configuration.nix flake.nix; do
        p=/etc/nixos/$f
        [ -e "$p" ] || [ -L "$p" ] || continue
        if [ -f "$p" ] && [ ! -L "$p" ]; then
          for stub in "/etc/static/nixos/$f" "$systemConfig/etc/nixos/$f"; do
            [ -f "$stub" ] && ${pkgs.diffutils}/bin/cmp -s "$p" "$stub" && continue 2
          done
        fi
        keep=$p.pre-kestrel
        { [ -e "$keep" ] || [ -L "$keep" ]; } && keep="$keep.$(date +%Y%m%d-%H%M%S)"
        mv "$p" "$keep"
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

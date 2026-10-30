{ config, lib, pkgs, ... }:

let
  cfg = config.torq.broker;
in
{
  options.torq.broker = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "The TorqOS change broker (sole writer of the config repo).";
    };
    package = lib.mkOption {
      type = lib.types.package;
      description = "torq-broker package (set by the TorqOS flake).";
    };
    configRepo = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/torq/config";
    };
    stateDir = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/torq/state";
    };
    configName = lib.mkOption {
      type = lib.types.str;
      default = "torq";
      description = ''
        The `nixosConfigurations.<name>` in the config repo that this machine is built from; the
        template's flake always exposes `torq`. The broker rebuilds `--flake <repo>#<name>`
        explicitly, and the name is fixed rather than the host name: without a name, nixos-rebuild
        picks one by the *running* host name, and if the name followed the host name, changing the
        host name would leave the next rebuild looking for a configuration that does not exist.
      '';
    };
    rebuildCommand = lib.mkOption {
      type = lib.types.str;
      default = "nixos-rebuild switch --flake {repo}#${cfg.configName}";
      defaultText = lib.literalExpression ''"nixos-rebuild switch --flake {repo}#''${config.torq.broker.configName}"'';
      description = ''
        What the broker runs after each commit; `{repo}` is replaced with the config repo's path.
        Part of the broker's fixed settings (/etc/torq/broker.json): a root broker refuses any other
        command from its caller. Tests set it to `true`.
      '';
    };
    guardEtcNixos = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Replace /etc/nixos/configuration.nix and /etc/nixos/flake.nix with stubs that refuse to
        build. A plain `nixos-rebuild switch` would otherwise build them and replace the whole
        system with a stock NixOS one, outside the broker and the ledger. Files already there
        (or symlinks) are moved aside to `<name>.pre-torq` first, never overwritten. Turning
        this off, or rolling back past it, deletes whatever is at those two paths (NixOS cleans up
        the files it installed) and does not move the old files back: while the guard is on, keep
        nothing of your own there.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    systemd.tmpfiles.rules = [
      "d /var/lib/torq 0755 root root -"
      "d ${cfg.stateDir} 0700 root root -"
      "d /run/torq 0755 root root -"
    ];

    # Drift from the ledger (the machine switched to another configuration, the config repo
    # changed behind the broker's back, an interrupted operation, a broken ledger) must not go
    # unnoticed: check at boot and every 15 minutes; problems go to /run/torq/drift (shown by
    # every new interactive shell) and to the journal at warning priority.
    # The unit never fails: it can run *during* a switch (the timer fires then), and a failed unit
    # fails the switch, which the broker treats as a failed rebuild and rolls back (ADR-008).
    # A monitor must not be able to undo the changes it watches.
    systemd.services.torq-status = {
      description = "Check that this machine is as its TorqOS ledger says";
      after = [ "torq-broker-init.service" ];
      wantedBy = [ "multi-user.target" ];
      restartIfChanged = false;
      serviceConfig.Type = "oneshot";
      # Never exits non-zero (see above), whatever happens in here.
      script = ''
        set +e
        out=$(${lib.getExe cfg.package} status 2>&1)
        code=$?
        mkdir -p /run/torq
        case $code in
          0)
            rm -f /run/torq/drift
            printf '%s\n' "$out" ;;
          75)  # an operation is running, nothing compared: keep any earlier finding as it is
            printf '%s\n' "$out" ;;
          *)
            { printf '%s\n' "$out" | grep '^PROBLEM' || printf 'PROBLEM: %s\n' "$out"; } > /run/torq/drift.new
            mv -f /run/torq/drift.new /run/torq/drift  # atomic: a shell never sees half of it
            printf '%s\n' "$out" | sed 's/^/<4>/' ;;  # journal priority: warning
        esac
        exit 0
      '';
    };
    systemd.timers.torq-status = {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnBootSec = "2min";
        OnUnitActiveSec = "15min";
      };
    };
    environment.interactiveShellInit = ''
      if [ -s /run/torq/drift ]; then
        echo "TorqOS: this machine is not as its ledger says (details: sudo torq-broker status):" >&2
        sed 's/^PROBLEM: /  - /' /run/torq/drift >&2
      fi
    '';

    # The broker's fixed settings: a root broker takes them only from here and refuses a caller's
    # flags or TORQ_* variables that differ (broker/src/config.rs; KNOWN-GAPS #4). PATH and HOME for
    # what it runs are fixed too, never the caller's.
    environment.etc = lib.mkMerge [
     {
      "torq/broker.json".text = builtins.toJSON {
        repo = cfg.configRepo;
        stateDir = cfg.stateDir;
        rebuild = cfg.rebuildCommand;
        currentSystem = "/run/current-system";
        systemProfile = "/nix/var/nix/profiles/system";
        # The tools undo depends on, by store path first: a reviewed but mistaken change that drops
        # git or nixos-rebuild from the system must not leave `revert` unable to run them. The
        # running system's tools come after, for anything else.
        path = lib.makeBinPath [
          config.system.build.nixos-rebuild
          config.nix.package
          pkgs.git
          pkgs.coreutils
          pkgs.bash
          config.systemd.package
        ] + ":/run/current-system/sw/bin";
        home = "/root";
      };
     }

    # A stray `nixos-rebuild switch` (a person, or another tool following stock NixOS habits) must
    # fail loudly instead of switching the machine away from its ledgered config.
     (lib.mkIf cfg.guardEtcNixos (
      let
        # Deliberately does not name the moved-aside files: building any other config is the same
        # mistake.
        message = ''
          This machine is managed by TorqOS. Its configuration is ${cfg.configRepo}, and only
          torq-broker changes it, so that every change is ledgered and can be undone:
            torq pkg add <package>      (software)
            sudo torq-broker propose --diff change.diff --reason "..."   (anything else)
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
          # Managed by TorqOS (torq.broker.guardEtcNixos). See the message below.
          throw '''
          ${message}'''
        '';
        "nixos/flake.nix" = stub ''
          # Managed by TorqOS (torq.broker.guardEtcNixos). See the message below.
          {
            outputs = _: throw '''
          ${message}''';
          }
        '';
      }
    ))
    ];
    # /etc activation replaces existing files silently: move the owner's files aside first.
    # A symlink is moved as a link, never opened (it could point at a FIFO or a hung mount, and this
    # runs at boot). A regular file is left in place only if it is byte-identical to our stub in the
    # running or the new generation; anything else — even a stub someone edited — is moved aside.
    system.activationScripts.torq-guard-etc-nixos = lib.mkIf cfg.guardEtcNixos ''
      for f in configuration.nix flake.nix; do
        p=/etc/nixos/$f
        [ -e "$p" ] || [ -L "$p" ] || continue
        if [ -f "$p" ] && [ ! -L "$p" ]; then
          for stub in "/etc/static/nixos/$f" "$systemConfig/etc/nixos/$f"; do
            [ -f "$stub" ] && ${pkgs.diffutils}/bin/cmp -s "$p" "$stub" && continue 2
          done
        fi
        keep=$p.pre-torq
        { [ -e "$keep" ] || [ -L "$keep" ]; } && keep="$keep.$(date +%Y%m%d-%H%M%S)"
        mv "$p" "$keep"
      done
    '';
    system.activationScripts.etc.deps = lib.mkIf cfg.guardEtcNixos [ "torq-guard-etc-nixos" ];
    assertions = lib.mkIf cfg.guardEtcNixos [{
      # With a mutable /etc overlay, moving the old file aside leaves a whiteout that hides the stub.
      assertion = !(config.system.etc.overlay.enable or false);
      message = "torq.broker.guardEtcNixos does not support system.etc.overlay yet.";
    }];

    # Initialise repo + ledger once a config flake has been placed in the repo directory.
    # Runs exactly once: after that, only the broker writes to the repo. Re-running it on every
    # boot or switch would commit out-of-band edits as "baseline" with no ledger entry, and
    # during a broker apply it collides with the broker lock and fails the switch.
    systemd.services.torq-broker-init = {
      description = "Initialise TorqOS config repo and ledger";
      wantedBy = [ "multi-user.target" ];
      restartIfChanged = false;
      unitConfig.ConditionPathExists = [
        "${cfg.configRepo}/flake.nix"
        "!${cfg.configRepo}/.git"
      ];
      # Its settings (and PATH) come from /etc/torq/broker.json.
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

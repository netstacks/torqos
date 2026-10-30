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

    # Initialise repo + ledger once a config flake has been placed in the repo directory.
    systemd.services.kestrel-broker-init = {
      description = "Initialise Kestrel config repo and ledger";
      wantedBy = [ "multi-user.target" ];
      unitConfig.ConditionPathExists = "${cfg.configRepo}/flake.nix";
      path = [ pkgs.git ];
      environment = {
        KESTREL_CONFIG_REPO = cfg.configRepo;
        KESTREL_STATE_DIR = cfg.stateDir;
      };
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${lib.getExe cfg.package} init";
        # TODO(M3): run the broker as a socket-activated daemon with tighter sandboxing.
        ProtectHome = true;
        PrivateTmp = true;
        NoNewPrivileges = true;
      };
    };
  };
}

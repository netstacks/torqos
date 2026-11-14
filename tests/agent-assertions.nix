# Evaluation check (builds nothing): modules/agent.nix's assertions each refuse what they say they
# refuse, with their message, and a plain configuration trips none of them. If an assertion is taken
# out or stops matching, evaluating this check throws.
{ pkgs, self }:

let
  lib = pkgs.lib;
  base = {
    torq.user = "dev";
    boot.loader.grub.enable = false;
    fileSystems."/" = { device = "/dev/disk/by-label/root"; fsType = "ext4"; };
  };
  configOf = extra: (import (pkgs.path + "/nixos/lib/eval-config.nix") {
    system = pkgs.stdenv.hostPlatform.system;
    modules = [ self.nixosModules.default base extra ];
  }).config;
  failing = extra:
    let
      config = (import (pkgs.path + "/nixos/lib/eval-config.nix") {
        system = pkgs.stdenv.hostPlatform.system;
        modules = [ self.nixosModules.default base extra ];
      }).config;
    in map (a: a.message) (lib.filter (a: !a.assertion) config.assertions);
  doctorOn = { enable = true; endpoint = "https://api.example.org/v1"; model = "m"; };
  ours = ms: lib.filter (m: lib.hasInfix "torq.agent" m || lib.hasInfix "torq-agent" m || lib.hasInfix "ptrace_scope" m) ms;
  # `name` fails evaluation with a message holding `text`.
  refuses = name: extra: text:
    let ms = failing extra; in
    if lib.any (lib.hasInfix text) ms then name
    else throw "agent-assertions: ${name}: no assertion says \"${text}\" (failing: ${builtins.toJSON (ours ms)})";
  cases = [
    (let ms = ours (failing { }); in if ms == [ ] then "plain" else throw "agent-assertions: a plain configuration fails: ${builtins.toJSON ms}")
    (refuses "not a person" { torq.agent.people = [ "ghost" ]; } "ghost is not a person on this machine")
    (refuses "too long" { torq.agent.people = [ "dev" "abcdefghijklmnopqrstu" ]; users.users.abcdefghijklmnopqrstu.isNormalUser = true; } "is longer than an account's and its group's name may be")
    (refuses "twice" { torq.agent.people = [ "dev" "dev" ]; } "a person is named twice")
    (refuses "a key in the endpoint's query" { torq.agent.doctor.endpoint = "https://api.example.org/v1?key=secret"; } "no query (?) or fragment (#)")
    (refuses "a fragment in the endpoint" { torq.agent.doctor.endpoint = "https://api.example.org/v1#secret"; } "no query (?) or fragment (#)")
    (refuses "a person in people named as an agent" { torq.agent.people = [ "torq-agent-x" ]; users.users.torq-agent-x.isNormalUser = true; } "torq-agent-x is named as an agent account is")
    (refuses "any person named as an agent" { users.users.torq-agent-y.isNormalUser = true; } "torq-agent-y is a person named as an agent account is")
    (refuses "a person named as a resident agent" { users.users.torq-resident-y.isNormalUser = true; } "torq-resident-y is a person named as an agent account is")
    (refuses "too long for a resident account" { torq.agent.people = [ "dev" "abcdefghijklmnopqr" ]; users.users.abcdefghijklmnopqr.isNormalUser = true; } "at most 17 characters")
    (refuses "SETENV on the launcher's rule" ({ config, ... }: {
      security.sudo.extraRules = [{ groups = [ "wheel" ]; commands = [{ command = "${config.torq.agent.launcher}/bin/torq-agent-run"; options = [ "NOPASSWD" "SETENV" ]; }]; }];
    }) "a rule for torq-agent-run with SETENV")
    # torq agent tools (ADR-051 5): Claude Code is unfree, so refused until `torq unfree` allows it — and then nothing
    # unfree is put in the agent's packages; allowed, it is there, in the agent's own profile, not the system's.
    (refuses "claude-code not allowed" { torq.agent.tools = [ "claude-code" ]; } "claude-code is unfree")
    (let c = configOf { torq.agent.tools = [ "claude-code" ]; }; in
      if c.users.users.torq-agent-dev.packages == [ ] then "claude-code not allowed: not installed" else throw "agent-assertions: claude-code put in the agent's packages unallowed")
    (let
      extra = { torq.agent.tools = [ "claude-code" ]; torq.unfree.packages = [ "claude-code" ]; };
      c = configOf extra;
      names = map lib.getName c.users.users.torq-agent-dev.packages;
      ms = ours (failing extra);
    in
      if ms == [ ] && names == [ "claude-code" ] && !(lib.any (p: lib.getName p == "claude-code") c.environment.systemPackages)
      then "claude-code allowed: the agent's alone"
      else throw "agent-assertions: claude-code allowed: ${builtins.toJSON { inherit ms names; }}")
    # What the broker's resident rule rests on (ADR-051 step 5's review): said, where the agent is resident.
    # Each the per-instance assertion's own message, which the template's check (below) does not share.
    (refuses "a delegated resident unit" { torq.agent.doctor = doctorOn; systemd.services."torq-agent@dev".serviceConfig.Delegate = true; } "systemd.services.\"torq-agent@dev\": no Delegate or DelegateSubgroup")
    (refuses "a resident unit's subgroup" { torq.agent.doctor = doctorOn; systemd.services."torq-agent@dev".serviceConfig.DelegateSubgroup = "x"; } "systemd.services.\"torq-agent@dev\": no Delegate or DelegateSubgroup")
    (refuses "a delegated template" { torq.agent.doctor = doctorOn; systemd.services."torq-agent@".serviceConfig.Delegate = true; } "in any torq-agent@ unit")
    (refuses "a lingering agent" { torq.agent.doctor = doctorOn; users.users.torq-agent-dev.linger = true; } "neither account has a user manager of its own")
    (refuses "tracing allowed" { torq.agent.doctor = doctorOn; boot.kernel.sysctl."kernel.yama.ptrace_scope" = lib.mkForce 0; } "kernel.yama.ptrace_scope")
    (refuses "the key where the agent reads it" { torq.agent.doctor = doctorOn // { apiKeyFile = "/var/lib/torq-agent-dev/model.key"; }; } "a file only root reads")
    (refuses "the key in /var/tmp" { torq.agent.doctor = doctorOn // { apiKeyFile = "/var/tmp/model.key"; }; } "a file only root reads")
    (refuses "the key in /dev/shm" { torq.agent.doctor = doctorOn // { apiKeyFile = "/dev/shm/model.key"; }; } "a file only root reads")
    (refuses "the key named another way (//)" { torq.agent.doctor = doctorOn // { apiKeyFile = "//home/dev/model.key"; }; } "named plainly")
    (refuses "the key named another way (/./)" { torq.agent.doctor = doctorOn // { apiKeyFile = "/./tmp/model.key"; }; } "named plainly")
    (refuses "the key named another way (..)" { torq.agent.doctor = doctorOn // { apiKeyFile = "/srv/../home/dev/k"; }; } "named plainly")
    (let ms = ours (failing { torq.agent.doctor = doctorOn // { apiKeyFile = "/var/lib/torq/secrets/model.key"; }; }); in if ms == [ ] then "the key root's" else throw "agent-assertions: a root-only key refused: ${builtins.toJSON ms}")
    # A missing key is said by a unit of its own, started by the agent unit's failure (modules/agent.nix,
    # keyMissing): there only where a key is named, given no credential and nothing of the key but its path
    # (in its script), and the agent's own unit still takes the key the one way and checks it at each start.
    (let
      c = configOf { torq.agent.doctor = doctorOn // { apiKeyFile = "/var/lib/torq/secrets/model.key"; }; };
      agent = c.systemd.services."torq-agent@dev";
      says = c.systemd.services."torq-agent-key@dev";
      sc = says.serviceConfig;
      text = c.systemd.units."torq-agent-key@dev.service".text;
    in
      if agent.onFailure == [ "torq-agent-key@dev.service" ]
        && agent.serviceConfig.LoadCredential == [ "model-key:/var/lib/torq/secrets/model.key" ]
        && lib.length agent.serviceConfig.ExecStartPre == 1 && lib.hasPrefix "+" (lib.head agent.serviceConfig.ExecStartPre)
        && !(agent.serviceConfig ? ExecCondition)
        && sc.Type == "oneshot" && lib.hasSuffix " torq-agent@dev.service" sc.ExecStart
        && sc.CapabilityBoundingSet == "CAP_DAC_READ_SEARCH" && sc.NoNewPrivileges && sc.ProtectSystem == "strict"
        && !(sc ? LoadCredential) && !(sc ? User)
        && !(lib.hasInfix "Credential" text) && !(lib.hasInfix "model.key" text)
        && says.wantedBy == [ ] && says.requiredBy == [ ]
      then "a missing key said by its own unit"
      else throw "agent-assertions: the unit that says a missing key is not as it should be: ${builtins.toJSON { inherit (agent) onFailure; inherit sc; }}")
    (let c = configOf { torq.agent.doctor = doctorOn; }; in
      if !(c.systemd.services ? "torq-agent-key@dev") && c.systemd.services."torq-agent@dev".onFailure == [ ]
        && c.systemd.services."torq-agent@dev".serviceConfig.LoadCredential == [ ]
      then "no key named: nothing to say"
      else throw "agent-assertions: a unit for a missing key where no key is named")
    (refuses "SETENV in sudoers text" ({ config, ... }: {
      security.sudo.extraConfig = "%wheel ALL=(root) NOPASSWD: SETENV: ${config.torq.agent.launcher}/bin/torq-agent-run";
    }) "a rule for torq-agent-run with SETENV")
  ];
in
pkgs.runCommand "torq-agent-assertions" { } ''
  echo ${lib.escapeShellArg (lib.concatStringsSep "\n" cases)} > $out
''

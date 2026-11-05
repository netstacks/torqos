# The agent's account (ADR-041): `torq-agent`, the one account whatever arrives from is the agent
# (ADR-040: the broker names its caller from the kernel's uid). It proposes, and only what
# `torq.agent.mayPropose` lists; a person applies. It is made for TorqOS's own agent (M5) and for the
# AI tools people already use — Claude Code first — run as it rather than as the owner.
{ config, lib, ... }:

let
  cfg = config.torq.agent;
in
{
  options.torq.agent.mayPropose = {
    users = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = lib.optional (config.torq.user != null) config.torq.user;
      defaultText = lib.literalExpression "[ config.torq.user ]";
      description = "The people whose packages (`users <name> packages`) the agent may propose: the owner, by default.";
    };
    settings = lib.mkOption {
      type = lib.types.listOf (lib.types.strMatching "[A-Za-z0-9_-]+(\\.[A-Za-z0-9_-]+)*");
      default = [ ];
      example = [ "services.openssh" "environment.systemPackages" "system.host" ];
      description = ''
        The settings the agent may propose changes in, as subtrees of the tree (dotted paths): every
        setting a proposal of the agent's changes must lie inside one of them, or the broker refuses
        it, and ledgers the refusal. None, by default: the agent proposes packages and nothing else.
        `torq.*` is never the agent's, listed or not (its own limits are there), and nor are config
        files, `flake.nix` or the lock.
\n        `system.<name>` means both torqcli's own settings (`system host-name`, `system packages`:
        `system.host`, `system.packages`) and NixOS's `system.*` options of that name — list the
        narrowest one. A bare `system` is refused: it would allow `system.activationScripts`, code
        run as root.
      '';
    };
  };

  config = {
    assertions = [{
      assertion = lib.all (s: lib.head (lib.splitString "." s) != "torq") cfg.mayPropose.settings;
      message = "torq.agent.mayPropose.settings: torq.* is never the agent's (its own limits are there).";
    }
    {
      assertion = !(builtins.elem "system" cfg.mayPropose.settings);
      message = "torq.agent.mayPropose.settings: a bare `system` would let the agent propose system.activationScripts (code run as root); list the setting itself (system.host, …).";
    }];

    # A static system user: the broker names its caller from /etc/passwd, where a DynamicUser is not.
    # No password, no login shell (no SSH, no console), no group but its own: no sudo, no devices.
    # Its home holds what a tool keeps (Claude Code's ~/.claude, its credentials) — never the
    # owner's. What it changes in the system, it proposes through the broker's socket, as torqcli does.
    users.users.torq-agent = {
      isSystemUser = true;
      group = "torq-agent";
      description = "TorqOS agent (proposes; a person applies)";
      home = "/var/lib/torq-agent";
      createHome = true;
      homeMode = "700";
      hashedPassword = "!";
    };
    users.groups.torq-agent = { };
  };
}

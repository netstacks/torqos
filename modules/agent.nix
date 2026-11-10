# The agents' accounts (ADR-041, ADR-051 3): one per person, `torq-agent-<person>` — the only accounts
# whatever arrives from is an agent (ADR-040: the broker names its caller from the kernel's uid, and
# its map, /etc/torq/broker.json, says which person each agent serves). An agent proposes, and only
# what `torq.agent.mayPropose` lists and its own person's packages; a person applies. Each is made for
# TorqOS's own agent (M5) and for the AI tools people already use — Claude Code first — run as it
# rather than as the person.
{ config, lib, pkgs, ... }:

let
  cfg = config.torq.agent;
  agentOf = person: "torq-agent-${person}";
  # Each agent account, and the person it serves: the broker's map (broker.json), and the accounts below.
  # (Only names that fit: one too long is the assertion's to say, below, not NixOS's abort.)
  fits = p: lib.stringLength (agentOf p) <= 31;
  # The launcher's sudo rules that would let its caller set the environment (SETENV), as text or as rules.
  isLauncher = c: lib.hasInfix "torq-agent-run" (if builtins.isString c then c else c.command);
  launcherSetenv = lib.filter (c: isLauncher c && !(builtins.isString c) && builtins.elem "SETENV" c.options) (lib.concatMap (r: r.commands) config.security.sudo.extraRules)
    ++ lib.filter (l: lib.hasInfix "torq-agent-run" l && lib.hasInfix "SETENV" l) (lib.splitString "\n" config.security.sudo.extraConfig);
  personsLikeAgents = lib.attrNames (lib.filterAttrs (n: u: u.isNormalUser && lib.hasPrefix "torq-agent" n) config.users.users);
  agents = lib.listToAttrs (map (p: lib.nameValuePair (agentOf p) p) (lib.filter fits cfg.people));
  # `torq agent run` (ADR-041, 5): a person starts a program as their own agent. This runs as root
  # (through sudo), hides every home and the people's runtime directories, gives a private /tmp,
  # binds in only the directory shared with it, and drops to the caller's agent for good.
  agentRun = pkgs.writeShellApplication {
    name = "torq-agent-run";
    runtimeInputs = [ pkgs.bubblewrap pkgs.coreutils pkgs.util-linux pkgs.bash pkgs.jq pkgs.getent ];
    text = builtins.readFile ./agent-run.sh;
  };
in
{
  imports = [
    # Removed, not renamed: it said whose packages the one agent might propose; `people` says who has an
    # agent of their own, which proposes only that person's. The same list would mean something else.
    (lib.mkRemovedOptionModule [ "torq" "agent" "mayPropose" "users" ] ''
      Each person now has an agent account of their own (ADR-051 3), which proposes packages only for
      that person: list the people who get one in torq.agent.people (the owner, by default). No agent
      proposes another person's packages any more.'')
  ];

  options.torq.agent.launcher = lib.mkOption {
    type = lib.types.package;
    internal = true;
    readOnly = true;
    default = agentRun;
    description = "The launcher of `torq agent run` (strict mode names it in its sudo rule).";
  };

  options.torq.agent.people = lib.mkOption {
    # A login name's shape, as the broker's (policy.rs, user_name_ok).
    type = lib.types.listOf (lib.types.strMatching "[a-z_][a-z0-9_-]*");
    default = lib.optional (config.torq.user != null) config.torq.user;
    defaultText = lib.literalExpression "[ config.torq.user ]";
    example = [ "alice" "bob" ];
    description = ''
      The people who each get an agent account of their own, `torq-agent-<name>` (ADR-051 3): no
      password, no login, no group but its own, its home /var/lib/torq-agent-<name> its own. Each
      person's agent proposes packages only for that person (`users <name> packages`), and settings
      only as `mayPropose.settings` allows; that person's trust covers it and no other. The owner, by
      default. Each must be a person of this machine (a normal user), with a name of at most 20
      characters (NixOS takes an account's and a group's name of at most 31).
    '';
  };

  options.torq.agent.agents = lib.mkOption {
    type = lib.types.attrsOf lib.types.str;
    internal = true;
    readOnly = true;
    default = agents;
    description = "Each agent account and the person it serves (the broker's map, /etc/torq/broker.json).";
  };

  options.torq.agent.mayPropose = {
    settings = lib.mkOption {
      type = lib.types.listOf (lib.types.strMatching "[A-Za-z0-9_-]+(\\.[A-Za-z0-9_-]+)*");
      default = [ ];
      example = [ "services.openssh" "environment.systemPackages" "system.host" ];
      description = ''
        The settings every agent may propose changes in (the machine's list, the same for each person's
        agent), as subtrees of the tree (dotted paths): every
        setting a proposal of the agent's changes must lie inside one of them, or the broker refuses
        it, and ledgers the refusal. None, by default: the agent proposes packages and nothing else.
        `torq.*` is never the agent's, listed or not (its own limits are there), and nor are config
        files, `flake.nix` or the lock.

        `system.<name>` means both torqcli's own settings (`system host-name`, `system packages`:
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
    }]
    # Each person's agent is that person's: a person of this machine, an account name Linux takes
    # (at most 31 characters, as NixOS takes a group's) and the broker does (policy.rs), once each, and never itself an agent's name.
    ++ lib.concatMap (p: [
      {
        assertion = config.users.users ? ${p} && config.users.users.${p}.isNormalUser;
        message = "torq.agent.people: ${p} is not a person on this machine (a normal user): an agent account is a person's.";
      }
      {
        assertion = fits p;
        message = "torq.agent.people: ${agentOf p} is longer than an account's and its group's name may be (31 characters on NixOS): ${p} is too long a name for an agent of its own.";
      }
      {
        assertion = !(lib.hasPrefix "torq-agent" p);
        message = "torq.agent.people: ${p} is named as an agent account is; a person's name never begins with torq-agent.";
      }
    ]) cfg.people
    ++ [{
      assertion = lib.unique cfg.people == cfg.people;
      message = "torq.agent.people: a person is named twice.";
    }]
    ++ [{
      # Whose agent the launcher runs is sudo's SUDO_UID (modules/agent-run.sh). A rule that lets the
      # caller set the environment lets them set that too (`sudo SUDO_UID=<another's> …` is taken as
      # given, and `Defaults !setenv` does not undo a rule's SETENV): never on the launcher's rules.
      assertion = launcherSetenv == [ ];
      message = "security.sudo: a rule for torq-agent-run with SETENV would let its caller say whose agent it runs (SUDO_UID); it never has one.";
    }
    {
      # No person is named as an agent account is, whether they have an agent or not: torqcli tells an
      # agent by its name (cli/src/agent.rs, is_agent), and would take such a person for one.
      assertion = personsLikeAgents == [ ];
      message = "users.users: ${lib.concatStringsSep ", " personsLikeAgents} is a person named as an agent account is (torq-agent…): a person's name never begins so.";
    }];

    # Static system users: the broker names its caller from /etc/passwd, where a DynamicUser is not.
    # Each: no password, no login shell (no SSH, no console), no group but its own: no sudo, no
    # devices. Its home holds what a tool keeps (Claude Code's ~/.claude, its credentials) — never the
    # person's, and never another agent's. What it changes in the system, it proposes through the
    # broker's socket, as torqcli does.
    users.users = lib.mapAttrs (agent: person: {
      isSystemUser = true;
      group = agent;
      description = "${person}'s TorqOS agent (proposes; a person applies)";
      home = "/var/lib/${agent}";
      createHome = true;
      homeMode = "700";
      hashedPassword = "!";
    }) agents;
    users.groups = lib.mapAttrs (_: _: { }) agents;

    # A person (an administrator) may start a program as their own agent, only through the launcher:
    # it binds the one shared directory as root, then drops to the caller's agent for good before
    # running anything (modules/agent-run.sh). No password: administrators have root already, and the
    # program runs with less than they have.
    environment.systemPackages = [ agentRun pkgs.acl ];
    # Where the launcher binds a shared directory first, as root, before checking it (root's alone).
    systemd.tmpfiles.settings."10-torq-agent"."/run/torq-agent-stage".d = { mode = "0700"; user = "root"; group = "root"; };
    security.sudo.extraRules = [{
      groups = [ "wheel" ];
      runAs = "root";
      commands = [{ command = "${agentRun}/bin/torq-agent-run"; options = [ "NOPASSWD" ]; }];
    }];
    # The shared directory comes as a descriptor torqcli opened, as the person — the agent has no
    # way into their home: sudo keeps that descriptor open for the launcher (`-C`).
    security.sudo.extraConfig = ''
      Defaults!${agentRun}/bin/torq-agent-run closefrom_override, env_reset
    '';
  };
}

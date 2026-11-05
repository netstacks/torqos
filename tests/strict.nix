# NixOS VM test: strict mode and `torq break-glass` (ADR-035). An administrator's sudo runs TorqOS's
# own commands and nothing else; a root shell is break-glass, which leaves its ledger entry first,
# and pushes it to the remote before the shell opens. The agent has no way to it.
{ pkgs, self }:

let
  devHash = "$6$torqsocketsalt$Z7RLey3l5vKGfukhfuZYYM/EjWNo7QM8.65r88XmZ9z3/jBCY.NAnH7RwYpiLoE2fedxUd/I7UFcKDX1KEXDg/";
in
pkgs.testers.runNixOSTest {
  name = "torq-strict";

  nodes.gitserver = { pkgs, ... }: {
    services.openssh.enable = true;
    users.users.git = { isNormalUser = true; shell = pkgs.bash; };
    environment.systemPackages = [ pkgs.git ];
  };

  nodes.machine = { pkgs, lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPassword = devHash;
    torq.broker.rebuildCommand = "true";
    torq.remote.url = "ssh://git@gitserver/home/git/config.git";
    torq.security.strict.enable = true;
    security.polkit.enable = true;
    environment.systemPackages = [ pkgs.jq pkgs.tmux pkgs.git ];
  };

  testScript = ''
    import json, shlex
    start_all()
    gitserver.wait_for_unit("sshd.service")
    gitserver.succeed("su - git -c 'git init -q --bare config.git && mkdir -p ~/.ssh && chmod 700 ~/.ssh'")
    machine.wait_for_unit("multi-user.target")
    repo = "/var/lib/torq/config"
    machine.succeed(f"mkdir -p {repo} && echo '{{}}' > {repo}/desktop.json && touch {repo}/flake.nix && torq-broker init")
    key = json.loads(machine.succeed("torq-broker --json remote-key"))["public_key"]
    gitserver.succeed(f"echo {shlex.quote(key)} > /home/git/.ssh/authorized_keys && chown git /home/git/.ssh/authorized_keys && chmod 600 /home/git/.ssh/authorized_keys")
    machine.succeed("torq-broker push")
    pw = "Torq-sock-2210"

    def sudo(cmd):
        return machine.execute(f"su - dev -c {shlex.quote(f'printf %s {pw} | sudo -S -k -p \"\" {cmd}')} 2>&1")

    def last():
        return json.loads(machine.succeed("torq-broker log --json --brief | tail -1"))

    with subtest("sudo runs TorqOS's commands, and nothing else"):
        # What sudo grants, from sudo itself: a refusal alone would pass for any reason at all.
        code, out = sudo("-l")
        assert code == 0, out
        granted = out.split("may run the following commands")[1]
        assert "torq-break-glass" in granted and "torq-agent-run" in granted, out
        assert "torq-broker" not in granted and " ALL\n" not in granted.replace("(root)", ""), out
        for cmd in ["bash -c id", "true", "cat /etc/shadow", "su -", "env bash", "torq-broker verify", "torq-broker password --user root --hash-file /etc/shadow", "/run/current-system/sw/bin/torq-broker log"]:
            code, out = sudo(cmd)
            assert code != 0, (cmd, out)
        # What a person needs needs no sudo; polkit does not make wheel root.
        machine.succeed("su - dev -c 'torqcli show system ledger'")
        machine.fail("grep -r wheel /etc/polkit-1/rules.d")
        # The agent's account has no way at all.
        machine.fail("su -s /bin/sh torq-agent -c 'sudo -n true'")
        out = machine.fail("su -s /bin/sh torq-agent -c 'torq break-glass anything' 2>&1")
        assert "no root shell" in out or "sudo" in out, out

    with subtest("no reason, no shell"):
        out = machine.fail("su - dev -c 'torq break-glass' 2>&1")
        assert "usage" in out, out

    with subtest("break-glass: its entry first, and on the remote, before the shell opens"):
        n = machine.succeed("torq-broker log --json --brief | wc -l").strip()
        machine.succeed("su - dev -c \"tmux new-session -d -s g -x 150 -y 40 'torq break-glass the disk is full; sleep 600'\"")
        machine.wait_until_succeeds("su - dev -c 'tmux capture-pane -p -t g' | grep -qi 'password'", timeout=60)
        # Nothing yet: the password comes first.
        assert machine.succeed("torq-broker log --json --brief | wc -l").strip() == n
        machine.succeed(f"su - dev -c 'tmux send-keys -t g {pw} Enter'")
        machine.wait_until_succeeds("su - dev -c 'tmux capture-pane -p -t g' | grep -q 'Root shell, on the record'", timeout=60)
        e = last()
        assert e["kind"] == "break_glass" and e["approved_by"] == "dev" and e["reason"] == "the disk is full", e
        got = gitserver.succeed("git -c safe.directory='*' -C /home/git/config.git cat-file blob refs/heads/torq-ledger:ledger.jsonl")
        assert json.loads(got.splitlines()[-1])["kind"] == "break_glass", "on the remote before the shell"
        # And it is a root shell.
        machine.succeed("su - dev -c \"tmux send-keys -t g 'id -u > /tmp/who' Enter\"")
        machine.wait_until_succeeds("test \"$(cat /tmp/who)\" = 0", timeout=30)
        machine.succeed("su - dev -c \"tmux send-keys -t g exit Enter\"")
        out = machine.succeed("su - dev -c 'torqcli show system ledger'")
        assert "Root shell opened: the disk is full" in out, out

    with subtest("the remote out of reach: asked, and the shell only on yes; the entry goes with the next push"):
        gitserver.succeed("systemctl stop sshd")
        machine.succeed("su - dev -c \"tmux new-session -d -s h -x 150 -y 40 'torq break-glass network down; sleep 600'\"")
        machine.wait_until_succeeds("su - dev -c 'tmux capture-pane -p -t h' | grep -qi 'password'", timeout=60)
        machine.succeed(f"su - dev -c 'tmux send-keys -t h {pw} Enter'")
        machine.wait_until_succeeds("su - dev -c 'tmux capture-pane -p -t h' | grep -q 'Open the root shell all the same'", timeout=120)
        assert last()["reason"] == "network down", last()
        machine.succeed("su - dev -c 'tmux send-keys -t h no Enter'")
        machine.wait_until_succeeds("! su - dev -c 'tmux capture-pane -p -t h' | grep -q 'Root shell, on the record'", timeout=10)
        gitserver.succeed("systemctl start sshd")
        gitserver.wait_for_unit("sshd.service")
        machine.succeed("torq-broker push")
        got = gitserver.succeed("git -c safe.directory='*' -C /home/git/config.git cat-file blob refs/heads/torq-ledger:ledger.jsonl")
        assert "network down" in got
  '';
}

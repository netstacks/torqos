# NixOS VM test: the config repo's remote (ADR-036) — pushed after every change, the ledger anchored
# there; a ledger rewritten here is an alarm, even one pushed on top by root; an unreachable remote
# never fails a change. A second machine is the remote: a bare repository behind SSH that, as
# branch protection would, refuses to rewrite or delete.
{ pkgs, self }:

pkgs.testers.runNixOSTest {
  name = "torq-remote";

  nodes.gitserver = { pkgs, ... }: {
    services.openssh.enable = true;
    users.users.git = { isNormalUser = true; shell = "${pkgs.git}/bin/git-shell"; };
    environment.systemPackages = [ pkgs.git ];
  };

  nodes.machine = { pkgs, lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPassword = "!";
    users.allowNoPasswordLogin = true; # (a test: nobody logs in by hand)
    torq.broker.rebuildCommand = "true";
    torq.remote.url = "ssh://git@gitserver/home/git/config.git";
    environment.systemPackages = [ pkgs.jq pkgs.git ];
  };

  testScript = ''
    import json, shlex
    start_all()
    gitserver.wait_for_unit("sshd.service")
    gitserver.succeed("su - git -s /bin/sh -c 'git init -q --bare config.git && git -C config.git config receive.denyNonFastForwards true && git -C config.git config receive.denyDeletes true && mkdir -p ~/.ssh && chmod 700 ~/.ssh'")
    machine.wait_for_unit("multi-user.target")
    repo = "/var/lib/torq/config"
    remote = "/home/git/config.git"
    machine.succeed(f"mkdir -p {repo} && echo '{{}}' > {repo}/desktop.json && touch {repo}/flake.nix && torq-broker init")

    def change(n):
        head = machine.succeed(f"git -C {repo} rev-parse HEAD").strip()
        diff = f"--- a/desktop.json\n+++ b/desktop.json\n@@ -1 +1 @@\n-{machine.succeed(f'cat {repo}/desktop.json').strip()}\n+{{\"n\": {n}}}\n"
        machine.succeed(f"printf %s {shlex.quote(diff)} > /tmp/c.diff")
        p = json.loads(machine.succeed(f"torq-broker --json propose --actor user --base {head} --diff /tmp/c.diff --reason 'change {n}'"))
        return json.loads(machine.succeed(f"torq-broker --json apply {p['seq']} --approved-by dev"))

    def last():
        return json.loads(machine.succeed("cat /var/lib/torq/state/remote/last.json"))

    def remote_ledger():
        return gitserver.succeed(f"git -c safe.directory='*' -C {remote} cat-file blob refs/torq/ledger:ledger.jsonl")

    with subtest("no key yet: a change still lands, and says so"):
        change(1)
        assert not last()["ok"] and "no key yet" in last()["error"], last()
        machine.succeed("torq-broker status")

    with subtest("the key is the machine's, root's alone; added to the remote, every change is pushed"):
        key = json.loads(machine.succeed("torq-broker --json remote-key"))["public_key"]
        assert key.startswith("ssh-ed25519 ") and key == json.loads(machine.succeed("torq-broker --json remote-key"))["public_key"], "made once, kept"
        machine.fail("su - dev -c 'cat /var/lib/torq/state/remote/key'")
        machine.fail("su -s /bin/sh torq-agent -c 'cat /var/lib/torq/state/remote/key'")
        gitserver.succeed(f"echo {shlex.quote(key)} > /home/git/.ssh/authorized_keys && chown git /home/git/.ssh/authorized_keys && chmod 600 /home/git/.ssh/authorized_keys")
        change(2)
        # (The proposal was pushed too, so the apply's push finds the remote agreeing.)
        assert last()["ok"] and last()["anchor"]["state"] == "agrees", last()
        assert gitserver.succeed(f"git -c safe.directory='*' -C {remote} rev-parse refs/heads/main").strip() == machine.succeed(f"git -C {repo} rev-parse HEAD").strip()
        assert remote_ledger() == machine.succeed("cat /var/lib/torq/state/ledger.jsonl")
        change(3)
        assert last()["ok"] and last()["anchor"]["state"] == "agrees", last()
        assert remote_ledger() == machine.succeed("cat /var/lib/torq/state/ledger.jsonl")
        # The remote's host key, pinned at first contact.
        machine.succeed("grep -q gitserver /var/lib/torq/state/remote/known_hosts")
        # And a person sees it, no sudo.
        out = machine.succeed("su - dev -c 'torqcli show system remote'")
        assert "Remote: ssh://git@gitserver/home/git/config.git" in out and "Last push" in out and "as they are here" in out, out

    with subtest("out of reach: the change lands, the push waits, the retry catches up"):
        gitserver.succeed("systemctl stop sshd.socket sshd.service || systemctl stop sshd")
        change(4)
        assert not last()["ok"] and last()["anchor"]["state"] == "unreachable", last()
        machine.succeed("torq-broker status")  # not an alarm: only late
        gitserver.succeed("systemctl start sshd")
        gitserver.wait_for_unit("sshd.service")
        machine.succeed("torq-broker push")
        assert last()["ok"], last()
        assert remote_ledger() == machine.succeed("cat /var/lib/torq/state/ledger.jsonl")

    with subtest("a ledger cut short here is an alarm, and is not pushed"):
        machine.succeed("cp /var/lib/torq/state/ledger.jsonl /tmp/ledger.good")
        machine.succeed("head -n -2 /tmp/ledger.good > /var/lib/torq/state/ledger.jsonl")
        machine.succeed("torq-broker verify")  # the chain alone cannot see a cut end (KNOWN-GAPS 2)
        before = remote_ledger()
        out = machine.fail("torq-broker push")
        assert "not pushed" in out, out
        assert remote_ledger() == before
        out = machine.fail("torq-broker status")
        assert "not the one its remote was given" in out, out

    with subtest("rewritten and pushed on top, by root with the machine's key: the remote's history says so"):
        ssh = "ssh -F /dev/null -i /var/lib/torq/state/remote/key -o UserKnownHostsFile=/var/lib/torq/state/remote/known_hosts -o BatchMode=yes"
        machine.succeed(
            f"cd {repo} && blob=$(git hash-object -w /var/lib/torq/state/ledger.jsonl) && "
            "tree=$(printf '100644 blob %s\\tledger.jsonl\\n' $blob | git mktree) && "
            "c=$(git -c user.name=root -c user.email=root@machine commit-tree $tree -p refs/torq/ledger -m rewritten) && git update-ref refs/torq/ledger $c && "
            f"GIT_SSH_COMMAND={shlex.quote(ssh)} git push -q ssh://git@gitserver/home/git/config.git refs/torq/ledger:refs/torq/ledger"
        )
        assert remote_ledger() == machine.succeed("cat /var/lib/torq/state/ledger.jsonl"), "the remote's newest is the rewritten one"
        out = machine.fail("torq-broker push")
        assert "rewritten at its push" in out, out
        # Whatever the remote allows (branch protection is its own), taking the rewrite back — a forced
        # push of the history before it — leaves this machine's ledger still not the remote's start.
        machine.execute(f"cd {repo} && GIT_SSH_COMMAND={shlex.quote(ssh)} git push -q --force ssh://git@gitserver/home/git/config.git $(git rev-parse refs/torq/ledger~1):refs/torq/ledger")
        out = machine.fail("torq-broker push")
        assert "not pushed" in out, out
        machine.fail("torq-broker status")
  '';
}

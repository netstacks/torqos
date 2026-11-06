# NixOS VM test: snapshots of the data (ADR-038). A btrfs disk laid out as the image's, its subvolumes
# put in place before the broker starts. A snapshot before each change, read only and root's alone;
# the ledger never in one; a restore that runs as the folder's owner, so a link a person planted
# leads nowhere they could not go; what was there kept; every restore in the ledger.
{ pkgs, self }:

let
  devHash = "$6$torqsocketsalt$Z7RLey3l5vKGfukhfuZYYM/EjWNo7QM8.65r88XmZ9z3/jBCY.NAnH7RwYpiLoE2fedxUd/I7UFcKDX1KEXDg/";
in
pkgs.testers.runNixOSTest {
  name = "torq-snapshots";

  nodes.machine = { pkgs, lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPassword = devHash;
    users.users.bob = { isNormalUser = true; };
    users.users.svc = { isSystemUser = true; group = "svc"; };
    users.groups.svc = { };
    torq.broker.rebuildCommand = "true";
    torq.broker.snapshots = { enable = true; keep = 3; };
    virtualisation.emptyDiskImages = [ 2048 ];
    environment.systemPackages = [ pkgs.jq pkgs.tmux pkgs.btrfs-progs pkgs.git ];
  };

  testScript = ''
    import json, shlex
    machine.wait_for_unit("multi-user.target")
    pw = "Torq-sock-2210"
    repo = "/var/lib/torq/config"

    with subtest("a disk laid out as the image's"):
        machine.succeed(
            "mkfs.btrfs -q -L data /dev/vdb && mkdir -p /top && mount /dev/vdb /top && "
            "for s in @home @var-lib @var-lib-torq @snapshots; do btrfs -q subvolume create /top/$s; done && "
            "cp -a /home/. /top/@home/ && cp -a /var/lib/. /top/@var-lib/ && chmod 700 /top/@snapshots && umount /top && "
            "mount -o subvol=@home /dev/vdb /home && mount -o subvol=@var-lib /dev/vdb /var/lib && "
            "mkdir -p /var/lib/torq /.snapshots && mount -o subvol=@var-lib-torq /dev/vdb /var/lib/torq && "
            "mount -o subvol=@snapshots /dev/vdb /.snapshots && chmod 755 /var/lib/torq"
        )
        machine.succeed(f"mkdir -p {repo} && echo '{{}}' > {repo}/desktop.json && touch {repo}/flake.nix && torq-broker init")

    def broker(args):
        return json.loads(machine.succeed(f"torq-broker --json {args}").strip().splitlines()[-1])

    def change(n):
        machine.succeed(f"cd {repo} && cp -r . /tmp/c{n} && cd /tmp/c{n} && echo '{{\"n\": {n}}}' > desktop.json && git diff > /tmp/c{n}.diff")
        p = broker(f"propose --diff /tmp/c{n}.diff --reason change-{n}")["seq"]
        return broker(f"apply {p} --approved-by dev")

    def as_user(user, cmd):
        return f"su - {user} -c {shlex.quote(cmd)}"

    def restore(n, path):
        """As a person at torqcli would: over the socket, with the password."""
        machine.succeed(as_user("dev", f"tmux new-session -d -s r{n} -x 170 -y 40 {shlex.quote(f'torqcli request system snapshot restore {n} {path}; echo done=$? > /tmp/rc; sleep 600')}"))
        machine.wait_until_succeeds(as_user("dev", f"tmux capture-pane -p -t r{n}") + " | grep -q 'assword'", timeout=60)
        machine.succeed("rm -f /tmp/rc")
        machine.succeed(as_user("dev", f"tmux send-keys -t r{n} {pw} Enter"))
        machine.wait_until_succeeds("test -e /tmp/rc", timeout=60)
        out = machine.succeed(as_user("dev", f"tmux capture-pane -p -t r{n}"))
        machine.succeed(as_user("dev", f"tmux kill-session -t r{n}"))
        return machine.succeed("cat /tmp/rc").strip(), " ".join(out.split())

    with subtest("a snapshot before each change: read only, root's, without the ledger"):
        machine.succeed(as_user("bob", "mkdir -p docs && echo one > docs/a && echo keep > docs/b"))
        machine.succeed("mkdir -p /var/lib/svcdata && echo v1 > /var/lib/svcdata/db && chown -R svc:svc /var/lib/svcdata")
        a = change(1)
        label = a["data"]["snapshot"]["label"]
        machine.succeed(f"btrfs property get -ts /.snapshots/{label}/home ro | grep -q true")
        machine.succeed(f"grep -q one /.snapshots/{label}/home/bob/docs/a")
        machine.fail(f"touch /.snapshots/{label}/home/x")
        machine.fail(as_user("bob", f"ls /.snapshots/{label}"))
        machine.fail(as_user("bob", "ls /.snapshots"))
        # The record is a subvolume of its own: not in a snapshot of /var/lib, so never rewound.
        machine.fail(f"test -e /.snapshots/{label}/var-lib/torq/state/ledger.jsonl")
        out = machine.succeed(as_user("dev", "torqcli show system snapshots"))
        assert f"#{a['seq']}" in out and "change-1" in out, out

    with subtest("a path comes back, as its owner, and what was there is kept"):
        machine.succeed(as_user("bob", "echo two > docs/a && rm docs/b"))
        rc, out = restore(a["seq"], "/home/bob/docs")
        assert rc == "done=0", out
        assert machine.succeed("cat /home/bob/docs/a").strip() == "one"
        assert machine.succeed("cat /home/bob/docs/b").strip() == "keep"
        assert machine.succeed("stat -c %U /home/bob/docs/a").strip() == "bob"
        assert machine.succeed(f"cat /home/bob/docs.before-{label}/a").strip() == "two"
        e = json.loads(machine.succeed("torq-broker log --json --brief | tail -1"))
        assert e["kind"] == "data_restored" and e["approved_by"] == "dev" and e["data"]["path"] == "/home/bob/docs", e

    with subtest("a service's whole folder comes back, in a folder of root's"):
        machine.succeed("echo v2-migrated > /var/lib/svcdata/db")
        rc, out = restore(a["seq"], "/var/lib/svcdata")
        assert rc == "done=0", out
        assert machine.succeed("cat /var/lib/svcdata/db").strip() == "v1"
        assert machine.succeed("stat -c %U:%a /var/lib/svcdata").strip().startswith("svc:")
        assert machine.succeed(f"cat /var/lib/svcdata.before-{label}/db").strip() == "v2-migrated"

    with subtest("a planted link goes nowhere its owner could not go"):
        # In the snapshot: refused. Live, where the path is to be written: followed as bob, not root.
        machine.succeed(as_user("bob", "mkdir -p trap && echo ok > trap/f && ln -s /etc trap/etc"))
        b = change(2)
        rc, out = restore(b["seq"], "/home/bob/trap/etc/shadow")
        assert rc != "done=0" and "is a link in the snapshot" in out, out
        machine.succeed(as_user("bob", "mv trap trap2 && ln -s /etc trap"))
        before = machine.succeed("sha256sum /etc/shadow /etc/passwd")
        rc, out = restore(b["seq"], "/home/bob/trap/f")
        assert rc != "done=0", out
        assert machine.succeed("sha256sum /etc/shadow /etc/passwd") == before, "root's files untouched"
        machine.fail("test -e /etc/f")

    with subtest("what may not be restored, and by whom"):
        for path, why in [("/var/lib/torq/state/ledger.jsonl", "machine's own"), ("/etc/shadow", "below /home and /var/lib"), ("/home/bob/../../etc", "no `..`")]:
            out = machine.fail(f"torq-broker restore-data {b['seq']} --path {path} --approved-by dev 2>&1")
            assert why in out, (path, out)
        # Not a person's to ask for without being an administrator, nor the agent's.
        out = machine.fail(as_user("bob", f"torqcli request system snapshot restore {b['seq']} /home/bob/docs") + " 2>&1")
        assert "not an administrator" in out, out
        out = machine.fail(f"su -s /bin/sh torq-agent -c 'torqcli request system snapshot restore {b['seq']} /home/bob/docs' 2>&1")
        assert "a person applies" in out, out

    with subtest("only the newest are kept"):
        for n in [3, 4, 5]:
            change(n)
        assert len(machine.succeed("ls /.snapshots").split()) == 3
        machine.fail(f"test -e /.snapshots/{label}")
        out = machine.succeed(as_user("dev", "torqcli show system snapshots"))
        assert "change-1" not in out and "change-5" in out, out
        machine.succeed("torq-broker verify")
  '';
}

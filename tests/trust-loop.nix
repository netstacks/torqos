# NixOS VM test: the M1 trust loop inside a real NixOS system.
# Rebuilds are stubbed (`true`) — this test proves broker/ledger/git behaviour, not nixos-rebuild.
{ pkgs, self }:

pkgs.testers.runNixOSTest {
  name = "torq-trust-loop";

  nodes.machine = { options, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
    # Rebuilds are stubbed through the broker's fixed settings (a root broker refuses a
    # TORQ_REBUILD_CMD from its caller). What a real machine would run is checked below.
    torq.broker.rebuildCommand = "true";
    environment.etc."torq-test/default-rebuild".text = options.torq.broker.rebuildCommand.default;
    # commit confirmed: the test activation and the boot default are stubbed too (no new system is
    # built, and the test VM has no boot loader); the rollback timer, the old broker it runs, and
    # the boot-time check are real.
    torq.broker.testCommand = "true";
    torq.broker.bootCommand = "true";
    specialisation.noguard.configuration.torq.broker.guardEtcNixos = false;
    # The test VM's disk is tiny: min-free would garbage-collect mid-test, deleting the unrooted
    # nixpkgs source the stray-rebuild check uses.
    nix.settings.min-free = 0;

    environment.etc."torq-test/desktop.nix".text = ''
      { nightLight = false; }
    '';
    environment.etc."torq-test/night-light.diff".text = ''
      --- a/desktop.nix
      +++ b/desktop.nix
      @@ -1 +1 @@
      -{ nightLight = false; }
      +{ nightLight = true; }
    '';
    environment.etc."torq-test/policy.diff".text = ''
      --- /dev/null
      +++ b/policy/tiers.toml
      @@ -0,0 +1 @@
      +agent_can_do_anything = true
    '';
  };

  testScript = ''
    machine.wait_for_unit("multi-user.target")
    repo = "/var/lib/torq/config"
    # The boot-time drift check ran (a unit that never ran also reports Result=success, so check
    # that it started and what it said), and passed on a machine with nothing ledgered yet.
    machine.succeed("test -n \"$(systemctl show -p ExecMainStartTimestamp --value torq-status)\"")
    machine.succeed("journalctl -b -u torq-status | grep -q 'OK as far as recorded'")
    machine.succeed("test ! -e /run/torq/drift")
    # The broker builds the fixed configuration `torq`, not one named after the host (this host is
    # "machine"): changing the host name must never leave rebuilds looking for a missing config.
    machine.succeed("grep -qx 'nixos-rebuild switch --flake {repo}#torq' /etc/torq-test/default-rebuild")
    machine.succeed("jq -e '.rebuild == \"true\" and .repo == \"/var/lib/torq/config\"' /etc/torq/broker.json")
    # Undo's tools come from fixed store paths, not only from the (changeable) running system.
    machine.succeed("jq -r .path /etc/torq/broker.json | tr : '\\n' | head -1 | grep -q '^/nix/store/.*nixos-rebuild'")

    machine.succeed(f"mkdir -p {repo} && cp /etc/torq-test/desktop.nix {repo}/ && touch {repo}/flake.nix")
    machine.succeed("torq-broker init")

    with subtest("init unit never re-runs on an initialised repo"):
        # Re-running would sweep out-of-band edits into a 'baseline' commit, and during a broker
        # apply it fails the switch on the broker lock.
        machine.succeed(f"echo tampered > {repo}/stray.nix")
        machine.succeed("systemctl start torq-broker-init")
        machine.succeed("systemctl show -p ConditionResult torq-broker-init | grep -qx ConditionResult=no")
        machine.succeed(f"git -C {repo} status --porcelain | grep -q stray.nix")
        machine.succeed(f"rm {repo}/stray.nix")

    with subtest("agent proposes, user approves, change lands"):
        out = machine.succeed("torq-broker propose --actor agent "
                              "--diff /etc/torq-test/night-light.diff --reason 'Night light'")
        assert "proposal #2" in out, out
        machine.succeed("torq-broker apply 2 --approved-by tester")
        machine.succeed(f"grep -q 'nightLight = true' {repo}/desktop.nix")
        machine.succeed(f"git -C {repo} log -1 --format=%B | grep -q 'Ledger-Entry: 3'")

    with subtest("one-command undo"):
        machine.succeed("torq-broker revert 3 --approved-by tester")
        machine.succeed(f"grep -q 'nightLight = false' {repo}/desktop.nix")

    with subtest("a root broker takes its settings only from /etc/torq/broker.json"):
        # A caller's override is refused, loudly, by variable or by flag.
        for override in ["TORQ_REBUILD_CMD='touch /tmp/pwned' torq-broker",
                         "TORQ_STATE_DIR=/tmp/fresh torq-broker",
                         "TORQ_CONFIG_REPO=/tmp/other torq-broker",
                         "torq-broker --state-dir /tmp/fresh"]:
            out = machine.fail(f"{override} verify 2>&1")
            assert "takes its settings only from /etc/torq/broker.json" in out, out
        machine.succeed("test ! -e /tmp/pwned -a ! -e /tmp/fresh")
        # Without the file, a root broker does not fall back to its caller's settings: it stops.
        machine.succeed("mv /etc/torq/broker.json /tmp/broker.json")
        out = machine.fail("TORQ_STATE_DIR=/tmp/fresh torq-broker verify 2>&1")
        assert "broker.json is missing" in out, out
        machine.succeed("mv /tmp/broker.json /etc/torq/broker.json")
        # Nothing of the caller's environment reaches git: a GIT_DIR elsewhere is ignored.
        machine.succeed("GIT_DIR=/nonexistent torq-broker status")

    with subtest("agent cannot touch policy"):
        machine.fail("torq-broker propose --actor agent "
                     "--diff /etc/torq-test/policy.diff --reason 'loosen permissions'")

    with subtest("ledger is tamper-evident"):
        machine.succeed("torq-broker verify")
        machine.succeed("cp /var/lib/torq/state/ledger.jsonl /tmp/ledger.good")
        machine.succeed("sed -i 's/Night light/Nothing here/' /var/lib/torq/state/ledger.jsonl")
        machine.fail("torq-broker verify")
        machine.succeed("systemctl start torq-status")  # never fails (it runs during switches)...
        machine.succeed("grep -q 'ledger' /run/torq/drift")  # ...but reports it
        machine.succeed("cp /tmp/ledger.good /var/lib/torq/state/ledger.jsonl")  # for what follows
        machine.succeed("systemctl start torq-status && test ! -e /run/torq/drift")

    with subtest("a stray nixos-rebuild cannot switch away from the ledgered config"):
        # The owner's files are moved aside, never overwritten or opened: a config that merely
        # mentions the option, and a symlink to a FIFO (reading it would hang activation at boot).
        machine.succeed("rm -f /etc/nixos/configuration.nix /etc/nixos/flake.nix /etc/nixos/*.pre-torq*")
        machine.succeed("echo '{ torq.broker.guardEtcNixos = false; }' > /etc/nixos/configuration.nix")
        machine.succeed("mkfifo /tmp/fifo && ln -s /tmp/fifo /etc/nixos/flake.nix")
        machine.succeed("timeout 120 /run/current-system/activate")
        machine.succeed("grep -qx '{ torq.broker.guardEtcNixos = false; }' /etc/nixos/configuration.nix.pre-torq")
        machine.succeed("test \"$(readlink /etc/nixos/flake.nix.pre-torq)\" = /tmp/fifo")
        # The stubs are real files; re-activating keeps them and makes no more copies.
        machine.succeed("timeout 120 /run/current-system/activate")
        machine.succeed("test -f /etc/nixos/flake.nix -a ! -L /etc/nixos/flake.nix")
        machine.succeed("test -f /etc/nixos/configuration.nix -a ! -L /etc/nixos/configuration.nix")
        machine.fail("ls /etc/nixos/*.pre-torq.*")
        # Building /etc/nixos refuses, every common way, with the message pointing at the broker.
        # (With flake.nix present nixos-rebuild uses it even when given -I, so --no-flake is what
        # reaches the configuration.nix stub.)
        for cmd in [
            "nixos-rebuild dry-build -I nixos-config=/etc/nixos/configuration.nix",  # what went wrong
            # The VM has no channels; the machine this happened on did, so supply nixpkgs.
            "nixos-rebuild dry-build --no-flake -I nixpkgs=${pkgs.path} -I nixos-config=/etc/nixos/configuration.nix",
            "nixos-rebuild dry-build",
            "nix --extra-experimental-features 'nix-command flakes' eval "
            "path:/etc/nixos#nixosConfigurations.machine.config.system.build.toplevel",
        ]:
            out = machine.fail(cmd + " 2>&1")
            assert "managed by TorqOS" in out, f"{cmd}: {out}"
        # Turning the guard off removes the stubs; the owner's files stay where they were put.
        machine.succeed("/run/current-system/specialisation/noguard/bin/switch-to-configuration test")
        machine.succeed("test ! -e /etc/nixos/flake.nix -a ! -e /etc/nixos/configuration.nix")
        machine.succeed("test -e /etc/nixos/configuration.nix.pre-torq -a -L /etc/nixos/flake.nix.pre-torq")

    with subtest("a machine switched outside the broker is reported"):
        # The guard subtest just switched to another configuration (the specialisation) without the
        # broker: the running system is no longer the last ledgered build. (That switch also ran the
        # check mid-switch, and succeeded: a monitor must not be able to fail a switch.)
        machine.succeed("systemctl start torq-status")
        machine.succeed("grep -q 'running system' /run/torq/drift")
        machine.succeed("journalctl -b -u torq-status -p warning | grep -q 'PROBLEM: running system'")
        out = machine.succeed("bash -ic true 2>&1")
        assert "not as its ledger says" in out, out
        # Back on the ledgered system, all is well again.
        machine.succeed("/run/booted-system/bin/switch-to-configuration test")
        machine.succeed("systemctl start torq-status")
        machine.succeed("test ! -e /run/torq/drift")
        out = machine.succeed("bash -ic true 2>&1")
        assert "not as its ledger says" not in out, out

    import json
    def broker(args):
        return json.loads(machine.succeed(f"torq-broker --json {args}"))
    def last_entry():
        return json.loads(machine.succeed("tail -n1 /var/lib/torq/state/ledger.jsonl"))
    def propose_night_light():
        return broker("propose --diff /etc/torq-test/night-light.diff --reason 'Night light'")["seq"]

    with subtest("commit confirmed: rolled back by its timer unless confirmed"):
        a = broker(f"apply {propose_night_light()} --approved-by tester --confirm 1")
        machine.succeed(f"grep -q 'nightLight = true' {repo}/desktop.nix")
        # A real transient timer, running the broker that made the change by its store path.
        unit = f"torq-confirm-{a['seq']}"
        machine.succeed(f"systemctl is-active {unit}.timer")
        machine.succeed(f"systemctl show -p ExecStart {unit}.service "
                        f"| grep -Eq 'argv\\[\\]=/nix/store/[^ ]+/bin/torq-broker expire-confirm {a['seq']} '")
        out = machine.succeed("torq-broker status")
        assert "rolls back at" in out, out
        out = machine.fail(f"torq-broker revert {a['seq']} --approved-by tester 2>&1")
        assert "waiting for confirmation" in out, out
        machine.wait_until_succeeds(f"grep -q 'nightLight = false' {repo}/desktop.nix", timeout=180)
        e = last_entry()
        assert e["kind"] == "rolled_back" and e["data"]["unconfirmed"] and not e["data"]["by_reboot"], e
        assert e["data"]["restored"] and e["data"]["applied"] == a["seq"], e
        machine.succeed("torq-broker status")

    with subtest("commit confirmed: confirmed in time, it stays"):
        a = broker(f"apply {propose_night_light()} --approved-by tester --confirm 1")
        c = broker("confirm --approved-by tester")
        assert c["kind"] == "confirmed" and c["data"]["applied"] == a["seq"], c
        # The timer still fires at the deadline, and finds nothing to roll back.
        machine.wait_until_succeeds(f"journalctl -u torq-confirm-{a['seq']}.service | grep -q 'nothing is waiting'", timeout=180)
        machine.succeed(f"grep -q 'nightLight = true' {repo}/desktop.nix")
        assert last_entry()["kind"] == "confirmed"
        machine.succeed("torq-broker status")
        broker(f"revert {a['seq']} --approved-by tester")  # back to nightLight = false

    with subtest("commit confirmed: a restart before confirming rolls back"):
        a = broker(f"apply {propose_night_light()} --approved-by tester --confirm 30")
        machine.succeed(f"grep -q 'nightLight = true' {repo}/desktop.nix")
        machine.shutdown()
        machine.start()
        machine.wait_for_unit("torq-confirm-boot.service")
        machine.succeed(f"grep -q 'nightLight = false' {repo}/desktop.nix")
        e = last_entry()
        assert e["kind"] == "rolled_back" and e["data"]["by_reboot"] and e["data"]["restored"], e
        assert e["data"]["applied"] == a["seq"], e
        machine.succeed("torq-broker status")
        machine.succeed("torq-broker verify")
  '';
}

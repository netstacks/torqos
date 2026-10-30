# NixOS VM test: the M1 trust loop inside a real NixOS system.
# Rebuilds are stubbed (`true`) — this test proves broker/ledger/git behaviour, not nixos-rebuild.
{ pkgs, self }:

pkgs.testers.runNixOSTest {
  name = "torq-trust-loop";

  nodes.machine = { ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
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
    env = "TORQ_REBUILD_CMD=true "
    repo = "/var/lib/torq/config"
    # The boot-time drift check ran (a unit that never ran also reports Result=success, so check
    # that it started and what it said), and passed on a machine with nothing ledgered yet.
    machine.succeed("test -n \"$(systemctl show -p ExecMainStartTimestamp --value torq-status)\"")
    machine.succeed("journalctl -b -u torq-status | grep -q 'OK as far as recorded'")
    machine.succeed("test ! -e /run/torq/drift")
    # The broker builds the fixed configuration `torq`, not one named after the host (this host is
    # "machine"): changing the host name must never leave rebuilds looking for a missing config.
    machine.succeed("grep -q \"flake {repo}#torq'\" \"$(readlink -f \"$(command -v torq-broker)\")\"")

    machine.succeed(f"mkdir -p {repo} && cp /etc/torq-test/desktop.nix {repo}/ && touch {repo}/flake.nix")
    machine.succeed(env + "torq-broker init")

    with subtest("init unit never re-runs on an initialised repo"):
        # Re-running would sweep out-of-band edits into a 'baseline' commit, and during a broker
        # apply it fails the switch on the broker lock.
        machine.succeed(f"echo tampered > {repo}/stray.nix")
        machine.succeed("systemctl start torq-broker-init")
        machine.succeed("systemctl show -p ConditionResult torq-broker-init | grep -qx ConditionResult=no")
        machine.succeed(f"git -C {repo} status --porcelain | grep -q stray.nix")
        machine.succeed(f"rm {repo}/stray.nix")

    with subtest("agent proposes, user approves, change lands"):
        out = machine.succeed(env + "torq-broker propose --actor agent "
                              "--diff /etc/torq-test/night-light.diff --reason 'Night light'")
        assert "proposal #2" in out, out
        machine.succeed(env + "torq-broker apply 2 --approved-by tester")
        machine.succeed(f"grep -q 'nightLight = true' {repo}/desktop.nix")
        machine.succeed(f"git -C {repo} log -1 --format=%B | grep -q 'Ledger-Entry: 3'")

    with subtest("one-command undo"):
        machine.succeed(env + "torq-broker revert 3 --approved-by tester")
        machine.succeed(f"grep -q 'nightLight = false' {repo}/desktop.nix")

    with subtest("agent cannot touch policy"):
        machine.fail(env + "torq-broker propose --actor agent "
                     "--diff /etc/torq-test/policy.diff --reason 'loosen permissions'")

    with subtest("ledger is tamper-evident"):
        machine.succeed(env + "torq-broker verify")
        machine.succeed("cp /var/lib/torq/state/ledger.jsonl /tmp/ledger.good")
        machine.succeed("sed -i 's/Night light/Nothing here/' /var/lib/torq/state/ledger.jsonl")
        machine.fail(env + "torq-broker verify")
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
  '';
}

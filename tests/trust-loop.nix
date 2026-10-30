# NixOS VM test: the M1 trust loop inside a real NixOS system.
# Rebuilds are stubbed (`true`) — this test proves broker/ledger/git behaviour, not nixos-rebuild.
{ pkgs, self }:

pkgs.testers.runNixOSTest {
  name = "kestrel-trust-loop";

  nodes.machine = { ... }: {
    imports = [ self.nixosModules.default ];
    kestrel.user = "dev";
    kestrel.desktop.enable = false;

    environment.etc."kestrel-test/desktop.nix".text = ''
      { nightLight = false; }
    '';
    environment.etc."kestrel-test/night-light.diff".text = ''
      --- a/desktop.nix
      +++ b/desktop.nix
      @@ -1 +1 @@
      -{ nightLight = false; }
      +{ nightLight = true; }
    '';
    environment.etc."kestrel-test/policy.diff".text = ''
      --- /dev/null
      +++ b/policy/tiers.toml
      @@ -0,0 +1 @@
      +agent_can_do_anything = true
    '';
  };

  testScript = ''
    machine.wait_for_unit("multi-user.target")
    env = "KESTREL_REBUILD_CMD=true "
    repo = "/var/lib/kestrel/config"

    machine.succeed(f"mkdir -p {repo} && cp /etc/kestrel-test/desktop.nix {repo}/ && touch {repo}/flake.nix")
    machine.succeed(env + "kestrel-broker init")

    with subtest("init unit never re-runs on an initialised repo"):
        # Re-running would sweep out-of-band edits into a 'baseline' commit, and during a broker
        # apply it fails the switch on the broker lock.
        machine.succeed(f"echo tampered > {repo}/stray.nix")
        machine.succeed("systemctl start kestrel-broker-init")
        machine.succeed("systemctl show -p ConditionResult kestrel-broker-init | grep -qx ConditionResult=no")
        machine.succeed(f"git -C {repo} status --porcelain | grep -q stray.nix")
        machine.succeed(f"rm {repo}/stray.nix")

    with subtest("agent proposes, user approves, change lands"):
        out = machine.succeed(env + "kestrel-broker propose --actor agent "
                              "--diff /etc/kestrel-test/night-light.diff --reason 'Night light'")
        assert "proposal #2" in out, out
        machine.succeed(env + "kestrel-broker apply 2 --approved-by tester")
        machine.succeed(f"grep -q 'nightLight = true' {repo}/desktop.nix")
        machine.succeed(f"git -C {repo} log -1 --format=%B | grep -q 'Ledger-Entry: 3'")

    with subtest("one-command undo"):
        machine.succeed(env + "kestrel-broker rollback 3 --approved-by tester")
        machine.succeed(f"grep -q 'nightLight = false' {repo}/desktop.nix")

    with subtest("agent cannot touch policy"):
        machine.fail(env + "kestrel-broker propose --actor agent "
                     "--diff /etc/kestrel-test/policy.diff --reason 'loosen permissions'")

    with subtest("ledger is tamper-evident"):
        machine.succeed(env + "kestrel-broker verify")
        machine.succeed("sed -i 's/Night light/Nothing here/' /var/lib/kestrel/state/ledger.jsonl")
        machine.fail(env + "kestrel-broker verify")

    with subtest("a stray nixos-rebuild cannot switch away from the ledgered config"):
        # Someone's own files survive activation, moved aside: a file, and a symlink (kept as a link).
        machine.succeed("rm -f /etc/nixos/configuration.nix /etc/nixos/flake.nix /etc/nixos/*.pre-kestrel*")
        machine.succeed("echo '{ mine = true; }' > /etc/nixos/configuration.nix")
        machine.succeed("ln -s /etc/hostname /etc/nixos/flake.nix")
        machine.succeed("/run/current-system/activate")
        machine.succeed("grep -qx '{ mine = true; }' /etc/nixos/configuration.nix.pre-kestrel")
        machine.succeed("test \"$(readlink /etc/nixos/flake.nix.pre-kestrel)\" = /etc/hostname")
        # The stubs are real files, and re-activating keeps them without making more copies.
        machine.succeed("/run/current-system/activate")
        machine.succeed("test ! -L /etc/nixos/flake.nix && grep -q guardEtcNixos /etc/nixos/flake.nix")
        machine.succeed("test ! -L /etc/nixos/configuration.nix && grep -q guardEtcNixos /etc/nixos/configuration.nix")
        machine.fail("ls /etc/nixos/*.pre-kestrel.*")
        # Building /etc/nixos refuses, every common way, with the message pointing at the broker.
        for cmd in [
            "nixos-rebuild dry-build -I nixos-config=/etc/nixos/configuration.nix",  # what went wrong
            "nixos-rebuild dry-build",
            "nix-instantiate --eval /etc/nixos/configuration.nix",
            "nix --extra-experimental-features 'nix-command flakes' eval "
            "path:/etc/nixos#nixosConfigurations.machine.config.system.build.toplevel",
        ]:
            out = machine.fail(cmd + " 2>&1")
            assert "managed by Kestrel" in out, f"{cmd}: {out}"

  '';
}

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

    machine.succeed(f"mkdir -p {repo} && cp /etc/kestrel-test/desktop.nix {repo}/")
    machine.succeed(env + "kestrel-broker init")

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
  '';
}

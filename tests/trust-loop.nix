# NixOS VM test: the M1 trust loop inside a real NixOS system.
# Rebuilds are stubbed (`true`) — this test proves broker/ledger/git behaviour, not nixos-rebuild.
{ pkgs, self }:

pkgs.testers.runNixOSTest {
  name = "torq-trust-loop";

  nodes.machine = { options, pkgs, ... }: {
    # A terminal to drive torqcli's line editor in.
    environment.systemPackages = [ pkgs.tmux ];
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
    # Rebuilds are stubbed through the broker's fixed settings (a root broker refuses a
    # TORQ_REBUILD_CMD from its caller). What a real machine would run is checked below.
    torq.broker.rebuildCommand = "true";
    environment.etc."torq-test/default-rebuild".text = options.torq.broker.rebuildCommand.default;
    # commit confirmed: the "build" is a prebuilt specialisation (the VM cannot build systems), and
    # the boot default is stubbed (the test VM has no boot loader). The activations — into that
    # system and back — the rollback timer, the old broker it runs, and the boot-time check are real.
    specialisation.confirmtarget.configuration.environment.etc."torq-test/new-system".text = "yes";
    torq.broker.buildCommand = "readlink -f /run/booted-system/specialisation/confirmtarget";
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
    environment.etc."torq-test/host.diff".text = ''
      --- /dev/null
      +++ b/system/host.json
      @@ -0,0 +1 @@
      +{ "host-name": "machine" }
    '';
    environment.etc."torq-test/policy.diff".text = ''
      --- /dev/null
      +++ b/policy/tiers.toml
      @@ -0,0 +1 @@
      +agent_can_do_anything = true
    '';
  };

  testScript = ''
    import json
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

    with subtest("torqcli reads the configuration as a user, by git plumbing only"):
        out = machine.succeed("torq-broker --json propose --diff /etc/torq-test/host.diff --reason host")
        machine.succeed(f"torq-broker apply {json.loads(out)['seq']} --approved-by tester")
        # The repo belongs to root; an ordinary user reads it (safe.directory), and nothing in their
        # environment points git elsewhere or configures it.
        hostile = "GIT_DIR=/nonexistent GIT_CONFIG_PARAMETERS=bogus GIT_INDEX_FILE=/nonexistent"
        out = machine.succeed(f"su - dev -c '{hostile} torqcli -c \"show configuration\"'")
        assert out == "system {\n    host-name machine;\n}\n", out
        out = machine.succeed("su - dev -c 'torqcli -c \"configure; set system time-zone UTC; show | compare\"'")
        assert "[edit system]\n+   time-zone UTC;" in out, out
        # -c uses a private candidate; the interactive shell keeps one, private, between sessions.
        machine.succeed("su - dev -c 'test ! -e ~/.local/state/torq/candidate.json'")
        machine.succeed("su - dev -c 'printf \"configure\\nset users dev cli style cisco\\nexit\\nexit\\n\" | torqcli'")
        machine.succeed("su - dev -c 'test \"$(stat -c %a ~/.local/state/torq/candidate.json)\" = 600'")
        out = machine.succeed("su - dev -c 'torqcli -c \"show system commit\"'")
        assert " host" in out.splitlines()[0], out
        out = machine.fail("su - dev -c 'torqcli -c \"configure; set system host-name my_laptop\"' 2>&1")
        assert "is not a valid system host-name" in out, out

    with subtest("torqcli on a terminal: ? lists what may come next, Tab completes"):
        import shlex
        def as_dev(cmd, ok=True):
            run = machine.succeed if ok else machine.fail
            return run(f"su - dev -c {shlex.quote(cmd)}")
        screen = "tmux capture-pane -p -t t"
        as_dev("tmux new-session -d -s t -x 160 -y 50 torqcli")
        machine.wait_until_succeeds(f"su - dev -c {shlex.quote(screen + ' | grep -q dev@machine')}", timeout=30)
        as_dev("tmux send-keys -t t configure Enter")
        machine.wait_until_succeeds(f"su - dev -c {shlex.quote(screen + ' | grep -q dev@machine#')}", timeout=30)
        as_dev("tmux send-keys -t t 'set system cli style ' '?'")
        machine.wait_until_succeeds(f"su - dev -c {shlex.quote(screen + ' | grep -q Possible.completions')}", timeout=30)
        out = as_dev(screen)
        assert "cisco" in out and "IOS-style" in out and "juniper" in out, out
        # The line is still there after the list, ready to finish.
        assert out.rstrip().splitlines()[-1].endswith("set system cli style"), out
        as_dev("tmux send-keys -t t C-u 'set system ti' Tab")
        # (capture-pane drops trailing spaces, so the space Tab adds is not seen here.)
        machine.wait_until_succeeds(f"su - dev -c {shlex.quote(screen + ' | grep -q \"set system time-zone\"')}", timeout=30)
        # A pasted block stops at its first error: nothing after a refused line runs.
        as_dev("tmux send-keys -t t C-u")
        as_dev("printf 'set system host-name my_laptop\\nset users dev packages vim\\n' | tmux load-buffer - && tmux paste-buffer -p -t t")
        as_dev("tmux send-keys -t t Enter")
        machine.wait_until_succeeds(f"su - dev -c {shlex.quote(screen + ' | grep -q \"pasted lines after it were not run\"')}", timeout=30)
        as_dev("tmux send-keys -t t 'show | compare' Enter")
        machine.sleep(3)
        after = as_dev(screen).split("were not run")[-1]
        assert "show | compare" in after and "vim" not in after, after
        # A pasted block of commands (bracketed paste) runs line by line.
        as_dev("tmux send-keys -t t C-u")
        as_dev("printf 'set users dev packages htop\\nshow | compare\\n' | tmux load-buffer - && tmux paste-buffer -p -t t")
        as_dev("tmux send-keys -t t Enter")
        # (the compare's output, not the pasted command: both lines ran)
        machine.wait_until_succeeds(f"su - dev -c {shlex.quote(screen + ' | grep -qF \"+   packages [ htop ];\"')}", timeout=30)
        # Leave (one line at a time: keys typed before the next prompt are not kept).
        last_line = screen + " | grep -v '^$' | tail -n 1"
        as_dev("tmux send-keys -t t exit Enter")
        machine.wait_until_succeeds(f"su - dev -c {shlex.quote(last_line + ' | grep -q \"dev@machine>\"')}", timeout=30)
        as_dev("tmux send-keys -t t exit Enter")
        machine.wait_until_fails(f"su - dev -c {shlex.quote('tmux has-session -t t')}", timeout=30)

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

    def broker(args):
        return json.loads(machine.succeed(f"torq-broker --json {args}"))
    def last_entry():
        return json.loads(machine.succeed("tail -n1 /var/lib/torq/state/ledger.jsonl"))
    def propose_night_light():
        return broker("propose --diff /etc/torq-test/night-light.diff --reason 'Night light'")["seq"]

    booted = machine.succeed("readlink -f /run/booted-system").strip()
    new_system = machine.succeed("readlink -f /run/booted-system/specialisation/confirmtarget").strip()
    def running():
        return machine.succeed("readlink -f /run/current-system").strip()

    with subtest("commit confirmed: rolled back by its timer unless confirmed, retrying while busy"):
        assert running() == booted
        a = broker(f"apply {propose_night_light()} --approved-by tester --confirm 1")
        # The new system really runs; the boot default is untouched.
        assert running() == new_system and a["data"]["system"] == new_system, a
        machine.succeed("test -e /etc/torq-test/new-system")
        machine.succeed(f"grep -q 'nightLight = true' {repo}/desktop.nix")
        # A real wall-clock timer, running the broker that made the change by its store path.
        unit = f"torq-confirm-{a['seq']}"
        machine.succeed(f"systemctl is-active {unit}.timer")
        machine.succeed(f"systemctl show -p TimersCalendar {unit}.timer | grep -q 'UTC'")
        machine.succeed(f"systemctl show -p ExecStart {unit}.service "
                        f"| grep -Eq 'argv\\[\\]=/nix/store/[^ ]+/bin/torq-broker expire-confirm {a['seq']} '")
        out = machine.succeed("torq-broker status")
        assert "rolls back at" in out, out
        out = machine.fail(f"torq-broker revert {a['seq']} --approved-by tester 2>&1")
        assert "waiting for confirmation" in out, out
        # Another operation holds the broker lock past the deadline: the timer's run is refused as
        # busy, and retried until it gets through.
        machine.succeed("systemd-run --unit=hold-lock flock /var/lib/torq/state/broker.lock "
                        "/run/current-system/sw/bin/sleep 100")
        machine.succeed("systemctl is-active hold-lock")
        machine.wait_until_succeeds(f"journalctl -u {unit}.service | grep -q 'another broker operation is running'", timeout=150)
        assert running() == new_system, "not rolled back while busy"
        # The ledger entry is the last step (the config is reset first, then the system activated).
        machine.wait_until_succeeds("tail -n1 /var/lib/torq/state/ledger.jsonl | grep -q rolled_back", timeout=180)
        machine.succeed(f"grep -q 'nightLight = false' {repo}/desktop.nix")
        e = last_entry()
        assert e["kind"] == "rolled_back" and e["data"]["unconfirmed"] and not e["data"]["by_reboot"], e
        assert e["data"]["restored"] and e["data"]["applied"] == a["seq"], e
        assert running() == booted, "the old system runs again"
        machine.succeed("test ! -e /etc/torq-test/new-system")
        machine.succeed("torq-broker status")

    with subtest("commit confirmed: a restart before confirming rolls back"):
        a = broker(f"apply {propose_night_light()} --approved-by tester --confirm 30")
        assert running() == new_system
        machine.shutdown()
        machine.start()
        machine.wait_for_unit("multi-user.target")
        assert running() == booted, "the boot default was never changed"
        machine.wait_until_succeeds("tail -n1 /var/lib/torq/state/ledger.jsonl | grep -q rolled_back", timeout=120)
        e = last_entry()
        assert e["data"]["by_reboot"] and e["data"]["restored"] and e["data"]["applied"] == a["seq"], e
        machine.succeed(f"grep -q 'nightLight = false' {repo}/desktop.nix")
        machine.succeed("torq-broker status")
        machine.succeed("torq-broker verify")

    with subtest("commit confirmed: confirmed in time, it stays"):
        a = broker(f"apply {propose_night_light()} --approved-by tester --confirm 1")
        c = broker("confirm --approved-by tester")
        assert c["kind"] == "confirmed" and c["data"]["applied"] == a["seq"], c
        # The timer still fires at the deadline, and finds nothing to roll back.
        machine.wait_until_succeeds(f"journalctl -u torq-confirm-{a['seq']}.service | grep -q 'nothing is waiting'", timeout=180)
        assert running() == new_system
        machine.succeed(f"grep -q 'nightLight = true' {repo}/desktop.nix")
        assert last_entry()["kind"] == "confirmed"
        machine.succeed("torq-broker status")
  '';
}

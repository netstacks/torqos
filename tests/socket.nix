# NixOS VM test: the broker on a socket (ADR-040). Who is asking is the kernel's answer: each claim
# the ADR makes is checked here against the broker itself, over its socket, and through torqcli.
# Rebuilds are stubbed (`true`), as in the trust loop: this proves who may do what, not the build.
{ pkgs, self }:

let
  # The test's own passwords (crypt SHA-512, fixed salts): dev is the owner (wheel), sam is not.
  devHash = "$6$torqsocketsalt$Z7RLey3l5vKGfukhfuZYYM/EjWNo7QM8.65r88XmZ9z3/jBCY.NAnH7RwYpiLoE2fedxUd/I7UFcKDX1KEXDg/";
  samHash = "$6$torqsocketsal2$p4ihhGlRahyPpWY43xyA52iNOzVtnXF1dQCVxf0eBeRehXODXyWOdptk3znfwCfmhj5ERP28izhmIRqrAL5J00";
in
pkgs.testers.runNixOSTest {
  name = "torq-socket";

  nodes.machine = { pkgs, lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPassword = devHash;
    users.users.sam = { isNormalUser = true; hashedPasswordFile = lib.mkForce null; hashedPassword = samHash; };
    # The agent's account (ADR-035; the module that makes it is M3's next item): what arrives from
    # it is the agent.
    users.users.torq-agent = { isSystemUser = true; group = "torq-agent"; };
    users.groups.torq-agent = { };
    torq.broker.rebuildCommand = "true";
    # socat speaks to the socket; jq reads the answers; tmux is a terminal to type a password at.
    environment.systemPackages = [ pkgs.socat pkgs.jq pkgs.tmux pkgs.pam ];
    environment.etc."torq-test/desktop.nix".text = "{ nightLight = false; }\n";
    environment.etc."torq-test/system.diff".text = ''
      --- a/desktop.nix
      +++ b/desktop.nix
      @@ -1 +1 @@
      -{ nightLight = false; }
      +{ nightLight = true; }
    '';
  };

  testScript = ''
    import json, shlex
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("torq-broker.socket")
    repo = "/var/lib/torq/config"
    machine.succeed(f"mkdir -p {repo} && cp /etc/torq-test/desktop.nix {repo}/ && touch {repo}/flake.nix")
    machine.succeed("torq-broker init")

    def ask(user, request):
        """One request over the socket as `user`: (exit code, stdout, stderr), from the frames."""
        line = json.dumps(request)
        cmd = f"printf '%s\\n' {shlex.quote(line)} | socat -t 30 - UNIX-CONNECT:/run/torq/broker.sock"
        raw = machine.succeed(f"su -s /bin/sh {user} -c {shlex.quote(cmd)}")
        code, out, err = None, "", ""
        for l in raw.splitlines():
            f = json.loads(l)
            out += f.get("out", "")
            err += f.get("err", "")
            if "exit" in f:
                code = f["exit"]
        return code, out, err

    def answer(code_out_err):
        code, out, _ = code_out_err
        return code, json.loads(out) if out.strip() else None

    def own_diff(name):
        return f"--- /dev/null\n+++ b/users/{name}/packages.json\n@@ -0,0 +1 @@\n+[\"hello\"]\n"

    def head():
        return machine.succeed(f"git -C {repo} rev-parse HEAD").strip()

    def propose(user, diff, reason):
        code, v = answer(ask(user, {"args": ["--json", "propose", f"--base={head()}", f"--reason={reason}"], "diff": diff}))
        assert code == 0, (code, v)
        return v

    with subtest("the socket is anyone's to reach; what each may do is decided inside"):
        machine.succeed("test \"$(stat -c %a /run/torq/broker.sock)\" = 666")
        # Reading is open to every account, the agent's included (no sudo, nothing secret).
        for who in ["dev", "sam", "torq-agent"]:
            code, v = answer(ask(who, {"args": ["--json", "verify"]}))
            assert code == 0 and v["entries"] == 1, (who, code, v)
        # Root runs the broker itself: not through the socket.
        code, v = answer(ask("root", {"args": ["--json", "verify"]}))
        assert code == 1 and "root runs torq-broker itself" in v["error"], v

    with subtest("no caller names itself, a file, or the broker's settings"):
        for args in [["--json", "apply", "2", "--approved-by=dev"], ["--json", "propose", "--actor", "user"],
                     ["--json", "propose", "--diff=/etc/shadow"], ["--json", "password", "--user", "sam", "--hash-file", "/etc/shadow"],
                     ["--json", "--rebuild-cmd=touch /tmp/pwned", "status"], ["--json", "--state-dir=/tmp/x", "log"]]:
            code, v = answer(ask("dev", {"args": args}))
            assert code == 1 and "not taken from a caller" in v["error"], (args, code, v)
        machine.succeed("test ! -e /tmp/pwned")
        # Fields the broker does not read are not quietly dropped: the request is refused.
        code, out, err = ask("dev", {"args": ["verify"], "actor": "dev"})
        assert code == 2 and "not one the broker reads" in err, (code, out, err)

    with subtest("a person's own settings are theirs: no password, no sudo"):
        p = propose("sam", own_diff("sam"), "hello for sam")
        assert p["actor"] == "user" and p["tier"] == "user" and p["data"]["via"] == "socket", p
        code, v = answer(ask("sam", {"args": ["--json", "apply", str(p["seq"])]}))
        assert code == 0 and v["approved_by"] == "sam" and v["data"]["via"] == "socket", (code, v)
        # Someone else's are not theirs (KNOWN-GAPS 6a): sam cannot apply dev's.
        p = propose("sam", own_diff("dev"), "hello for dev, by sam")
        code, v = answer(ask("sam", {"args": ["--json", "apply", str(p["seq"])]}))
        assert code == 1 and "not an administrator" in v["error"], (code, v)

    with subtest("a system change asks an administrator's password, checked by PAM"):
        p = propose("dev", machine.succeed("cat /etc/torq-test/system.diff"), "night light")
        seq = str(p["seq"])
        assert p["tier"] == "system", p
        # Not an administrator: refused, whatever the password.
        code, v = answer(ask("sam", {"args": ["--json", "apply", seq], "password": "Sam-sock-7742"}))
        assert code == 1 and "not an administrator" in v["error"], (code, v)
        # The owner: without the password, asked for it (77); a wrong one, refused and counted.
        code, v = answer(ask("dev", {"args": ["--json", "apply", seq]}))
        assert code == 77 and v["password"] and "needs dev's password" in v["error"], (code, v)
        code, v = answer(ask("dev", {"args": ["--json", "apply", seq], "password": "wrong-guess"}))
        assert code == 77 and "not dev's password" in v["error"], (code, v)
        machine.succeed("test $(faillock --user dev | grep -c '^[0-9]') -ge 1")
        machine.succeed(f"git -C {repo} show HEAD:desktop.nix | grep -q 'nightLight = false'")
        # The right one: applied, ledgered as dev's, through the socket.
        code, v = answer(ask("dev", {"args": ["--json", "apply", seq], "password": "Torq-sock-2210"}))
        assert code == 0 and v["approved_by"] == "dev" and v["data"]["via"] == "socket", (code, v)
        night = v["seq"]
        machine.succeed(f"git -C {repo} show HEAD:desktop.nix | grep -q 'nightLight = true'")
        machine.succeed("faillock --user dev --reset")

    with subtest("guessing stops: five wrong, locked, even with the right one"):
        # Someone else's folder: an administrator's password.
        p = propose("dev", own_diff("torq-agent"), "for the agent's folder, by dev")
        for _ in range(5):
            code, _v = answer(ask("dev", {"args": ["--json", "apply", str(p["seq"])], "password": "guess"}))
            assert code == 77
        code, v = answer(ask("dev", {"args": ["--json", "apply", str(p["seq"])], "password": "Torq-sock-2210"}))
        assert code == 77, ("locked out", code, v)
        machine.succeed("faillock --user dev --reset")
        code, v = answer(ask("dev", {"args": ["--json", "apply", str(p["seq"])], "password": "Torq-sock-2210"}))
        assert code == 0, (code, v)

    with subtest("the agent proposes, and can do nothing else"):
        p = propose("torq-agent", own_diff("dev"), "the agent suggests hello")
        assert p["actor"] == "agent", p
        for args in [["apply", str(p["seq"])], ["revert", "3"], ["confirm"], ["reconcile"], ["repair"], ["password", "--user", "dev"]]:
            code, v = answer(ask("torq-agent", {"args": ["--json"] + args, "password": "Torq-sock-2210", "hash": "$6$x$y"}))
            assert code == 1 and "the agent proposes; a person applies" in v["error"], (args, code, v)
        # The machine's own commands are not on the socket for anyone.
        for args in [["init"], ["setup"], ["expire-confirm"], ["serve"]]:
            code, v = answer(ask("dev", {"args": ["--json"] + args}))
            assert code == 1 and "not run over the socket" in v["error"], (args, code, v)

    with subtest("torqcli: no sudo to read the ledger; a system revert asks the password at the terminal"):
        machine.fail("su - sam -c 'sudo -n true'")
        out = machine.succeed("su - sam -c 'torqcli show system ledger'")
        assert "hello for sam" in out and "night light" in out, out
        # Without a terminal: nobody to ask, nothing changed.
        before = head()
        machine.fail(f"su - dev -c 'torqcli request system revert {night}' < /dev/null")
        assert head() == before
        # At a terminal: the question, then the password — typed, not echoed.
        machine.succeed(f"su - dev -c \"tmux new-session -d -s c -x 160 -y 40 'torqcli request system revert {night}; sleep 600'\"")
        pane = lambda: machine.succeed("su - dev -c 'tmux capture-pane -p -t c'")
        machine.wait_until_succeeds("su - dev -c 'tmux capture-pane -p -t c' | grep -q 'y/N'", timeout=120)
        machine.succeed("su - dev -c 'tmux send-keys -t c y Enter'")
        machine.wait_until_succeeds("su - dev -c 'tmux capture-pane -p -t c' | grep -q 'Password for dev:'", timeout=120)
        machine.succeed("su - dev -c 'tmux send-keys -t c Torq-sock-2210 Enter'")
        machine.wait_until_succeeds(f"git -C {repo} show HEAD:desktop.nix | grep -q 'nightLight = false'", timeout=120)
        last = json.loads(machine.succeed("torq-broker log --json --brief | tail -1"))
        assert last["kind"] == "reverted" and last["approved_by"] == "dev" and last["data"]["via"] == "socket", last
        assert "Torq-sock-2210" not in pane(), pane()

    with subtest("a caller that hangs up stops nothing"):
        p = propose("dev", own_diff("dev").replace("hello", "jq"), "jq for dev")
        line = json.dumps({"args": ["--json", "apply", str(p["seq"])]})
        # Send, and leave at once: the change still lands, and is ledgered.
        send = f"printf '%s\\n' {shlex.quote(line)} | timeout 0.3 socat - UNIX-CONNECT:/run/torq/broker.sock || true"
        machine.succeed(f"su -s /bin/sh dev -c {shlex.quote(send)}")
        machine.wait_until_succeeds(f"torq-broker log --json --brief | jq -e 'select(.kind == \"applied\" and .data.proposal == {p['seq']})'", timeout=60)
        machine.succeed("torq-broker verify && torq-broker status")
        # No request's files are left behind.
        machine.succeed("test -z \"$(ls /var/lib/torq/state | grep '^request-' || true)\"")

    with subtest("sudo stays the recovery door, and is ledgered as what root says about itself"):
        # sudo sets SUDO_UID; root at a shell, and the machine's own units, do not.
        machine.succeed(f"printf %s {shlex.quote(own_diff('nobody'))} > /tmp/by-hand.diff")
        v = json.loads(machine.succeed(f"SUDO_UID=1000 torq-broker --json propose --actor user --base {head()} --diff /tmp/by-hand.diff --reason by-hand"))
        assert v["data"]["via"] == "sudo", v
        v = json.loads(machine.succeed(f"torq-broker --json propose --actor user --base {head()} --diff /tmp/by-hand.diff --reason by-root"))
        assert "via" not in v["data"], v
        machine.succeed("torq-broker verify")
  '';
}

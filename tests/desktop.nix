# NixOS VM test: TorqOS Desktop (ADR-045) boots to COSMIC's greeter, and the owner's password logs in to
# a COSMIC session. On it: NetworkManager with iwd as its Wi-Fi backend (one Wi-Fi stack, ADR-046 2), no
# wpa_supplicant; Flatpak with Flathub for the person only, never the system's (ADR-037, amended).
# An agent's use of a secret that waits is seen by the panel's applet (ADR-051, ADR-052 4): its dialog is not driven.
# Smoothness is judged on real hardware, not here.
{ pkgs, self }:

let
  # The owner's password, `torq-desktop`, as the broker keeps it (modules/accounts.nix).
  hash = "$6$torqtestsalt$wXsKSX3z/kTvGvAzmdrks4iI95ZmWWtQgfYUMV/OEbQ7h7HZxSpMJYh0hvWPma/izeabeQYfkEhhQhWA52xbE1";
in
pkgs.testers.runNixOSTest {
  name = "torq-desktop";
  # The desktop sets nixpkgs.overlays (COSMIC from its own nixpkgs), so the nodes build their own package set.
  node.pkgsReadOnly = false;

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "alice";
    torq.desktop.enable = true;
    torq.desktop.dimInsteadOfSuspend = true;
    # As every TorqOS machine has it (lib.mkSystem); the desktop reads it to take COSMIC from its own nixpkgs.
    nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
    # NixOS's test instrumentation gives root an empty password; TorqOS asserts root has none.
    users.users.root.hashedPasswordFile = lib.mkForce null;
    # COSMIC's session and its applications: NixOS's own COSMIC test gives it 4 GB.
    virtualisation.memorySize = 4096;
    # What a change does here does not matter: this proves who may approve it, and how.
    torq.broker.rebuildCommand = "true";
    environment.etc."torq-test/desktop.nix".text = "{ nightLight = false; }\n";
    environment.etc."torq-test/system.diff".text = ''
      --- a/desktop.nix
      +++ b/desktop.nix
      @@ -1 +1 @@
      -{ nightLight = false; }
      +{ nightLight = true; }
    '';
    # torq-secret-set-example: TorqOS Settings' way of keeping a secret's value, without the window.
    environment.systemPackages = [ pkgs.pam (import ./secret-set-example.nix { inherit pkgs self; }) ];
  };

  testScript = ''
    import json, shlex
    machine.wait_for_unit("multi-user.target")
    # The owner's password, set as setup sets it, then the accounts written from it.
    machine.succeed("install -d -m 700 /var/lib/torq/secrets/passwords"
                    " && echo '${hash}' > /var/lib/torq/secrets/passwords/alice"
                    " && /run/current-system/activate")

    with subtest("the greeter"):
        machine.wait_for_unit("graphical.target", timeout=180)
        machine.wait_until_succeeds("pgrep -u cosmic-greeter -f cosmic-greeter", timeout=60)
        # greetd draws the password field a little after the greeter starts.
        machine.sleep(10)
        machine.screenshot("greeter")

    with subtest("the owner logs in to COSMIC"):
        # Typed again if the greeter had not yet drawn its password field (on a slower host it may not have).
        for attempt in range(4):
            machine.send_chars("torq-desktop\n", delay=0.2)
            if machine.execute("timeout 60 sh -c 'until pgrep -u alice -fx cosmic-panel; do sleep 2; done'")[0] == 0:
                break
            machine.sleep(5)
        # COSMIC's programs run wrapped: their names are the wrapper's, their command lines their own.
        machine.wait_until_succeeds("pgrep -u alice -fx cosmic-comp", timeout=120)
        machine.wait_until_succeeds("pgrep -u alice -fx cosmic-panel", timeout=120)
        machine.sleep(10)
        machine.screenshot("session")

    with subtest("one Wi-Fi stack: NetworkManager on iwd"):
        machine.wait_for_unit("NetworkManager.service")
        machine.succeed("NetworkManager --print-config | grep -qx 'wifi.backend=iwd'")
        machine.fail("systemctl cat wpa_supplicant.service")

    with subtest("TorqOS Settings is in the menu"):
        machine.succeed("test -e /run/current-system/sw/share/applications/org.torqos.Settings.desktop")
        machine.succeed("test -x /run/current-system/sw/bin/torq-settings")
        # And TorqOS in the panel (ADR-049): an applet COSMIC offers in its panel settings.
        machine.succeed("grep -qx X-CosmicApplet=true /run/current-system/sw/share/applications/org.torqos.Applet.desktop")
        machine.succeed("test -x /run/current-system/sw/bin/torq-applet")
        # Started by the panel through the wrapper that makes a death by a signal an exit with a code.
        machine.succeed("test -x /run/current-system/sw/bin/torq-applet-run")
        machine.succeed("grep -qx 'Exec=torq-applet-run torq-applet' /run/current-system/sw/share/applications/org.torqos.Applet.desktop")
        # In the panel by default, running there: a new machine has it without anyone adding it.
        machine.wait_until_succeeds("pgrep -u alice -fx torq-applet", timeout=60)

    with subtest("COSMIC's own first-run wizard is not offered: TorqOS has its setup"):
        machine.fail("test -e /run/current-system/sw/bin/cosmic-initial-setup")

    with subtest("COSMIC starts its panel again by itself, with the applet in it: what a switch relies on"):
        panel = machine.succeed("pgrep -u alice -fx cosmic-panel").strip()
        machine.succeed("pkill -u alice -fx cosmic-panel")
        machine.wait_until_succeeds(f"pgrep -u alice -fx cosmic-panel | grep -vx {panel}", timeout=60)
        machine.wait_until_succeeds("pgrep -u alice -fx torq-applet", timeout=60)
        machine.succeed("pgrep -u alice -fx cosmic-comp")

    with subtest("an applet a signal ends is started again by the panel, through its wrapper; the wrapper's own death is the known gap"):
        # The panel starts an applet again only after an exit with a code, read in cosmic-panel 1.11's source
        # (launch-pad 1.0.1). The panel starts each TorqOS applet as `torq-applet-run <applet>` (the desktop item's
        # Exec), whose child is the real applet and which exits with 128 + the signal when a signal ended it. The
        # names: the wrapper's command line is "torq-applet-run torq-applet" and the applet's is "torq-applet", which
        # is what `pgrep -fx torq-applet` finds, here and in the other tests.
        for sig in ["KILL", "SEGV", "TERM"]:
            applet = machine.succeed("pgrep -u alice -fx torq-applet").strip()
            wrapper = machine.succeed(f"ps -o ppid= -p {applet}").strip()
            assert machine.succeed(f"cat /proc/{wrapper}/cmdline | tr '\\0' ' '").strip() == "torq-applet-run torq-applet", wrapper
            machine.succeed(f"kill -{sig} {applet}")
            machine.wait_until_succeeds(f"pgrep -u alice -fx torq-applet | grep -vx {applet}", timeout=60)
        # The wrapper itself killed: the applet goes with it (it asks for that), and the panel, which reads a
        # death by a signal as no reason to start again, does not. The known gap, for the wrapper only: if this
        # fails because the applet comes back, COSMIC changed, and the gap in KNOWN-GAPS is no longer one.
        applet = machine.succeed("pgrep -u alice -fx torq-applet").strip()
        wrapper = machine.succeed(f"ps -o ppid= -p {applet}").strip()
        machine.succeed(f"kill -KILL {wrapper}")
        machine.wait_until_fails(f"kill -0 {applet}", timeout=30)
        machine.sleep(30)
        machine.fail("pgrep -u alice -fx torq-applet")
        # The panel starting again brings it back, as a log-in does (the subtest above).
        panel = machine.succeed("pgrep -u alice -fx cosmic-panel").strip()
        machine.succeed("pkill -u alice -fx cosmic-panel")
        machine.wait_until_succeeds(f"pgrep -u alice -fx cosmic-panel | grep -vx {panel}", timeout=60)
        machine.wait_until_succeeds("pgrep -u alice -fx torq-applet", timeout=60)

    with subtest("memory run short ends a program, it does not freeze the desktop"):
        machine.succeed("swapon --show=NAME --noheadings | grep -q zram")

    with subtest("a system change from the desktop is approved in COSMIC's dialog, and only there"):
        repo = "/var/lib/torq/config"
        machine.succeed(f"mkdir -p {repo} && cp /etc/torq-test/desktop.nix {repo}/ && touch {repo}/flake.nix")
        machine.succeed("torq-broker init")
        sock = "${pkgs.socat}/bin/socat -t 150 - UNIX-CONNECT:/run/torq/broker.sock"
        def on_the_desktop(request):
            # A program of alice's desktop session, as an application is: in her user manager.
            line = json.dumps(request)
            shell = f"printf '%s\\n' {shlex.quote(line)} | {sock}"
            return f"systemd-run --user --machine=alice@ --pipe --wait --quiet /bin/sh -c {shlex.quote(shell)}"
        def frames(raw):
            code, out = None, ""
            for l in raw.splitlines():
                f = json.loads(l)
                out += f.get("out", "")
                if "exit" in f:
                    code = f["exit"]
            return code, json.loads(out) if out.strip() else None
        head = machine.succeed(f"git -C {repo} rev-parse HEAD").strip()
        diff = machine.succeed("cat /etc/torq-test/system.diff")
        code, p = frames(machine.succeed(on_the_desktop({"args": ["--json", "propose", f"--base={head}", "--reason=night light"], "diff": diff})))
        assert code == 0, (code, p)
        seq = str(p["seq"])

        # A password sent straight to the broker, right or not, is refused from here.
        code, v = frames(machine.succeed(on_the_desktop({"args": ["--json", "apply", seq], "password": "torq-desktop"})))
        assert code == 1 and "dialog" in v["error"], (code, v)
        machine.fail(f"git -C {repo} show HEAD:desktop.nix | grep -q 'nightLight = true'")

        # Only people reach the password helper: not a service, and not the agent.
        machine.succeed("test \"$(stat -c %a:%G /run/polkit/agent-helper.socket)\" = 660:users")
        machine.fail("su -s /bin/sh torq-agent-alice -c '${pkgs.socat}/bin/socat -u OPEN:/dev/null UNIX-CONNECT:/run/polkit/agent-helper.socket'")
        # A wrong password in the dialog counts toward the same lockout as one at a terminal.
        ask = on_the_desktop({"args": ["--json", "apply", seq]})
        def ask_in_the_dialog(out):
            # Asked until COSMIC's dialog has registered with polkit: on a slow, software-rendered desktop (.52)
            # it was still starting 25 s after login, and the broker said so ("no authentication agent is
            # running"), as it would to a person; torqcli asks again the same way. A dialog that never registers
            # (one left on a polkit that has gone) fails it, after about two minutes.
            # Then until polkit's password helper has started for this request, one that was not there before
            # it: cosmic-osd starts it when it shows the dialog, and on a loaded machine (.52) that was six
            # seconds after pkcheck began to wait, so the keys typed after a fixed sleep went nowhere. The
            # helper is a oneshot unit, "activating" for as long as the dialog is open. The sleep that follows
            # each call is only for the dialog to draw its field and take the keyboard.
            helper = "systemctl list-units --plain --no-legend --state=activating 'polkit-agent-helper@*' | awk '{print $1}'"
            machine.succeed(f"{helper} > /tmp/helpers.before")
            for attempt in range(8):
                machine.succeed(f"(setsid {ask} > {out} 2>&1 < /dev/null &)")
                # A dialog is a pkcheck still waiting a second later: without one, pkcheck ends at once.
                asked, _ = machine.execute(f"timeout 30 sh -c 'until pgrep -x pkcheck >/dev/null && sleep 1 && pgrep -x pkcheck >/dev/null; do grep -q \"no authentication agent\" {out} && exit 1; sleep 0.2; done'")
                if asked == 0:
                    machine.wait_until_succeeds(f"{helper} | grep -vxFf /tmp/helpers.before | grep -q .", timeout=60)
                    return
                print(machine.execute(f"cat {out}; pgrep -u alice -fa cosmic-osd")[1])
                assert "no authentication agent" in machine.succeed(f"cat {out}"), "no dialog, and not for want of one"
                machine.sleep(10)
            raise AssertionError("COSMIC's dialog never registered with polkit")
        ask_in_the_dialog("/tmp/refused.out")
        machine.sleep(8)
        machine.screenshot("approval")
        machine.send_chars("wrong-password\n", delay=0.1)
        machine.wait_until_succeeds("test $(faillock --dir /run/torq/faillock --user alice | wc -l) -gt 2", timeout=30)
        machine.fail(f"git -C {repo} show HEAD:desktop.nix | grep -q 'nightLight = true'")
        machine.succeed("pkill -x pkcheck || true")
        machine.sleep(3)

        # polkit replaced, as a switch that changes it does: the applet has COSMIC's agent register again. Even an
        # applet that itself starts after the replacement (the panel restarting it, a crash): it reads the processes'
        # start times, not a pid it remembered (the full check on .52, 2026-11-10, where it did not).
        # A fresh applet by restarting the panel: COSMIC 1.11's panel does not start again an applet that exits,
        # only its applets when it starts itself (the laptop's run of this test).
        applet = machine.succeed("pgrep -u alice -fx torq-applet").strip()
        osd = machine.succeed("pgrep -u alice -fx cosmic-osd").strip()
        machine.succeed("systemctl restart polkit")
        machine.succeed("pkill -u alice -fx cosmic-panel")
        machine.wait_until_succeeds(f"pgrep -u alice -fx torq-applet | grep -vx {applet}", timeout=120)
        machine.wait_until_succeeds(f"pgrep -u alice -fx cosmic-osd | grep -vx {osd}", timeout=120)
        machine.sleep(3)
        # A process another account names polkitd is not polkit: newer than the dialog, it must not have the
        # applet end it (the agent could otherwise end the person's dialog every few seconds).
        osd = machine.succeed("pgrep -u alice -fx cosmic-osd").strip()
        machine.succeed("cp ${pkgs.bash}/bin/bash /tmp/polkitd && chmod 755 /tmp/polkitd")
        machine.succeed("systemd-run --uid=nobody --unit=fake-polkitd /tmp/polkitd -c '${pkgs.coreutils}/bin/sleep 120; true'")
        machine.wait_until_succeeds("pgrep -u nobody -x polkitd", timeout=10)
        machine.sleep(12)
        machine.succeed(f"pgrep -u alice -fx cosmic-osd | grep -qx {osd}")
        machine.succeed("systemctl stop fake-polkitd")

        # Meanwhile the applet saved alice's desktop (it saves when the ledger changes, and every 15
        # minutes): the configuration has moved on from the change's base, by a commit to her own folder.
        # The regression of ADR-053: it was refused as stale, so no change waiting for a person could land.
        machine.wait_until_succeeds(f"git -C {repo} cat-file -e HEAD:users/alice/desktop.json", timeout=150)
        moved = machine.succeed(f"git -C {repo} rev-parse HEAD").strip()
        assert moved != head, moved

        # The right one, in the dialog: the change is made.
        ask_in_the_dialog("/tmp/approved.out")
        machine.sleep(8)
        machine.send_chars("torq-desktop\n", delay=0.1)
        try:
            machine.wait_until_succeeds("grep -q '\"exit\":0' /tmp/approved.out", timeout=60)
        except Exception:
            # What the broker answered, and what it and polkit logged: a failure here says why.
            print(machine.execute("cat /tmp/approved.out; journalctl -b --no-pager -u 'torq-broker@*' -u polkit -u 'polkit-agent-helper@*' | tail -40")[1])
            raise
        machine.succeed(f"git -C {repo} show HEAD:desktop.nix | grep -q 'nightLight = true'")
        # Re-checked on the moved configuration, and the history says so; the desktop save kept.
        applied = [json.loads(l) for l in machine.succeed("torq-broker --json log --brief").splitlines()]
        applied = [e for e in applied if e["kind"] == "applied" and e["data"].get("proposal") == int(seq)]
        assert len(applied) == 1 and applied[0]["data"].get("rechecked_on"), applied
        machine.succeed(f"git -C {repo} cat-file -e HEAD:users/alice/desktop.json")

    with subtest("a program that has closed its memory still gets the dialog: a system secret kept the way TorqOS Settings keeps one"):
        # TorqOS Settings is not dumpable once its Secrets page is used, and stays so until it is closed: every
        # approval it asks after that is asked of polkit about a process whose files in /proc are root's. The page's
        # own call (torq::app::secret_set, tests/secret-set-example.nix), from a program of alice's session, which
        # is not dumpable by the time the broker asks. The system's secret, so an administrator's approval.
        keep = "printf %s dialog-token-1 | torq-secret-set-example dlg"
        # (`ask` is what ask_in_the_dialog starts, above.)
        ask = f"systemd-run --user --machine=alice@ --pipe --wait --quiet /bin/sh -c {shlex.quote(keep)}"
        ask_in_the_dialog("/tmp/secret.out")
        pid = machine.succeed("pgrep -u alice -x torq-secret-set").strip()
        # Not dumpable: its files are root's and alice cannot read its environment. Its folder stays alice's, which
        # is what the broker reads to know the caller is still there.
        assert machine.succeed(f"stat -c %U /proc/{pid} /proc/{pid}/stat /proc/{pid}/environ").split() == ["alice", "root", "root"]
        machine.fail(f"su -s /bin/sh alice -c 'cat /proc/{pid}/environ'")
        machine.sleep(8)
        machine.screenshot("approval-secret")
        machine.send_chars("torq-desktop\n", delay=0.1)
        try:
            machine.wait_until_succeeds("grep -q 'The value of the system.s secret dlg is kept' /tmp/secret.out", timeout=60)
        except Exception:
            print(machine.execute("cat /tmp/secret.out; journalctl -b --no-pager -u 'torq-broker@*' -u polkit -u 'polkit-agent-helper@*' | tail -40")[1])
            raise
        store = "/var/lib/torq/secrets/store/system/dlg"
        assert machine.succeed(f"systemd-creds decrypt --name=torq-secret.system.dlg {store} -") == "dialog-token-1"
        assert "dialog-token-1" not in machine.succeed("torq-broker log") + machine.succeed("cat /tmp/secret.out")

    with subtest("an agent's use of a secret that waits is seen by the panel's applet, which tells alice; her yes, as its dialog sends it, is on the record as the dialog's"):
        # Driven here: the applet, in alice's session, asks the broker what waits for her, finds the use and raises
        # its notification. Not driven: the dialog's drawing, and the press of its button. Nothing in these tests
        # clicks in the panel, so the yes is the request the dialog's Allow once sends (secret-approve <n> --where
        # dialog), sent from her session by the test. That an approved use reaches its host is tests/secrets.nix's:
        # this machine has no such host to send to.
        import time
        machine.wait_for_unit("torq-secrets.service")
        machine.wait_for_open_port(7800)
        def as_account(user, request):
            line = json.dumps(request)
            shell = f"printf '%s\\n' {shlex.quote(line)} | {sock}"
            return frames(machine.succeed(f"su -s /bin/sh {user} -c {shlex.quote(shell)}"))
        def in_her_session(request):
            return frames(machine.succeed(on_the_desktop(request)))
        # A token of alice's own, which her agent may use for one host, a person asked each time.
        code, v = in_her_session({"args": ["--json", "secret-set", "--name", "gh", "--user", "alice"], "key": "desk-token-1"})
        assert code == 0, (code, v)
        rules = '{"gh": {"kind": "token", "https": {"hosts": ["api.example.test"]}, "actors": ["torq-agent"], "approve": "each-use"}}'
        diff = f"--- /dev/null\n+++ b/users/alice/secrets.json\n@@ -0,0 +1 @@\n+{rules}\n"
        head = machine.succeed(f"git -C {repo} rev-parse HEAD").strip()
        code, p = in_her_session({"args": ["--json", "propose", f"--base={head}", "--reason=gh for the agent"], "diff": diff})
        assert code == 0, (code, p)
        code, v = in_her_session({"args": ["--json", "apply", str(p["seq"])]})
        assert code == 0, (code, v)
        # Her agent's account (a coding tool's, so its session is one the broker cannot see into) uses it: it waits.
        code, s = as_account("torq-agent-alice", {"args": ["--json", "session-start"]})
        assert code == 0, (code, s)
        use = f"curl -s -o /tmp/use.body -w '%{{http_code}}' -H 'X-Torq-Session: {s['session']}' -H 'X-Torq-Reason: listing the repositories' http://127.0.0.1:7800/user/alice/gh/api.example.test/echo > /tmp/use.status"
        machine.succeed(f"rm -f /tmp/use.status; su -s /bin/sh torq-agent-alice -c {shlex.quote(use)} >&2 &")
        waiting = []
        for _ in range(30):
            code, w = in_her_session({"args": ["--json", "secret-waiting"]})
            assert code == 0, (code, w)
            waiting = w["waiting"]
            if waiting:
                break
            time.sleep(1)
        assert len(waiting) == 1 and waiting[0]["facts"]["secret"] == "gh" and waiting[0]["facts"]["person"] == "alice" and waiting[0]["facts"]["tainted"], waiting
        n = waiting[0]["n"]
        # The applet has seen that very use (it asks every 10 seconds), and its notification went out without an
        # error. What the applet writes to its standard error is in the journal: cosmic-panel, which starts it,
        # writes each line there under the applet's name ("cosmic-panel[…]: org.torqos.Applet: …"), as seen on
        # the laptop's own journal for another applet's lines.
        # (grep -c reads the whole journal: a grep that left early would break the pipe.)
        machine.wait_until_succeeds(f"journalctl -b --no-pager | grep -c 'torq-applet: use {n} of a secret waits for an answer' > /dev/null", timeout=60)
        machine.sleep(5)
        machine.screenshot("use-waiting")
        machine.fail("journalctl -b --no-pager | grep -c 'the notification that a use of a secret waits:' > /dev/null")
        # Her agent's own account cannot say yes to it. Alice's yes, as the dialog sends it, is recorded as the dialog's.
        code, v = as_account("torq-agent-alice", {"args": ["--json", "secret-approve", str(n), "--where", "dialog"]})
        assert code == 1, (code, v)
        code, v = in_her_session({"args": ["--json", "secret-approve", str(n), "--where", "dialog"]})
        assert code == 0 and v["kind"] == "secret_use_approved" and v["approved_by"] == "alice" and v["data"]["where"] == "dialog" and v["data"]["ask"] == n, (code, v)
        # The use is let go, and the daemon tries to send it: there is no such host, which is the daemon's 502 (a
        # send that failed), where the broker's refusal would have been its 403.
        machine.wait_until_succeeds("test -s /tmp/use.status", timeout=60)
        status, body = machine.succeed("cat /tmp/use.status").strip(), machine.succeed("cat /tmp/use.body")
        assert status == "502" and "desk-token-1" not in body, (status, body)
        code, w = in_her_session({"args": ["--json", "secret-waiting"]})
        assert code == 0 and w["waiting"] == [], (code, w)
        ledger = machine.succeed("torq-broker log")
        assert "desk-token-1" not in ledger and "listing the repositories" not in ledger, "neither the value nor the agent's words are on the record"

    with subtest("Flathub is the person's, not the system's"):
        machine.wait_until_succeeds("su - alice -c 'flatpak remotes --user' | grep -q flathub", timeout=60)
        machine.fail("flatpak remotes --system | grep -q flathub")

    # Last: it locks the session.
    with subtest("a laptop that cannot wake from suspend never sleeps: the lid dims and locks, and gives it back"):
        machine.succeed("grep -qx 'HandleLidSwitch=ignore' /etc/systemd/logind.conf")
        machine.succeed("grep -qx 'AllowSuspend=no' /etc/systemd/sleep.conf")
        machine.succeed("mkdir -p /tmp/bl/panel && echo 260000 > /tmp/bl/panel/brightness")
        close = "TORQ_BACKLIGHT=/tmp/bl /etc/torq/lid-action 'button/lid LID close'"
        machine.succeed(close)
        machine.succeed("test \"$(cat /tmp/bl/panel/brightness)\" = 0")
        machine.succeed(close)  # closed twice: the brightness kept is the first one
        machine.succeed("TORQ_BACKLIGHT=/tmp/bl /etc/torq/lid-action 'button/lid LID open'")
        machine.succeed("test \"$(cat /tmp/bl/panel/brightness)\" = 260000")
  '';
}

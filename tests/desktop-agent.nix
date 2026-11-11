# NixOS VM test: the person's agent on the desktop (ADR-051 4, steps 3 and 4). After alice logs in to COSMIC, the
# agent's applet, `torq-agent-applet`, runs in her panel without anyone adding it — org.torqos.Agent is in
# the panel's default right after TorqOS's own applet; the agent's window, "TorqOS Agent", is in the menu;
# the applet's looking never starts her agent's service (only asking does); the client every UI shares
# reaches her own service from her session (`torq-agent-ui --check`) and says what it remembers; someone
# with no agent is told so; and the window is one a person — started again, the one open stays the only one.
# Step 4's entry points: `ask` in the login shell, the launcher's plugin where pop-launcher looks, a crash offered
# with nothing sent until the person asks, Super+Shift+A opening the window (Super+A still the App Library's). The model is scripted (as in
# tests/agent-service.nix).
{ pkgs, self }:

let
  # The owner's password, `torq-desktop`, as the broker keeps it (modules/accounts.nix).
  hash = "$6$torqtestsalt$wXsKSX3z/kTvGvAzmdrks4iI95ZmWWtQgfYUMV/OEbQ7h7HZxSpMJYh0hvWPma/izeabeQYfkEhhQhWA52xbE1";
  fakeModel = pkgs.writeScript "fake-model" ''
    #!${pkgs.python3}/bin/python3
    import json
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    class H(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass
        def do_POST(self):
            self.rfile.read(int(self.headers["Content-Length"]))
            out = json.dumps({"choices": [{"message": {"role": "assistant", "content": "Hello alice."}}]}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(out)))
            self.end_headers()
            self.wfile.write(out)
    # On a unix socket in a directory only root writes (its RuntimeDirectory): a model no account here can stand
    # in for, so the resident agent's sessions start clean (ADR-051 step 5; a TCP one on this machine would not).
    import os, socketserver
    class S(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
        daemon_threads = True
    SOCK = "/run/fake-model/model.sock"
    if os.path.exists(SOCK):
        os.unlink(SOCK)
    server = S(SOCK, H)
    os.chmod(SOCK, 0o666)
    server.serve_forever()
  '';
in
pkgs.testers.runNixOSTest {
  name = "torq-desktop-agent";
  # The desktop sets nixpkgs.overlays (COSMIC from its own nixpkgs), so the nodes build their own package set.
  node.pkgsReadOnly = false;

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "alice";
    torq.desktop.enable = true;
    torq.desktop.dimInsteadOfSuspend = true;
    nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    virtualisation.memorySize = 4096;
    torq.broker.rebuildCommand = "true";
    # alice's agent, resident, asking a scripted model on this machine.
    torq.agent.people = [ "alice" ];
    torq.agent.doctor = { enable = true; endpoint = "unix:/run/fake-model/model.sock"; model = "fake"; maxSteps = 2; };
    systemd.services.fake-model = { wantedBy = [ "multi-user.target" ]; serviceConfig = { ExecStart = fakeModel; RuntimeDirectory = "fake-model"; }; };
  };

  testScript = ''
    import shlex
    machine.wait_for_unit("multi-user.target")
    machine.succeed("install -d -m 700 /var/lib/torq/secrets/passwords"
                    " && echo '${hash}' > /var/lib/torq/secrets/passwords/alice"
                    " && /run/current-system/activate")
    repo = "/var/lib/torq/config"
    machine.succeed(f"mkdir -p {repo} && echo '{{}}' > {repo}/desktop.json && touch {repo}/flake.nix && torq-broker init")
    unit = "torq-agent@alice.service"

    def in_session(cmd, ok=True):
        # A program of alice's desktop session, as an application is: in her user manager.
        code, out = machine.execute(f"systemd-run --user --machine=alice@ --pipe --wait --quiet /bin/sh -c {shlex.quote(cmd)}")
        assert (code == 0) == ok, f"{cmd}: exit {code}:\n{out}"
        return out

    with subtest("the owner logs in to COSMIC"):
        machine.wait_for_unit("graphical.target", timeout=180)
        machine.wait_until_succeeds("pgrep -u cosmic-greeter -f cosmic-greeter", timeout=60)
        # greetd draws the password field some seconds after the greeter starts: typed again if it was early.
        for attempt in range(4):
            machine.sleep(15)
            machine.send_chars("torq-desktop\n", delay=0.2)
            if machine.execute("timeout 60 sh -c 'until pgrep -u alice -fx cosmic-panel; do sleep 2; done'")[0] == 0:
                break
        machine.wait_until_succeeds("pgrep -u alice -fx cosmic-panel", timeout=30)

    with subtest("the agent's applet is in the default panel, right after TorqOS's, running without anyone adding it"):
        wings = machine.succeed("cat /run/current-system/sw/share/cosmic/com.system76.CosmicPanel.Panel/v1/plugins_wings")
        assert '"org.torqos.Applet", "org.torqos.Agent", "com.system76.CosmicAppletInputSources"' in wings, wings
        machine.succeed("grep -qx X-CosmicApplet=true /run/current-system/sw/share/applications/org.torqos.Agent.desktop")
        machine.wait_until_succeeds("pgrep -u alice -fx torq-applet", timeout=60)
        machine.wait_until_succeeds("pgrep -u alice -fx torq-agent-applet", timeout=60)
        machine.sleep(10)
        machine.screenshot("panel")

    with subtest("the agent's window is in the menu"):
        entry = machine.succeed("cat /run/current-system/sw/share/applications/org.torqos.AgentWindow.desktop")
        assert "Name=TorqOS Agent" in entry and "Exec=torq-agent-ui" in entry and "NoDisplay" not in entry, entry
        machine.succeed("test -x /run/current-system/sw/bin/torq-agent-ui")

    with subtest("the applet's looking does not start the agent: only asking does"):
        # It has looked (every 10 s) for well over a minute of the session by now.
        machine.sleep(25)
        assert machine.succeed(f"systemctl show -P ActiveState {unit}").strip() == "inactive"

    with subtest("from her session, the client the UIs share reaches her own agent"):
        out = in_session("torq-agent-ui --check")
        assert out.splitlines()[0] == "Idle · remembers nothing yet", out
        assert "Local model (llama-server on this machine)" in out, out
        assert machine.succeed(f"ps -o user= -p $(systemctl show -P MainPID {unit})").strip() == "torq-resident-alice"
        assert in_session("torq ask hello").strip() == "Hello alice."
        assert in_session("torq-agent-ui --check").splitlines()[0] == "Idle · remembers 1 turn"
        # The Agent page's Forget is a torqcli line: through her own socket, and the count says so.
        assert "forgotten the conversation" in in_session("torqcli request system agent forget")
        assert in_session("torq-agent-ui --check").splitlines()[0] == "Idle · remembers nothing yet"

    with subtest("someone with no agent is told so, and reaches no one's"):
        # (Not a second person: COSMIC's greeter would no longer open on alice.)
        code, out = machine.execute("su -s /bin/sh nobody -c 'torq-agent-ui --check' 2>&1")
        assert code != 0 and "The agent is not set up for you" in out, out

    with subtest("step 4: the terminal's ask, in the login shell"):
        out = machine.succeed("su - alice -c \"zsh -i -c 'alias ask'\" 2>/dev/null")
        assert "noglob torq ask" in out, out
        # Alone, it reads one line as the question: nothing in it is the shell's.
        out = machine.succeed("echo 'why is $HOME * here?' | su - alice -c 'torq ask'")
        assert out.strip() == "Hello alice.", out

    with subtest("step 4: the launcher's plugin is where pop-launcher looks, and offers the question"):
        ron = machine.succeed("cat /etc/pop-launcher/plugins/torq-agent/plugin.ron")
        assert 'bin: (path: "/run/current-system/sw/bin/torq-agent-launcher")' in ron and 'regex: "^(ask )"' in ron, ron
        machine.succeed("test -x /run/current-system/sw/bin/torq-agent-launcher")
        said = machine.succeed("printf '%s\\n' '{\"Search\":\"ask why is it slow?\"}' '\"Exit\"' | su - alice -c torq-agent-launcher")
        assert "Ask your agent: why is it slow?" in said, said

    with subtest("step 4: a crash is offered, and nothing reaches the agent until the person asks"):
        machine.wait_until_succeeds("systemctl --user --machine=alice@ is-active torq-crash-watch.service", timeout=60)
        before = in_session("torq-agent-ui --check").splitlines()[0]
        # A crash anyone can write to the journal (its COREDUMP_ fields are the sender's words): never offered.
        # (echo, not printf: systemd would read a % in a unit's command line as its own.)
        fields = ["MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1", "COREDUMP_UID=1000", "COREDUMP_PID=1", "COREDUMP_EXE=/forged/exe", "COREDUMP_SIGNAL_NAME=SIGSEGV", "MESSAGE=forged"]
        forge = "{ " + "; ".join(f"echo {f}" for f in fields) + "; } | logger --journald"
        in_session(forge)
        machine.sleep(5)
        machine.succeed("journalctl --no-pager COREDUMP_EXE=/forged/exe | grep -q forged")
        # ($$$$: systemd makes $$ of it, the shell's own pid. Its exit status is not what is checked: the watcher's word is.)
        machine.execute("systemd-run --user --machine=alice@ --pipe --wait --quiet /bin/sh -c 'ulimit -c unlimited; kill -SEGV $$$$'")
        machine.wait_until_succeeds("journalctl --no-pager _SYSTEMD_USER_UNIT=torq-crash-watch.service | grep -q 'torq-crash-watch: offered: '", timeout=90)
        machine.sleep(5)
        watched = machine.succeed("journalctl --no-pager _SYSTEMD_USER_UNIT=torq-crash-watch.service")
        assert "asked about pid" not in watched, watched
        assert "/forged/exe" not in watched, watched
        # Nothing handed to the window, and the agent asked nothing.
        machine.fail("ls /run/user/$(id -u alice)/torq-agent/ask-* 2>/dev/null")
        assert in_session("torq-agent-ui --check").splitlines()[0] == before

    with subtest("step 4: Super+Shift+A opens the agent's window, a default of COSMIC's shortcuts; Super+A stays the App Library's"):
        custom = machine.succeed("cat /run/current-system/sw/share/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom")
        assert '(modifiers: [Super, Shift], key: "a"): Spawn("/run/current-system/sw/bin/torq-agent-ui")' in custom, custom
        assert '[Super], key: "a"' not in custom, custom
        defaults = machine.succeed("cat /run/current-system/sw/share/cosmic/com.system76.CosmicSettings.Shortcuts/v1/defaults")
        assert '(modifiers: [Super], key: "a"): System(AppLibrary)' in defaults
        assert '[Super, Shift], key: "a"' not in defaults, "Super+Shift+A must be free in COSMIC's own defaults"
        machine.send_key("meta_l-shift-a")
        machine.wait_until_succeeds("pgrep -u alice -f 'bin/torq-agent-ui$'", timeout=60)
        machine.sleep(5)
        machine.screenshot("agent-key")
        machine.succeed("pkill -u alice -f 'bin/torq-agent-ui$'")
        machine.wait_until_fails("pgrep -u alice -f 'bin/torq-agent-ui$'", timeout=30)

    with subtest("the agent's window: one a person"):
        display = machine.succeed("ls /run/user/$(id -u alice) | grep -x 'wayland-[0-9]*' | head -1").strip()
        def window(name):
            machine.succeed(f"systemd-run --user --machine=alice@ --quiet --unit={name} --setenv=WAYLAND_DISPLAY={display} /run/current-system/sw/bin/torq-agent-ui")
        def state(name):
            return machine.succeed(f"systemctl --user --machine=alice@ show -P ActiveState {name}.service").strip()
        def log(name):
            return machine.execute(f"journalctl --no-pager _SYSTEMD_USER_UNIT={name}.service | tail -20")[1]
        window("agent-window")
        machine.sleep(10)
        assert state("agent-window") == "active", log("agent-window")
        machine.screenshot("window")
        # Started again (as the applet's Open does): it hands over to the one open and ends.
        window("agent-window-again")
        machine.wait_until_succeeds("test \"$(systemctl --user --machine=alice@ show -P ActiveState agent-window-again.service)\" != active", timeout=30)
        # It handed over (the first was activated), and ended well: not merely ended.
        machine.wait_until_succeeds("journalctl --no-pager _SYSTEMD_USER_UNIT=agent-window.service | grep -q 'started again: this window raised'", timeout=30)
        # (A transient unit that ended well is gone, inactive; one that failed stays, failed.)
        assert state("agent-window-again") == "inactive", log("agent-window-again")
        assert state("agent-window") == "active", log("agent-window")
        print(log("agent-window-again"))
  '';
}

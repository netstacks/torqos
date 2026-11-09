# NixOS VM test: TorqOS Desktop (ADR-045) boots to COSMIC's greeter, and the owner's password logs in to
# a COSMIC session. On it: NetworkManager with iwd as its Wi-Fi backend (one Wi-Fi stack, ADR-046 2), no
# wpa_supplicant; Flatpak with Flathub for the person only, never the system's (ADR-037, amended).
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
    environment.systemPackages = [ pkgs.pam ];
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
        machine.send_chars("torq-desktop\n", delay=0.2)
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
        # In the panel by default, running there: a new machine has it without anyone adding it.
        machine.wait_until_succeeds("pgrep -u alice -fx torq-applet", timeout=60)

    with subtest("COSMIC's own first-run wizard is not offered: TorqOS has its setup"):
        machine.fail("test -e /run/current-system/sw/bin/cosmic-initial-setup")

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

        # A wrong password in the dialog counts toward the same lockout as one at a terminal.
        ask = on_the_desktop({"args": ["--json", "apply", seq]})
        machine.succeed(f"(setsid {ask} > /tmp/refused.out 2>&1 < /dev/null &)")
        machine.wait_until_succeeds("pgrep -x pkcheck", timeout=30)
        machine.sleep(8)
        machine.screenshot("approval")
        machine.send_chars("wrong-password\n", delay=0.1)
        machine.wait_until_succeeds("faillock --dir /run/torq/faillock --user alice | grep -q ' V$\\| V '", timeout=30)
        machine.fail(f"git -C {repo} show HEAD:desktop.nix | grep -q 'nightLight = true'")
        machine.succeed("pkill -x pkcheck || true")
        machine.sleep(3)

        # The right one, in the dialog: the change is made.
        machine.succeed(f"(setsid {ask} > /tmp/approved.out 2>&1 < /dev/null &)")
        machine.wait_until_succeeds("pgrep -x pkcheck", timeout=30)
        machine.sleep(8)
        machine.send_chars("torq-desktop\n", delay=0.1)
        machine.wait_until_succeeds("grep -q '\"exit\":0' /tmp/approved.out", timeout=60)
        machine.succeed(f"git -C {repo} show HEAD:desktop.nix | grep -q 'nightLight = true'")

    with subtest("Flathub is the person's, not the system's"):
        machine.wait_until_succeeds("su - alice -c 'flatpak remotes --user' | grep -q flathub", timeout=60)
        machine.fail("flatpak remotes --system | grep -q flathub")
  '';
}

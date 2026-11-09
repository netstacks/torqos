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
  };

  testScript = ''
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

    with subtest("Flathub is the person's, not the system's"):
        machine.wait_until_succeeds("su - alice -c 'flatpak remotes --user' | grep -q flathub", timeout=60)
        machine.fail("flatpak remotes --system | grep -q flathub")
  '';
}

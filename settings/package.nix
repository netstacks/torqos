# TorqOS Settings (ADR-045 2), built as COSMIC's own applications are (libcosmicAppHook), from the nixpkgs
# the desktop's COSMIC comes from (flake.nix, `nixpkgs-cosmic`).
{ lib, rustPlatform, libcosmicAppHook, pkg-config, makeDesktopItem, copyDesktopItems, cosmic-panel, openssh }:

rustPlatform.buildRustPackage {
  pname = "torq-settings";
  # One place: Cargo.toml (scripts/bump-version sets it).
  version = (lib.importTOML ./Cargo.toml).package.version;
  # This crate and what it shares (torqcli's library, torq-wire); not a build left in the working tree.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.difference (lib.fileset.unions [ ./. ../cli ../wire ])
      (lib.fileset.unions (map lib.fileset.maybeMissing [ ./target ../cli/target ../wire/target ]));
  };
  cargoRoot = "settings";
  buildAndTestSubdir = "settings";
  # libcosmic and its own git dependencies, each at the revision Cargo.lock names, vendored as nixpkgs does
  # COSMIC's own applications (two git sources give crates of one name and version, which cargoLock refuses).
  # Changing Cargo.lock changes this hash: build once, and take the one Nix reports.
  cargoHash = "sha256-mqLnx7S6+LYbu9cf5G0Pju1ml12g9jO7SGR0fxxZ6w8=";

  nativeBuildInputs = [ pkg-config libcosmicAppHook copyDesktopItems ];

  # Its own code has no tests; torqcli's library, whose words it shows, is tested in torqcli's package.
  doCheck = false;

  # TorqOS in COSMIC's panel by default (ADR-049 2). The panel reads its whole default configuration or none,
  # so all of it, COSMIC's own at this version, with our applets first among the status ones. A default: a
  # person's own panel settings come before it. The build stops if COSMIC's list no longer reads as expected.
  postInstall = ''
    install -Dm644 ${./org.torqos.policy} $out/share/polkit-1/actions/org.torqos.policy
    mkdir -p $out/share
    cp -r --no-preserve=mode ${cosmic-panel.src}/data/default_schema $out/share/cosmic
    wings=$out/share/cosmic/com.system76.CosmicPanel.Panel/v1/plugins_wings
    grep -q '"com.system76.CosmicAppletInputSources"' $wings
    sed -i 's/"com.system76.CosmicAppletInputSources"/"org.torqos.Applet", &/' $wings
    grep -q org.torqos.Applet $wings
    # The person's agent beside it (ADR-051 4, settled by the owner): right after TorqOS's own, or no build.
    grep -q '"org.torqos.Applet", "com.system76.CosmicAppletInputSources"' $wings
    sed -i 's/"org.torqos.Applet", /&"org.torqos.Agent", /' $wings
    grep -q '"org.torqos.Applet", "org.torqos.Agent", "com.system76.CosmicAppletInputSources"' $wings

    # Super+A opens the person's agent (ADR-051 4, step 4): a default of COSMIC's own custom shortcuts, read where
    # the panel's default is; the person may change it in COSMIC Settings, which then writes their own. It
    # replaces COSMIC's Super+A (the App Library, still on the dock and in the launcher).
    install -Dm644 /dev/stdin $out/share/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom <<SHORTCUTS
    {
        (modifiers: [Super], key: "a"): Spawn("/run/current-system/sw/bin/torq-agent-ui"),
    }
    SHORTCUTS

    # A person's SSH tunnel (ADR-050 2), kept up by their own user manager while they are logged in. Started
    # from the tunnels applet; "Start at login" is `systemctl --user enable`, in the person's own home. A wrong
    # file (78) is not tried again until the person starts it; anything else is, waiting longer each time.
    install -Dm644 /dev/stdin "$out/lib/systemd/user/torq-tunnel@.service" <<UNIT
    [Unit]
    Description=SSH tunnel %i (TorqOS, ADR-050)
    StartLimitIntervalSec=0

    [Service]
    Environment=PATH=${openssh}/bin
    ExecStart=$out/bin/torq-tunnel run %i
    Restart=always
    RestartSec=5s
    RestartSteps=5
    RestartMaxDelaySec=60s
    RestartPreventExitStatus=78

    [Install]
    WantedBy=default.target
    UNIT
  '';

  desktopItems = [
    # TorqOS in COSMIC's panel (ADR-049): an applet, not an application of the menu.
    (makeDesktopItem {
      name = "org.torqos.Applet";
      desktopName = "TorqOS";
      comment = "This machine's state: a change to confirm, a restart or log-out it needs, alarms, proposals";
      exec = "torq-applet";
      icon = "torqos";
      noDisplay = true;
      extraConfig = { X-CosmicApplet = "true"; X-CosmicHoverPopup = "Auto"; X-OverflowPriority = "10"; };
    })
    # The person's agent (ADR-051 4): in the default panel, beside TorqOS's own.
    (makeDesktopItem {
      name = "org.torqos.Agent";
      desktopName = "TorqOS Agent";
      comment = "Your agent: ask it, see what it is doing, stop it, review what it proposed";
      exec = "torq-agent-applet";
      icon = "torqos";
      noDisplay = true;
      extraConfig = { X-CosmicApplet = "true"; X-CosmicHoverPopup = "Auto"; X-OverflowPriority = "10"; };
    })
    # Its window, in the menu: one a person (started again, it raises the one open).
    (makeDesktopItem {
      name = "org.torqos.AgentWindow";
      desktopName = "TorqOS Agent";
      comment = "Ask your agent about this machine: it explains, and proposes changes you approve";
      exec = "torq-agent-ui";
      icon = "torqos";
      categories = [ "System" "Utility" ];
      keywords = [ "torq" "agent" "assistant" "ai" "ask" ];
      startupWMClass = "org.torqos.AgentWindow";
    })
    # The person's SSH tunnels (ADR-050 4): in COSMIC's applet list, not in the default panel.
    (makeDesktopItem {
      name = "org.torqos.Tunnels";
      desktopName = "SSH tunnels";
      comment = "Your SSH tunnels: start, stop and add them, and see whether each is up";
      exec = "torq-tunnels";
      icon = "network-vpn-symbolic";
      noDisplay = true;
      extraConfig = { X-CosmicApplet = "true"; X-CosmicHoverPopup = "Auto"; X-OverflowPriority = "10"; };
    })
    (makeDesktopItem {
      name = "org.torqos.Settings";
      desktopName = "TorqOS Settings";
      comment = "This machine's configuration, its history, and what it waits for you to approve";
      exec = "torq-settings";
      icon = "torqos";
      categories = [ "Settings" "System" ];
      keywords = [ "torq" "configuration" "history" "ledger" "rollback" ];
      startupWMClass = "org.torqos.Settings";
    })
  ];

  meta = {
    description = "TorqOS Settings: torqcli in a COSMIC window, and TorqOS in its panel";
    mainProgram = "torq-settings";
    license = lib.licenses.asl20;
    platforms = lib.platforms.linux;
  };
}

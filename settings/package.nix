# TorqOS Settings (ADR-045 2), built as COSMIC's own applications are (libcosmicAppHook), from the nixpkgs
# the desktop's COSMIC comes from (flake.nix, `nixpkgs-cosmic`).
{ lib, rustPlatform, libcosmicAppHook, pkg-config, makeDesktopItem, copyDesktopItems, cosmic-panel }:

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
  cargoHash = "sha256-lPihLAR3p5IvwWjMCaxdJ6IRAh5c54Vfn1cNSlAXdgE=";

  nativeBuildInputs = [ pkg-config libcosmicAppHook copyDesktopItems ];

  # Its own code has no tests; torqcli's library, whose words it shows, is tested in torqcli's package.
  doCheck = false;

  # TorqOS in COSMIC's panel by default (ADR-049 2). The panel reads its whole default configuration or none,
  # so all of it, COSMIC's own at this version, with our applet first among the status ones. A default: a
  # person's own panel settings come before it. The build stops if COSMIC's list no longer reads as expected.
  postInstall = ''
    mkdir -p $out/share
    cp -r --no-preserve=mode ${cosmic-panel.src}/data/default_schema $out/share/cosmic
    wings=$out/share/cosmic/com.system76.CosmicPanel.Panel/v1/plugins_wings
    grep -q '"com.system76.CosmicAppletInputSources"' $wings
    sed -i 's/"com.system76.CosmicAppletInputSources"/"org.torqos.Applet", &/' $wings
    grep -q org.torqos.Applet $wings
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

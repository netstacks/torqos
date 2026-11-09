# TorqOS Settings (ADR-045 2), built as COSMIC's own applications are (libcosmicAppHook), from the nixpkgs
# the desktop's COSMIC comes from (flake.nix, `nixpkgs-cosmic`).
{ lib, rustPlatform, libcosmicAppHook, pkg-config, makeDesktopItem, copyDesktopItems }:

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
  # libcosmic and its own git dependencies, each at the revision the lock names.
  cargoLock = {
    lockFile = ./Cargo.lock;
    allowBuiltinFetchGit = true;
  };

  nativeBuildInputs = [ pkg-config libcosmicAppHook copyDesktopItems ];

  # Its own code has no tests; torqcli's library, whose words it shows, is tested in torqcli's package.
  doCheck = false;

  desktopItems = [
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
    description = "TorqOS Settings: torqcli in a COSMIC window, a client of the broker's socket";
    mainProgram = "torq-settings";
    license = lib.licenses.asl20;
    platforms = lib.platforms.linux;
  };
}

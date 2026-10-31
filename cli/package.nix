{ lib, stdenv, rustPlatform, symlinkJoin, git, diffutils, coreutils, gnugrep, bubblewrap }:

let
  # The filters after a torqcli `show` (`| grep`, `| wc` …, cli/src/filter.rs, ADR-017) run from these
  # tools, never from PATH, each inside bubblewrap. Linux only: elsewhere no filter runs.
  filterEnv = lib.optionalAttrs stdenv.hostPlatform.isLinux {
    TORQ_FILTER_BIN = "${symlinkJoin { name = "torq-filters"; paths = [ coreutils gnugrep ]; }}/bin";
    TORQ_BWRAP = lib.getExe bubblewrap;
  };
in

rustPlatform.buildRustPackage {
  pname = "torq";
  version = "0.1.0";
  src = lib.cleanSource ./.;
  cargoLock.lockFile = ./Cargo.lock;

  env = filterEnv;
  # For the dev shell (flake.nix), so `cargo test` there builds the same way.
  passthru = { inherit filterEnv; };

  # The candidate's tests read a real git repo and build diffs.
  nativeCheckInputs = [ git diffutils ];

  # torqcli, the shell (ADR-016), is this program under its own name.
  postInstall = ''
    ln -s torq $out/bin/torqcli
  '';

  meta = {
    description = "TorqOS command line (unprivileged): changes the system by proposing to torq-broker";
    mainProgram = "torq";
    license = with lib.licenses; [ mit asl20 ];
  };
}

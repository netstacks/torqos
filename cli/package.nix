{ lib, rustPlatform, git, diffutils }:

rustPlatform.buildRustPackage {
  pname = "torq";
  version = "0.1.0";
  src = lib.cleanSource ./.;
  cargoLock.lockFile = ./Cargo.lock;

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

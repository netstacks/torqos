{ lib, rustPlatform }:

rustPlatform.buildRustPackage {
  pname = "torq";
  version = "0.1.0";
  src = lib.cleanSource ./.;
  cargoLock.lockFile = ./Cargo.lock;

  meta = {
    description = "TorqOS command line (unprivileged): changes the system by proposing to torq-broker";
    mainProgram = "torq";
    license = with lib.licenses; [ mit asl20 ];
  };
}

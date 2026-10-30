{ lib, rustPlatform }:

rustPlatform.buildRustPackage {
  pname = "kestrel";
  version = "0.1.0";
  src = lib.cleanSource ./.;
  cargoLock.lockFile = ./Cargo.lock;

  meta = {
    description = "Kestrel command line (unprivileged): changes the system by proposing to kestrel-broker";
    mainProgram = "kestrel";
    license = with lib.licenses; [ mit asl20 ];
  };
}

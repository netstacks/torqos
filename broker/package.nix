{ lib, rustPlatform, git }:

rustPlatform.buildRustPackage {
  pname = "kestrel-broker";
  version = "0.1.0";
  src = lib.cleanSource ./.;

  # Generate once with `cargo generate-lockfile` in broker/ and commit Cargo.lock.
  cargoLock.lockFile = ./Cargo.lock;

  # Integration tests drive a real git repo, single-threaded (see .cargo/config.toml).
  nativeCheckInputs = [ git ];
  dontUseCargoParallelTests = true;

  meta = {
    description = "Kestrel privileged change broker";
    mainProgram = "kestrel-broker";
    license = with lib.licenses; [ mit asl20 ];
  };
}

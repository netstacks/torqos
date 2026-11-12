# TorqOS Settings' way of keeping a secret's value (`torq::app::secret_set`, what its masked fields call), without
# the window: torqcli's package, building its one example (cli/examples/secret_set.rs) and nothing else. For the
# VM tests alone (tests/secrets.nix, tests/desktop.nix); no system has it.
{ pkgs, self }:

self.packages.${pkgs.stdenv.hostPlatform.system}.torq.overrideAttrs (_: {
  pname = "torq-secret-set-example";
  cargoBuildFlags = [ "--example" "secret_set" ];
  doCheck = false;
  installPhase = ''
    install -Dm755 "$(find . -path '*/release/examples/secret_set' -type f)" $out/bin/torq-secret-set-example
  '';
})

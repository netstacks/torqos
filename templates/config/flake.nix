# Per-machine Kestrel config repo. Lives at /var/lib/kestrel/config and is written ONLY by
# kestrel-broker after it has been initialised. Every change here is a commit + ledger entry.
{
  inputs = {
    # The Kestrel distribution. While developing Kestrel itself, point this at your checkout
    # instead, e.g. "path:/home/<you>/kestrel". Bumping it is itself a broker-approved commit.
    kestrel.url = "github:netstacks/kestrel";
    nixpkgs.follows = "kestrel/nixpkgs";
  };

  outputs = { kestrel, ... }: {
    nixosConfigurations.kestrel-utm = kestrel.lib.mkSystem {
      system = "aarch64-linux";
      # Installed software is read from packages.json and users/<name>.packages.json here
      # (`kestrel pkg add`; ADR-011).
      configDir = ./.;
      modules = [
        ./machine.nix
        ./system.nix
        ./desktop.nix
      ];
    };
  };
}

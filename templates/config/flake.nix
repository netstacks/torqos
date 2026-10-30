# Per-machine Kestrel config repo. Lives at /var/lib/kestrel/config and is written ONLY by
# kestrel-broker after it has been initialised. Every change here is a commit + ledger entry.
{
  inputs = {
    # During development: your local checkout of the Kestrel repo.
    # Later: "github:<owner>/kestrel". Bumping this is itself a broker-approved commit.
    kestrel.url = "path:/home/casey/kestrel";
    nixpkgs.follows = "kestrel/nixpkgs";
  };

  outputs = { kestrel, ... }: {
    nixosConfigurations.kestrel-utm = kestrel.lib.mkSystem {
      system = "aarch64-linux";
      modules = [
        ./machine.nix
        ./system.nix
        ./desktop.nix
        ./users/casey.nix
      ];
    };
  };
}

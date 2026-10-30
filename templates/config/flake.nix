# Per-machine TorqOS config repo. Lives at /var/lib/torq/config and is written ONLY by
# torq-broker after it has been initialised. Every change here is a commit + ledger entry.
{
  inputs = {
    # The TorqOS distribution. While developing TorqOS itself, point this at your checkout
    # instead, e.g. "path:/home/<you>/torq". Bumping it is itself a broker-approved commit.
    torq.url = "github:netstacks/torqos";
    nixpkgs.follows = "torq/nixpkgs";
  };

  outputs = { torq, ... }: {
    # Always `torq`, whatever the host is called: the broker builds this name (torq.broker.configName).
    nixosConfigurations.torq = torq.lib.mkSystem {
      system = "aarch64-linux";
      # Installed software is read from packages.json and users/<name>.packages.json here
      # (`torq pkg add`; ADR-011).
      configDir = ./.;
      modules = [
        ./machine.nix
        ./system.nix
        ./desktop.nix
      ];
    };
  };
}

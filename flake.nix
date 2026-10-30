{
  description = "Kestrel — a secure, AI-native, reversible laptop OS built on NixOS";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    # TODO(M4): lanzaboote for owner-key Secure Boot + measured boot.
    # lanzaboote = {
    #   url = "github:nix-community/lanzaboote";
    #   inputs.nixpkgs.follows = "nixpkgs";
    # };
  };

  outputs = { self, nixpkgs, ... }:
    let
      lib = nixpkgs.lib;
      linuxSystems = [ "x86_64-linux" "aarch64-linux" ];
      allSystems = linuxSystems ++ [ "aarch64-darwin" ];
      forSystems = systems: f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      # ── Packages ─────────────────────────────────────────────────────────────
      packages = forSystems allSystems (pkgs: {
        kestrel-broker = pkgs.callPackage ./broker/package.nix { };
        kestrel = pkgs.callPackage ./cli/package.nix { };
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.kestrel-broker;
      });

      # ── The distribution, as one NixOS module ────────────────────────────────
      nixosModules.default = { pkgs, lib, ... }: {
        imports = [ ./modules ];
        kestrel.broker.package =
          lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.kestrel-broker;
        kestrel.cli.package = lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.kestrel;
      };

      # Helper used by per-machine config repos (see templates/config).
      # `configDir = ./.;` lets the machine's package lists (packages.json, users/*.packages.json)
      # be read from the config repo; see modules/packages.nix.
      lib.mkSystem = { system, modules ? [ ], configDir ? null }:
        nixpkgs.lib.nixosSystem {
          modules = [
            self.nixosModules.default
            { nixpkgs.hostPlatform = system; kestrel.packages.configDir = configDir; }
          ] ++ modules;
        };

      # ── Throwaway dev VMs: nix build .#nixosConfigurations.dev-vm-<arch>.config.system.build.vm
      nixosConfigurations = lib.genAttrs [ "dev-vm-aarch64" "dev-vm-x86_64" ] (name:
        self.lib.mkSystem {
          system = if name == "dev-vm-aarch64" then "aarch64-linux" else "x86_64-linux";
          modules = [ ./hosts/dev-vm ];
        });

      # ── Template for the per-machine config repo the broker manages ─────────
      templates.config = {
        path = ./templates/config;
        description = "Kestrel per-machine config repo (lives at /var/lib/kestrel/config)";
      };

      # ── Tests ────────────────────────────────────────────────────────────────
      checks = forSystems linuxSystems (pkgs: {
        broker = self.packages.${pkgs.stdenv.hostPlatform.system}.kestrel-broker;
        trust-loop = import ./tests/trust-loop.nix { inherit pkgs self; };
      });

      # ── Dev shell (works on the Mac too) ─────────────────────────────────────
      devShells = forSystems allSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [ cargo rustc rustfmt clippy rust-analyzer git nixfmt-rfc-style ];
        };
      });
    };
}

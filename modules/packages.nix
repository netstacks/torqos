# Installed software, as data. The package lists live in the machine's config repo as JSON so that
# tools (the `torq pkg` CLI, later the agent's MCP packages server) can change them by proposing
# a plain diff through the broker, instead of editing Nix code:
#
#   packages.json                 system-wide packages     (system tier: explicit approval)
#   users/<name>.packages.json    one user's packages      (user tier)
#
# Each file is a JSON list of nixpkgs attribute paths, e.g. [ "htop", "python3Packages.numpy" ].
{ config, lib, pkgs, ... }:

let
  cfg = config.torq.packages;
  dir = cfg.configDir;

  # A package is a top-level nixpkgs attribute, or one level under one of these sets of ordinary
  # packages. Anything deeper (pkgsCross.*.hello, nixosTests.*, hello.tests.*) is refused: those are
  # derivations too, but a user-tier change could use them to start hour-long builds or VM tests
  # while holding the broker lock.
  packageSets = [
    "python3Packages" "haskellPackages" "nodePackages" "perlPackages" "rubyPackages" "luaPackages"
    "ocamlPackages" "rPackages" "vimPlugins" "emacsPackages" "gnomeExtensions" "kdePackages"
    "texlivePackages" "fishPlugins"
  ];
  isAttrName = s: builtins.match "[A-Za-z_][A-Za-z0-9_'+-]*" s != null;

  resolve = owner: name:
    let
      path = lib.splitString "." name;
      shaped = lib.all isAttrName path
        && (builtins.length path == 1 || (builtins.length path == 2 && builtins.elem (builtins.head path) packageSets));
      pkg = lib.attrByPath path null pkgs;
      what = "torq.packages: '${name}' (${owner})";
    in
    if !shaped then
      throw "${what} is not a package name: use a top-level nixpkgs attribute, or one level under ${lib.concatStringsSep ", " packageSets}"
    else if !(lib.isDerivation pkg) then
      throw "${what} is not a package in nixpkgs"
    else if !(pkg.meta.available or true) then
      throw "${what} is not available here: unfree, broken, insecure, or unsupported on ${pkgs.stdenv.hostPlatform.system}"
    else
      pkg;

  readList = file: if builtins.pathExists file then lib.importJSON file else [ ];

  # Same shape the broker treats as user tier (broker/src/policy.rs, is_user_data).
  isUserName = s: builtins.match "[a-z_][a-z0-9_-]*" s != null;
  usersDir = dir + "/users";
  userFiles =
    if dir != null && builtins.pathExists usersDir then
      lib.mapAttrs (n: _:
        let user = lib.removeSuffix ".packages.json" n;
        in if isUserName user then user else throw "torq.packages: users/${n} does not name a valid user"
      ) (lib.filterAttrs (n: t: t == "regular" && lib.hasSuffix ".packages.json" n) (builtins.readDir usersDir))
    else
      { };
in
{
  options.torq.cli.package = lib.mkOption {
    type = lib.types.package;
    description = "The `torq` command line (set by the TorqOS flake).";
  };

  options.torq.packages = {
    configDir = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        The machine's config repo. When set, `packages.json` and `users/<name>.packages.json` in it
        are read into `system` and `user`. Set by `torq.lib.mkSystem { configDir = ./.; }`.
      '';
    };
    system = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "nixpkgs attribute paths installed system-wide.";
    };
    user = lib.mkOption {
      type = lib.types.attrsOf (lib.types.listOf lib.types.str);
      default = { };
      description = "Per-user nixpkgs attribute paths, keyed by user name.";
    };
  };

  config = {
    torq.packages.system = lib.mkIf (dir != null) (readList (dir + "/packages.json"));
    torq.packages.user = lib.mkIf (dir != null) (
      lib.mapAttrs' (n: user: lib.nameValuePair user (lib.importJSON (usersDir + "/${n}"))) userFiles
    );

    assertions = lib.mapAttrsToList (user: _: {
      assertion = config.users.users.${user}.isNormalUser or false;
      message = "torq.packages: users/${user}.packages.json names '${user}', which is not a normal user on this machine.";
    }) cfg.user;

    environment.systemPackages = [ config.torq.cli.package ] ++ map (resolve "system") cfg.system;
    users.users = lib.mapAttrs (user: names: { packages = map (resolve "user ${user}") names; }) cfg.user;
  };
}

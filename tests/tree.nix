# Evaluation test: the configuration tree's data files (docs/CLI-DESIGN.md) are read as documented,
# and anything malformed fails evaluation — the modules are the authority on what is valid, whoever
# wrote the file. Each case is a tiny config repo under tests/tree/<case>/.
#
# Evaluation only (no VM): a failing case makes `nix flake check` fail with the list of mismatches.
{ pkgs, self }:

let
  inherit (pkgs) lib;

  configOf = case: (self.lib.mkSystem {
    system = pkgs.stdenv.hostPlatform.system;
    # "template": the config repo template's own data files (its .nix modules need hardware).
    configDir = if case == "template" then ../templates/config else ./tree + "/${case}";
    modules = [ { torq.user = "dev"; } ];
  }).config;

  # The value, or null if evaluating it fails (a throw).
  tryValue = v: let r = builtins.tryEval (builtins.deepSeq v v); in if r.success then r.value else null;

  host = case: let c = configOf case; in tryValue { host = c.networking.hostName; zone = c.time.timeZone; };

  # TorqOS's own failed assertions (the case's minimal system fails NixOS's, e.g. no fileSystems).
  torqFailures = c: map (a: a.message) (lib.filter (a: !a.assertion && lib.hasPrefix "torq:" a.message) c.assertions);
  # The CLI styles, or null if evaluation throws or a TorqOS assertion fails.
  cli = case:
    let c = configOf case; in
    tryValue (if torqFailures c != [ ] then throw "assertion" else {
      inherit (c.torq.cli) style userStyles;
      etc = builtins.fromJSON c.environment.etc."torq/cli.json".text;
    });
  cliExpect = {
    host-empty = { style = "juniper"; userStyles = { }; etc = { style = "juniper"; users = { }; }; };
    cli-good = { style = "cisco"; userStyles = { dev = "juniper"; }; etc = { style = "cisco"; users = { dev = "juniper"; }; }; };
    cli-user-only = { style = "juniper"; userStyles = { dev = "cisco"; }; etc = { style = "juniper"; users = { dev = "cisco"; }; }; };
  };
  cliInvalid = [ "cli-bad-style" "cli-bad-extra" "cli-bad-list" "cli-bad-leaf" "cli-bad-user" "cli-bad-symlink" ];

  defaults = host "host-empty";
  expect = {
    host-good = { host = "my-laptop"; zone = "Europe/Berlin"; };
    host-utc = { host = defaults.host; zone = "UTC"; };
    host-ok-63 = { host = lib.concatStrings (lib.replicate 63 "a"); zone = defaults.zone; };
    host-link-zone = { host = defaults.host; zone = "US/Eastern"; };
    template = { host = "torq"; zone = "America/New_York"; };
  };
  invalid = [
    "host-bad-hyphen" "host-bad-dot" "host-bad-long" "host-bad-number"
    "host-bad-area" "host-bad-escape" "host-bad-dotdot" "host-bad-nowhere"
    "host-bad-key" "host-bad-list"
    # Files under zoneinfo that are not zones tzdata lists; "not set"; null; "ask DHCP"; and an
    # underscore, which NixOS's own type allows: only host-rules.nix refuses it.
    "host-bad-posix-dir" "host-bad-right" "host-bad-leapseconds" "host-bad-factory"
    "host-bad-null" "host-bad-empty-name" "host-bad-underscore"
  ];

  # The rules themselves, not only NixOS's checks behind them.
  rules = import ../modules/host-rules.nix { inherit lib; inherit (pkgs) tzdata; };
  ruleCases = [
    [ "hostNameOk" "torq" true ] [ "hostNameOk" "a-1" true ] [ "hostNameOk" "my_laptop" false ]
    [ "hostNameOk" "-a" false ] [ "hostNameOk" "a-" false ] [ "hostNameOk" "a.b" false ]
    [ "hostNameOk" "" false ] [ "hostNameOk" 5 false ] [ "hostNameOk" (lib.concatStrings (lib.replicate 64 "a")) false ]
    [ "timeZoneOk" "UTC" true ] [ "timeZoneOk" "Europe/Berlin" true ] [ "timeZoneOk" "US/Eastern" true ]
    [ "timeZoneOk" "posix/Europe" false ] [ "timeZoneOk" "posix/Europe/Berlin" false ] [ "timeZoneOk" "right/UTC" false ]
    [ "timeZoneOk" "leapseconds" false ] [ "timeZoneOk" "Factory" false ] [ "timeZoneOk" "America" false ]
    [ "timeZoneOk" "europe/berlin" false ] [ "timeZoneOk" "../etc/passwd" false ] [ "timeZoneOk" null false ]
  ];
  ruleMismatches = lib.concatMap (c:
    let f = builtins.elemAt c 0; arg = builtins.elemAt c 1; want = builtins.elemAt c 2; in
    lib.optional (rules.${f} arg != want) "${f} ${builtins.toJSON arg}: want ${builtins.toJSON want}"
  ) ruleCases;

  mismatches =
    ruleMismatches
    ++ lib.optional (defaults == null) "host-empty: does not evaluate"
    ++ lib.concatLists (lib.mapAttrsToList (case: want:
      let got = host case; in lib.optional (got != want) "${case}: got ${builtins.toJSON got}, want ${builtins.toJSON want}"
    ) expect)
    ++ lib.concatMap (case: lib.optional (host case != null) "${case}: evaluated, but must be refused") invalid
    ++ lib.concatLists (lib.mapAttrsToList (case: want:
      let got = cli case; in lib.optional (got != want) "${case}: cli got ${builtins.toJSON got}, want ${builtins.toJSON want}"
    ) cliExpect)
    ++ lib.concatMap (case: lib.optional (cli case != null) "${case}: cli evaluated, but must be refused") cliInvalid;
in
if mismatches != [ ] then
  throw "tree test:\n  ${lib.concatStringsSep "\n  " mismatches}"
else
  pkgs.runCommand "torq-tree-test" { } ''
    echo "rules: ${toString (builtins.length ruleCases)} cases" > $out
    echo "host: ${toString (builtins.length invalid)} invalid cases refused, ${toString (builtins.length (lib.attrNames expect) + 1)} valid ones read" >> $out
    echo "cli: ${toString (builtins.length cliInvalid)} invalid cases refused, ${toString (builtins.length (lib.attrNames cliExpect))} valid ones read" >> $out
  ''

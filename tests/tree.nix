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

  # The schema (modules/schema.nix) is consistent: every node's parent exists, files and keys are
  # where the kind says, `users <name> …` is user tier and nothing else is, and the user-tier leaves
  # it describes are exactly the leaves a users/<name>/ folder may hold — no setting without a node,
  # no node without a reader.
  schemaConfig = configOf "cli-good";
  schema = builtins.fromJSON schemaConfig.environment.etc."torq/schema.json".text;
  nodes = schema.nodes;
  parentOf = path: lib.concatStringsSep " " (lib.init (lib.splitString " " path));
  underUser = path: lib.hasPrefix "users <name>" path;
  schemaMismatches = lib.concatLists (lib.mapAttrsToList (path: n:
    let has = k: n ? ${k}; in
    lib.optional (lib.length (lib.splitString " " path) > 1 && !(nodes ? ${parentOf path})) "schema ${path}: no parent node"
    ++ lib.optional (n.kind == "leaf" && !(has "file" && has "key")) "schema ${path}: a leaf needs file and key"
    ++ lib.optional (n.kind == "leaf-list" && !(has "file") || n.kind == "leaf-list" && has "key") "schema ${path}: a leaf-list is a whole file"
    ++ lib.optional (builtins.elem n.kind [ "container" "list" ] && (has "file" || has "key")) "schema ${path}: a ${n.kind} has no file"
    ++ lib.optional ((n.tier == "user") != underUser path) "schema ${path}: tier ${n.tier}"
    ++ lib.optional (has "file" && underUser path && !(lib.hasPrefix "users/<name>/" n.file)) "schema ${path}: file outside users/<name>/"
    ++ lib.optional (has "file" && !(underUser path) && lib.hasPrefix "users/" n.file) "schema ${path}: system tier inside users/"
  ) nodes)
  # Each setting is in exactly one place: no two nodes share a (file, key), and a leaf-list's file is
  # its own (a leaf-list is the whole file).
  ++ (let
    withFile = lib.filter (n: n ? file) (lib.attrValues nodes);
    places = map (n: "${n.file}#${n.key or "*"}") withFile;
    listFiles = map (n: n.file) (lib.filter (n: n.kind == "leaf-list") withFile);
    sharedLists = lib.filter (f: lib.count (n: n.file == f) withFile > 1) listFiles;
  in lib.optional (lib.length (lib.unique places) != lib.length places) "schema: two nodes share a file and key"
    ++ lib.optional (sharedLists != [ ]) "schema: a leaf-list's file is also used elsewhere: ${toString sharedLists}")
  # `users` offers only names the broker treats as user tier, and no descriptions (full names).
  ++ (let
    withOdd = (self.lib.mkSystem {
      system = pkgs.stdenv.hostPlatform.system;
      configDir = ./tree/cli-good;
      modules = [ {
        torq.user = "dev";
        users.users.dev.description = "Secret Person"; # a listed user's full name must not be copied
        users.users."Bad.Name".isNormalUser = true;
      } ];
    }).config;
    users = (builtins.fromJSON withOdd.environment.etc."torq/schema.json".text).nodes.users;
  in lib.optional (users.values ? "Bad.Name" || !(users.values ? dev)) "schema: users ${builtins.toJSON users.values}"
    ++ lib.optional (lib.hasInfix "Secret" (builtins.toJSON users)) "schema: users carries descriptions"
    ++ lib.optional ((users.type or null) != "user-name") "schema: users has no name type")
  ++ (let
    described = lib.unique (map (n: lib.removePrefix "users/<name>/" n.file)
      (lib.filter (n: n ? file && n.tier == "user") (lib.attrValues nodes)));
    allowed = schemaConfig.torq.packages.userLeaves;
  in lib.optional (lib.sort lib.lessThan described != lib.sort lib.lessThan allowed)
    "schema: user-tier files ${builtins.toJSON described} != allowed user leaves ${builtins.toJSON allowed}")
  ++ lib.optional (!(nodes.users.values ? dev)) "schema: users lists the machine's normal users (dev)"
  ++ lib.optional (!(nodes."system time-zone".values ? "Europe/Berlin") || nodes."system time-zone".values ? Factory)
    "schema: time zones are tzdata's list"
  ++ lib.optional (lib.attrNames nodes.services.values != lib.attrNames schemaConfig.torq.services.catalog)
    "schema: services are the catalog";

  # torqcli's tests parse a copy of this schema (cli/src/schema-fixture.json): it must be the real one.
  # After a schema change, regenerate it (the command is in cli/src/tree.rs's tests).
  fixtureMismatch = lib.optional (builtins.fromJSON (builtins.readFile ../cli/src/schema-fixture.json) != schema)
    "cli/src/schema-fixture.json is not the schema the modules produce: regenerate it";

  mismatches =
    ruleMismatches
    ++ fixtureMismatch
    ++ schemaMismatches
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
    echo "schema: ${toString (builtins.length (lib.attrNames nodes))} nodes consistent" >> $out
    echo "host: ${toString (builtins.length invalid)} invalid cases refused, ${toString (builtins.length (lib.attrNames expect) + 1)} valid ones read" >> $out
    echo "cli: ${toString (builtins.length cliInvalid)} invalid cases refused, ${toString (builtins.length (lib.attrNames cliExpect))} valid ones read" >> $out
  ''

# The "no delegation" check on the units as built (modules/agent-units-check.nix): a unit file or drop-in a
# package brings for a torq-agent@ unit is refused when it delegates, as this configuration's own is at
# evaluation (tests/agent-assertions.nix). No VM: each case is a small directory laid out as NixOS lays out
# /etc/systemd/system (links into the store), given to the very check the module adds to the system's build.
# A case that should be refused is built with testBuildFailure, and its log must name the file.
#
# Then the same check as a real configuration carries it (its `system.checks`), with a package that brings a
# delegating unit file or drop-in, and without: that configuration's own unit directory, as NixOS builds it
# (TorqOS's programs replaced by stand-ins, so nothing long is built).
{ pkgs, self }:

let
  lib = pkgs.lib;
  check = import ../modules/agent-units-check.nix { inherit pkgs; };
  # A package's unit files, as a package has them: real files under lib/systemd/system.
  package = name: files: pkgs.runCommand name { } (lib.concatStrings (lib.mapAttrsToList (path: text: ''
    mkdir -p "$out/lib/systemd/system/$(dirname ${lib.escapeShellArg path})"
    printf '%s' ${lib.escapeShellArg text} > $out/lib/systemd/system/${lib.escapeShellArg path}
  '') files));
  # A unit directory as NixOS makes it: a package's unit files linked in, its directories linked file by file.
  tree = name: files: extra:
    let p = package "${name}-package" files; in
    pkgs.runCommand "${name}-units" { nativeBuildInputs = [ pkgs.lndir ]; } ''
      mkdir -p $out
      for fn in ${p}/lib/systemd/system/*; do
        if [ -d "$fn" ]; then
          mkdir -p "$out/$(basename "$fn")"
          lndir -silent "$fn" "$out/$(basename "$fn")"
        else
          ln -s "$fn" $out/
        fi
      done
      ${extra}
    '';
  ours = "[Service]\nExecStart=/bin/true\nProtectControlGroups=true\n";
  delegates = "[Service]\nDelegate=yes\n";
  passes = name: files: extra: check (tree name files extra);
  # Refused, and for that file: the failed build's log says `says`.
  refuses = name: files: extra: says:
    pkgs.runCommand "refused-${name}" { failed = pkgs.testers.testBuildFailure (check (tree name files extra)); } ''
      if ! grep -q -F -- ${lib.escapeShellArg says} "$failed/testBuildFailure.log"; then
        echo "agent-units: ${name}: refused, but not saying ${lib.escapeShellArg says}:" >&2
        cat "$failed/testBuildFailure.log" >&2
        exit 1
      fi
      grep -q -F "no Delegate or DelegateSubgroup in any torq-agent@ unit" "$failed/testBuildFailure.log"
      touch $out
    '';
  cases = [
    # The controls: the agent's units as they are, a comment that names the word, another unit's own Delegate
    # (a container manager's, say), a masked unit, the directories of links.
    (passes "plain" { "torq-agent@alice.service" = ours; "torq-agent@alice.socket" = "[Socket]\nListenStream=/run/x\n"; } "")
    (passes "a-comment" { "torq-agent@alice.service" = ours + "# No Delegate here.\n; Delegate=yes\n"; } "")
    (passes "another-units" { "torq-agent@alice.service" = ours; "docker.service" = delegates; "docker.service.d/x.conf" = delegates; } "")
    (passes "masked" { "torq-agent@alice.service" = ours; } "ln -s /dev/null $out/torq-agent@bob.service")
    (passes "links-by-name" { "torq-agent@alice.service" = ours; } "mkdir $out/torq-agent@alice.service.requires && ln -s ../nothing.socket $out/torq-agent@alice.service.requires/")
    # A package's unit file or drop-in, each place systemd reads one for torq-agent@<person>.service.
    (refuses "a-packages-unit" { "torq-agent@alice.service" = delegates; } "" "torq-agent@alice.service: Delegate=yes")
    (refuses "a-packages-template" { "torq-agent@.service" = "[Service]\nDelegateSubgroup=x\n"; } "" "torq-agent@.service: DelegateSubgroup=x")
    (refuses "a-packages-drop-in" { "torq-agent@alice.service" = ours; "torq-agent@alice.service.d/10-pkg.conf" = delegates; } "" "torq-agent@alice.service.d/10-pkg.conf: Delegate=yes")
    (refuses "the-templates-drop-in" { "torq-agent@alice.service" = ours; "torq-agent@.service.d/pkg.conf" = delegates; } "" "torq-agent@.service.d/pkg.conf: Delegate=yes")
    (refuses "the-dash-drop-in" { "torq-agent@alice.service" = ours; "torq-.service.d/pkg.conf" = delegates; } "" "torq-.service.d/pkg.conf: Delegate=yes")
    (refuses "every-services-drop-in" { "torq-agent@alice.service" = ours; "service.d/pkg.conf" = delegates; } "" "service.d/pkg.conf: Delegate=yes")
    # However the line is written: spaces before the key and around the sign, "no" as well (as at evaluation).
    (refuses "spaces" { "torq-agent@alice.service" = "[Service]\n  \tDelegate = cpu memory\n"; } "" "torq-agent@alice.service: ")
    (refuses "delegate-no" { "torq-agent@alice.service" = "[Service]\nDelegate=no\n"; } "" "torq-agent@alice.service: Delegate=no")
    # NixOS's own layout when a package brings the unit this configuration also writes: the package's file
    # stays the unit, the configuration's becomes its overrides.conf. The package's Delegate is still found.
    (refuses "ours-over-a-packages" { "torq-agent@alice.service" = delegates; }
      "mkdir $out/torq-agent@alice.service.d && ln -s ${pkgs.writeText "overrides.conf" ours} $out/torq-agent@alice.service.d/overrides.conf"
      "torq-agent@alice.service: Delegate=yes")
    # A file that is not in the store cannot be read here, so is refused rather than let by.
    (refuses "a-link-out-of-the-store" { "torq-agent@alice.service" = ours; } "mkdir $out/torq-agent@alice.service.d && ln -s /etc/torq/elsewhere.conf $out/torq-agent@alice.service.d/x.conf" "x.conf: a link out of the store")
    (refuses "a-link-to-nothing" { "torq-agent@bob.service" = ours; } "ln -s /nix/store/nothing-here $out/torq-agent@alice.service" "torq-agent@alice.service: a link out of the store, or to nothing")
  ];

  # The check as a configuration carries it: a real configuration's unit directory, as NixOS builds it.
  # TorqOS's own programs are not what is checked here, and building them is the long part: stand-ins.
  stub = pkgs.runCommand "torq-stand-in" { version = "0"; meta.mainProgram = "torq-stand-in"; } "mkdir -p $out/bin";
  configOf = extra: (import (pkgs.path + "/nixos/lib/eval-config.nix") {
    system = pkgs.stdenv.hostPlatform.system;
    modules = [ self.nixosModules.default {
      torq.user = "dev";
      boot.loader.grub.enable = false;
      fileSystems."/" = { device = "/dev/disk/by-label/root"; fsType = "ext4"; };
      torq.agent.doctor = { enable = true; endpoint = "https://api.example.org/v1"; model = "m"; package = stub; };
      torq.broker.package = stub;
      torq.secrets.package = stub;
      torq.cli.package = stub;
    } extra ];
  }).config;
  carried = config: lib.filter (c: (c.name or "") == "torq-agent-units-delegate-nothing") config.system.checks;
  ofSystem = extra:
    let found = carried (configOf extra); in
    if lib.length found == 1 then lib.head found else throw "agent-units: the configuration carries ${toString (lib.length found)} such checks, not one";
  refusesSystem = name: files: says:
    pkgs.runCommand "refused-${name}" { failed = pkgs.testers.testBuildFailure (ofSystem { systemd.packages = [ (package "brings-${name}" files) ]; }); } ''
      grep -F -- ${lib.escapeShellArg says} "$failed/testBuildFailure.log"
      touch $out
    '';
  # The system's build depends on the check (NixOS's system.checks: named in the system's own derivation), so a
  # configuration that fails it is not built. Read from the derivation, which evaluation gives; nothing is built.
  dependsOnIt =
    let
      config = configOf { };
      named = builtins.unsafeDiscardStringContext (toString (config.system.build.toplevel.passedChecks or ""));
      it = builtins.unsafeDiscardStringContext "${ofSystem { }}";
    in
      if lib.hasInfix it named then "the system's build depends on it"
      else throw "agent-units: the system's build does not depend on the check (${it} is not among its checks)";
  # Only where the agent is resident, as the assertions are: with doctor mode off there is no such unit.
  notWithout =
    if carried (configOf { torq.agent.doctor.enable = lib.mkForce false; }) == [ ] then "not carried where no agent is resident"
    else throw "agent-units: the check is carried where no agent is resident";
  systemCases = [
    # The controls: the configuration as it is, and a package that brings a unit of its own, delegating (its own business).
    (ofSystem { })
    (ofSystem { systemd.packages = [ (package "brings-its-own" { "container-manager.service" = "[Service]\nExecStart=/bin/true\nDelegate=yes\n"; }) ]; })
    (refusesSystem "a-drop-in" { "torq-agent@dev.service.d/10-pkg.conf" = delegates; } "torq-agent@dev.service.d/10-pkg.conf: Delegate=yes")
    (refusesSystem "every-services-drop-in" { "service.d/10-pkg.conf" = delegates; } "service.d/10-pkg.conf: Delegate=yes")
    # The unit itself: NixOS keeps the package's file as the unit and makes this configuration's its overrides.conf.
    (refusesSystem "the-unit-itself" { "torq-agent@dev.service" = delegates; } "torq-agent@dev.service: Delegate=yes")
  ];
in
pkgs.runCommand "torq-agent-units" { } ''
  echo ${lib.escapeShellArg (toString (cases ++ systemCases ++ [ dependsOnIt notWithout ]))} > $out
''

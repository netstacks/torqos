# The machine's identity, as data: `system host-name` and `system time-zone` in the configuration tree
# (docs/CLI-DESIGN.md), stored in the config repo as `system/host.json` (system tier):
#
#   { "host-name": "torq-utm", "time-zone": "Europe/Berlin" }
#
# Both are optional. This module is the authority on what is valid, whoever wrote the file (torqcli,
# the agent, a raw diff): anything else fails evaluation, so the broker's rebuild fails and the change
# is undone. Changing the host name never changes which configuration the broker builds
# (`nixosConfigurations.torq`, torq.broker.configName).
{ config, lib, pkgs, ... }:

let
  dir = config.torq.packages.configDir;
  file = dir + "/system/host.json";
  where = "torq: system/host.json";

  # RFC 1123: one label, letters, digits and hyphens, not starting or ending with a hyphen, at most
  # 63 characters. (No dots: a fully qualified name is a domain setting, not a host name.) NixOS's
  # own type for networking.hostName refuses these too; this check is for the message. The time
  # zone has no such second check: NixOS takes any string.
  hostNameOk = s: builtins.match "[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?" s != null;

  # A zone tzdata knows: an Area/Location name of plain segments (never `..` or an absolute path),
  # naming a file in zoneinfo (many are links: UTC, US/Eastern), not a folder ("America" alone is
  # not a time zone).
  zoneinfo = "${pkgs.tzdata}/share/zoneinfo";
  timeZoneOk = s:
    let
      segments = lib.splitString "/" s;
      parent = lib.concatStringsSep "/" (lib.init segments);
      entries = builtins.readDir (zoneinfo + lib.optionalString (parent != "") "/${parent}");
    in
    builtins.match "[A-Za-z0-9_+-]+(/[A-Za-z0-9_+-]+)*" s != null
      && builtins.all (seg: builtins.match "[.]+" seg == null) segments
      && builtins.pathExists "${zoneinfo}/${s}"
      && builtins.elem (entries.${lib.last segments} or null) [ "regular" "symlink" ];

  known = [ "host-name" "time-zone" ];

  settings =
    let
      v = lib.importJSON file;
      unknown = lib.subtractLists known (lib.attrNames v);
      host = v.host-name or null;
      zone = v.time-zone or null;
    in
    if !(builtins.isAttrs v) then
      throw "${where} must be a JSON object with \"host-name\" and/or \"time-zone\""
    else if unknown != [ ] then
      throw "${where}: \"${lib.head unknown}\" is not a setting (it holds ${lib.concatStringsSep ", " known})"
    else if host != null && !(builtins.isString host && hostNameOk host) then
      throw "${where}: host-name ${builtins.toJSON host} must be one label of letters, digits and hyphens, at most 63 characters, not starting or ending with a hyphen"
    else if zone != null && !(builtins.isString zone && timeZoneOk zone) then
      throw "${where}: time-zone ${builtins.toJSON zone} is not a time zone (e.g. Europe/Berlin, America/New_York, UTC)"
    else
      { inherit host zone; };

  present = dir != null && builtins.pathExists file;
in
{
  config = lib.mkIf present {
    # Normal priority: setting the host name in Nix as well is a conflict, reported, not silently
    # overridden one way or the other.
    networking.hostName = lib.mkIf (settings.host != null) settings.host;
    time.timeZone = lib.mkIf (settings.zone != null) settings.zone;
  };
}

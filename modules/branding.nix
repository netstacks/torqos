# TorqOS's name and look wherever the system shows them (branding/README.md): what the machine says
# it is (/etc/os-release, /etc/lsb-release, the boot menu, the boot's own lines), the console's
# greeting before login, the banner over first-boot setup, and fastfetch's logo. The shell's prompt
# is its own (modules/shell.nix) and not touched here.
#
# TorqOS is NixOS underneath, and says so: os-release's ID_LIKE is `nixos`, and its VERSION names
# the NixOS release it is built on. The version is TorqOS's own (torqcli's, one number for both).
{ config, lib, pkgs, ... }:

let
  version = config.torq.cli.package.version;
  nixos = config.system.nixos.release;
  # The public repository: each release's source, its images, and where to report a problem.
  repo = "https://github.com/netstacks/torqos-releases";
  share = "${config.torq.branding.package}/share/torqos";
  # agetty reads `\` in /etc/issue as the start of an escape (`\n` the host name, `\l` the
  # terminal): the logo's own backslashes, doubled, are drawn as they are.
  wordmark = lib.replaceStrings [ "\\" ] [ "\\\\" ] (builtins.readFile ../branding/terminal/torqos-wordmark-ansi.txt);
  # fastfetch's configuration, its logo found where TorqOS installs it (the file names a person's).
  fastfetchLogo = "~/.config/fastfetch/torqos-logo-fastfetch.txt";
  fastfetch = builtins.readFile ../branding/terminal/fastfetch-config.jsonc;
in
{
  options.torq.branding.package = lib.mkOption {
    type = lib.types.package;
    default = pkgs.callPackage ../branding/package.nix { };
    defaultText = lib.literalExpression "pkgs.callPackage ../branding/package.nix { }";
    description = "TorqOS's icons, terminal logos and fastfetch configuration (branding/).";
  };

  config = {
    system.nixos = {
      distroName = "TorqOS";
      distroId = "torqos";
      vendorName = "TorqOS";
      vendorId = "torqos";
      # Each value replaces NixOS's (BUILD_ID, the NixOS build, and SUPPORT_END, its release's end
      # of support, stay: TorqOS is supported as long as the NixOS release under it).
      extraOSReleaseArgs = {
        PRETTY_NAME = "TorqOS ${version}";
        VERSION = "${version} (NixOS ${nixos})";
        VERSION_ID = version;
        VERSION_CODENAME = "";
        CPE_NAME = "cpe:/o:torqos:torqos:${version}";
        LOGO = "distributor-logo-torqos";
        ANSI_COLOR = "38;5;208";
        HOME_URL = repo;
        VENDOR_URL = repo;
        DOCUMENTATION_URL = "${repo}/tree/stable/docs";
        SUPPORT_URL = "${repo}/issues";
        BUG_REPORT_URL = "${repo}/issues";
        DEFAULT_HOSTNAME = "torq";
      };
      extraLSBReleaseArgs = {
        LSB_VERSION = "${version} (NixOS ${nixos})";
        DISTRIB_RELEASE = version;
        DISTRIB_CODENAME = "";
        DISTRIB_DESCRIPTION = "TorqOS ${version} (NixOS ${nixos})";
      };
    };

    # The console, before login: the wordmark, what this is, and where to start.
    environment.etc.issue.text = ''

      ${wordmark}
      TorqOS ${version} (NixOS ${nixos}) on \n, \l
      Log in, then run `torqcli` to see and change this machine's configuration.

    '';
    # The wordmark over first-boot setup's questions (cli/src/setup.rs).
    environment.etc."torq/banner".source = "${share}/torqos-wordmark-ansi.txt";
    # fastfetch, for whoever installs it: TorqOS's logo and what this machine is.
    environment.etc."xdg/fastfetch/config.jsonc".text =
      assert lib.assertMsg (lib.hasInfix fastfetchLogo fastfetch) "branding/terminal/fastfetch-config.jsonc no longer names ${fastfetchLogo}";
      lib.replaceStrings [ fastfetchLogo ] [ "${share}/torqos-logo-fastfetch.txt" ] fastfetch;
  };
}

# casey's NixOS settings. This is a module, so changing it is *system* tier like any .nix file.
# casey's apps go in users/casey.packages.json instead (user tier; `kestrel pkg add`).
# TODO(M3): home-manager, with its user-tier settings as data too.
{ pkgs, ... }:
{
  users.users.casey.packages = with pkgs; [ ];
}

# User-tier settings for casey (apps, theme). TODO(M3): home-manager.
{ pkgs, ... }:
{
  users.users.casey.packages = with pkgs; [ ];
}

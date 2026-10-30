# System-tier settings. Changes here require explicit approval.
{ ... }:
{
  # The owner's login name — the only place a user name is set. Their account, their package list
  # (users/<name>.packages.json) and their broker approvals all follow from it. Set it once at
  # setup: sudo sed -i "s/CHANGE-ME/$USER/" /var/lib/kestrel/config/system.nix
  kestrel.user = "CHANGE-ME";

  # Services (SSH included) are in services.json: `kestrel service list`. SSH there is keys only.
}

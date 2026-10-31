# System-tier settings. Changes here require explicit approval.
{ ... }:
{
  # The owner's login name — the only place a user name is set. Their account, their package list
  # (users/<name>/packages.json) and their broker approvals all follow from it. Set it once at
  # setup: sudo sed -i "s/CHANGE-ME/$USER/" /var/lib/torq/config/system.nix
  torq.user = "CHANGE-ME";

  # Services are in services.json: `show services` in torqcli. All off by default — SSH included: a new
  # machine has no keys yet, so it would only be listening. Install your key, then
  # `set services ssh` in torqcli (keys only, only you).
}

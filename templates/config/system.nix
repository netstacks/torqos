# System-tier settings. Changes here require explicit approval.
{ ... }:
{
  kestrel.user = "casey";

  # Dev convenience inside the VM: SSH in from macOS (key-based login recommended).
  services.openssh.enable = true;
}

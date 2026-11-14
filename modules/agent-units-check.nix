# The "no delegation" check, on the units as built (modules/agent.nix; KNOWN-GAPS 6zzo).
#
# The configuration's own check (agent.nix, delegatedUnits) reads `systemd.units`: what this configuration
# writes. A unit file or drop-in that a package brings (`systemd.packages`: its lib/systemd/system and
# etc/systemd/system) is not there. It is a file inside a built package, and evaluation cannot read it without
# building the package first. So this reads the finished directory instead, the one NixOS makes
# /etc/systemd/system from, where the packages' files and the configuration's are already put together. It is
# a build of its own that the system's build depends on (`system.checks`): a system that fails it is not built,
# so never switched to.
#
# `units`: that directory. What is read in it: every unit file and drop-in directory of a `torq-agent@` unit
# (an instance or the template), and the two drop-in directories systemd also applies to those units,
# `torq-.service.d` (the name cut at its dash) and `service.d` (every service). Refused: a line that sets a key
# beginning with `Delegate` (Delegate, DelegateSubgroup), and a file there that is not in the store (a link out
# of it, or to nothing: what it would say on the machine is not known here; /dev/null, a masked unit, is let by).
{ pkgs }:

units:
pkgs.runCommand "torq-agent-units-delegate-nothing" { inherit units; } ''
  bad=$TMPDIR/bad
  : > "$bad"
  for entry in "$units"/torq-agent@* "$units"/torq-.service.d "$units"/service.d; do
    # (No such entry: the pattern itself, or a name that is not there. A link to nothing is looked at below.)
    if [ ! -e "$entry" ] && [ ! -L "$entry" ]; then continue; fi
    case "$entry" in
      # Only links to other units, by name: nothing is read from them as settings.
      *.wants | *.requires | *.upholds) continue ;;
    esac
    if ! find -L "$entry" ! -type d -print0 > "$TMPDIR/files"; then
      echo "$entry: could not be read through" >> "$bad"
    fi
    while IFS= read -r -d "" file; do
      # Where the file really is: in the store (so the same on the machine as here), or /dev/null (a unit
      # masked). Anything else is not known here.
      real=$(realpath -e -- "$file" 2>/dev/null || true)
      case "$real" in
        /dev/null) ;;
        /nix/store/*)
          if [ ! -f "$real" ] || [ ! -r "$real" ]; then
            echo "$file: not a file that can be read" >> "$bad"
          elif grep -q -E '^[[:space:]]*Delegate' -- "$real"; then
            echo "$file: $(grep -E '^[[:space:]]*Delegate' -- "$real" | head -n 1)" >> "$bad"
          fi
          ;;
        *) echo "$file: a link out of the store, or to nothing: what it says on the machine is not known here" >> "$bad" ;;
      esac
    done < "$TMPDIR/files"
  done
  if [ -s "$bad" ]; then
    {
      echo "torq-agent: no Delegate or DelegateSubgroup in any torq-agent@ unit, whoever brings it: a unit file or a"
      echo "drop-in from a package (systemd.packages) counts as this configuration's own does. The broker takes the"
      echo "unit's control group for the resident agent, which only root may move a process into (a second line"
      echo "behind the resident account's uid). Found in the units as built:"
      cat "$bad"
    } >&2
    exit 1
  fi
  touch $out
''

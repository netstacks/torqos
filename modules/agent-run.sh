# torq-agent-run [--share-fd <n> <dir>] [--] [<command> …] | torq-agent-run --hold-frozen — started by `torq agent run`, through
# sudo, as root: it does one thing as root, then is the caller's own agent for good (ADR-051 3).
#
# As root, in a mount namespace of its own: every home hidden, and the one directory shared with the
# agent — the directory torqcli opened, as the person, handed over as descriptor <n> — bound where it
# is — by its name, then checked against that descriptor, device and inode (its name could have been
# pointed elsewhere; then nothing runs). Then it drops to
# the caller's own agent, torq-agent-<person> (setpriv: its uid and group, no capabilities, no new privileges) and runs the program in
# bubblewrap, in a process namespace of its own (nothing it starts outlives it): the people's runtime
# directories (their sockets: ssh-agent, D-Bus) hidden, /tmp its own,
# its home the agent's, the agent services' memory and their own home hidden, the agent's own profile (its coding
# tools, `torq agent tools`) on its PATH, TorqOS linked into those tools first, as the agent. Nothing the person asked for runs as root; the agent
# has no way into the home. `--hold-frozen` holds the agent still while torqcli shares a directory (below).
set -euo pipefail
if [ "$(id -u)" != 0 ]; then
  echo "torq-agent-run: is started by torq agent run (through sudo)" >&2
  exit 2
fi
# Whose agent (ADR-051 3): the caller's own, torq-agent-<the person who ran sudo>, and no other — never
# one the caller names. Who ran sudo is SUDO_UID, which sudo sets: the caller can set it only where
# its sudo rule allows SETENV (sudo then takes `sudo SUDO_UID=… cmd` as given — tried, 2026-11-10), and
# modules/agent.nix asserts the launcher's rule never does (and resets the environment). /proc/self/loginuid
# is not used: it is unset when a person is reached by su or runuser from a root shell, and stays the
# first person's after `su - <another>`. The person's name is the account of that uid, as the machine
# says; their agent must be the account the broker's map (/etc/torq/broker.json, written by
# modules/agent.nix) names as theirs, and a system account, not a person's.
case "${SUDO_UID-}" in
  '' | *[!0-9]*) echo "torq-agent-run: is started by a person, through sudo (no SUDO_UID)" >&2; exit 2 ;;
esac
if [ "$SUDO_UID" -lt 1000 ] || ! person=$(getent passwd -- "$SUDO_UID" | cut -d: -f1) || [ -z "$person" ]; then
  echo "torq-agent-run: uid $SUDO_UID, who ran sudo, is not a person of this machine" >&2
  exit 2
fi
case "$person" in
  [!a-z_]* | *[!a-z0-9_-]*) echo "torq-agent-run: $person is not a person's name TorqOS acts for" >&2; exit 2 ;;
esac
agent=torq-agent-$person
if ! jq -e --arg a "$agent" --arg p "$person" '.agent.agents[$a] == $p' /etc/torq/broker.json >/dev/null 2>&1; then
  echo "torq agent run: $person has no agent account on this machine (torq agent people names who has one; adding you is a system change, an administrator's)" >&2
  exit 2
fi
if ! entry=$(getent passwd -- "$agent"); then
  echo "torq agent run: $agent is not an account on this machine yet" >&2
  exit 2
fi
uid=$(printf '%s\n' "$entry" | cut -d: -f3)
home=$(printf '%s\n' "$entry" | cut -d: -f6)
# A system account (NixOS gives people 1000 and up; never root): an agent is never a person.
if [ -z "$uid" ] || [ "$uid" -eq 0 ] || [ "$uid" -ge 1000 ] || [ "$home" != "/var/lib/$agent" ]; then
  echo "torq agent run: $agent is not an agent account (uid $uid, home $home)" >&2
  exit 2
fi
# `--hold-frozen` (torq agent run --share, ADR-051 3 step 2): the agent held still while torqcli, as the
# person, walks the directory being shared and sets its ACLs — a program of the agent's running then could
# swap a file for a link between the walk's look and its ACL. The person's agent service is frozen
# (systemd's freezer: each of its processes stopped where it is, the task it runs included), and any
# other process of the agent's — another `torq agent run` — refuses the share, as one session at a time
# always did. A service that starts during the walk runs with every home hidden (modules/agent.nix): it
# cannot reach the directory. `frozen` said, it waits for its input to end (torqcli done with the walk,
# or gone however it went), then thaws — on every way out.
if [ "${1-}" = --hold-frozen ]; then
  unit="torq-agent@$person.service"
  frozen="" cgroup=""
  # Thawed on every way out: tried three times; failing that, stopped — it is started by its socket,
  # so it comes back at the person's next ask, never left frozen for good. The exit code stays the
  # first thing that went wrong. (Run by the trap below.)
  # shellcheck disable=SC2329
  thaw() {
    rc=$?
    [ -n "$frozen" ] || exit "$rc"
    for _ in 1 2 3; do
      if systemctl thaw -- "$unit"; then
        exit "$rc"
      fi
      sleep 1
    done
    echo "torq agent run: could not thaw $unit: stopping it (it starts again when next asked)" >&2
    systemctl stop -- "$unit" || echo "torq agent run: could not stop $unit either: systemctl stop $unit" >&2
    [ "$rc" -ne 0 ] || rc=1
    exit "$rc"
  }
  trap thaw EXIT
  trap 'exit 130' INT TERM HUP PIPE
  case "$(systemctl show -P ActiveState -- "$unit" 2>/dev/null || true)" in
    '' | inactive | failed) ;;
    *)
      frozen=1
      if ! systemctl freeze -- "$unit" || [ "$(systemctl show -P FreezerState -- "$unit")" != frozen ]; then
        echo "torq agent run: could not hold your agent ($unit) still: nothing shared" >&2
        exit 2
      fi
      cgroup=$(systemctl show -P ControlGroup -- "$unit")
      ;;
  esac
  # A process is the agent's if any of its uids (real, effective, saved, filesystem: the `Uid:` line of
  # its status) is the agent's — not by who owns its /proc entries: a process that makes itself
  # undumpable has them shown as root's.
  for p in /proc/[0-9]*; do
    uids=$(sed -n 's/^Uid:[[:space:]]*//p' -- "$p/status" 2>/dev/null) || continue
    mine=""
    for u in $uids; do
      [ "$u" != "$uid" ] || mine=1
    done
    [ -n "$mine" ] || continue
    cg=$(sed -n 's/^0:://p' -- "$p/cgroup" 2>/dev/null) || continue
    [ -n "$cg" ] || continue
    if [ -n "$cgroup" ] && { [ "$cg" = "$cgroup" ] || [ "${cg#"$cgroup"/}" != "$cg" ]; }; then
      continue
    fi
    echo "torq agent run: your agent ($agent) is running already (process ${p#/proc/}): one session at a time, and none while a directory is shared" >&2
    exit 2
  done
  echo frozen
  cat >/dev/null || true
  exit 0
fi
share="" fd=""
if [ "${1-}" = --share-fd ]; then
  fd=${2-} share=${3-}
  shift 3
fi
if [ "${1-}" = -- ]; then
  shift
fi
if [ $# -eq 0 ]; then
  set -- bash
fi
if [ -n "$share" ]; then
  person_uid=${SUDO_UID:?torq-agent-run: shares only for the person who ran sudo}
  case "$fd" in
    '' | *[!0-9]*) echo "torq-agent-run: --share-fd needs a descriptor number" >&2; exit 2 ;;
  esac
  link=/proc/self/fd/$fd
  # The open directory itself: a directory, the person's, and where it says it is.
  if [ ! -d "$link" ] || [ "$(stat -L -c %u "$link")" != "$person_uid" ] || [ "$(readlink "$link")" != "$share" ]; then
    echo "torq-agent-run: descriptor $fd is not $share, a directory of yours" >&2
    exit 2
  fi
  case "$share" in
    /home/*/*) ;;
    *) echo "torq-agent-run: shares a directory inside a home, not $share" >&2; exit 2 ;;
  esac
fi
# (The inner script is single-quoted on purpose: its variables are its own arguments.)
# shellcheck disable=SC2016
# The shared directory bound, as root, and checked to be the one that was opened (a descriptor from
# another mount namespace cannot itself be bound); every home hidden around it; then the agent.
exec unshare --mount --propagation private -- bash -euo pipefail -c '
  share=$1 fd=$2 agent=$3 home=$4
  shift 4
  if [ -n "$share" ]; then
    # Bound by its name, on a staging point only root reaches; then checked to be the very directory
    # that was opened (device and inode): if a link on the way led elsewhere, nothing runs.
    stage=/run/torq-agent-stage
    mount --bind -- "$share" "$stage"
    # No set-id program runs from it, and no device node works, while the agent is in it. That holds in
    # the namespace of the agent only: the view the person has of the folder has no such mount, so what it
    # leaves set-id is made plain when it ends (below; the audit of M3, L2).
    mount -o remount,bind,nosuid,nodev -- "$stage"
    if [ "$(stat -c %d:%i "$stage")" != "$(stat -L -c %d:%i "/proc/self/fd/$fd")" ]; then
      echo "torq-agent-run: $share is not the directory that was opened (a link changed?): nothing run" >&2
      exit 2
    fi
    eval "exec $fd<&-"
  fi
  mount -t tmpfs -o mode=0755,nosuid,nodev torq-agent-homes /home
  if [ -n "$share" ]; then
    mkdir -p -- "$share"
    mount --move "$stage" "$share"
  fi
  # The memory of the agent services (their conversations, /var/lib/torq-agent-memory/<person>): written by
  # each service alone, hidden from every program run here (ADR-051 3, step 2) — made first, as root,
  # so that it is there to hide.
  mkdir -p -m 0755 /var/lib/torq-agent-memory
  # The credentials systemd gives units (the resident agent gets the model key as one): hidden as well, made
  # first so that it is there to hide.
  mkdir -p -m 0755 /run/credentials
  # The sandbox, as the agent: every program the launcher runs as the agent runs in it (the link below too).
  sandbox=(setpriv --reuid "$agent" --regid "$agent" --init-groups --no-new-privs --inh-caps=-all --bounding-set=-all --
    bwrap --die-with-parent --unshare-pid --dev-bind / / --proc /proc
      --tmpfs /run/user --tmpfs /tmp --tmpfs /root --tmpfs /var/lib/torq-agent-memory --tmpfs /run/credentials
      --clearenv
      --setenv HOME "$home" --setenv USER "$agent" --setenv LOGNAME "$agent"
      --setenv SHELL /run/current-system/sw/bin/bash
      --setenv PATH "/run/wrappers/bin:/etc/profiles/per-user/$agent/bin:/run/current-system/sw/bin"
      --setenv TERM "${TERM:-dumb}")
  # TorqOS linked into the coding tools the owner chose (torq agent tools; ADR-051 5 and 7): its MCP server and
  # skill in the settings of Claude Code, in the home of the agent — done as the agent in its sandbox (the
  # memory of the services and the credentials hidden), never as root, so nothing is written there with
  # privilege and no link there is followed but as the agent. Advice only: it failing leaves the tool without
  # the tools of TorqOS, and the tool runs all the same. (No apostrophe in this script: it is one
  # single-quoted word.)
  if jq -e ".tools | index(\"claude-code\")" /etc/torq/agent-tools.json >/dev/null 2>&1 && [ -x /run/current-system/sw/bin/torq-mcp ]; then
    "${sandbox[@]}" --chdir "$home" -- /run/current-system/sw/bin/torq-mcp --link </dev/null \
      || echo "torq agent run: TorqOS is not linked into the settings of Claude Code this time (it runs without them)" >&2
  fi
  rc=0
  "${sandbox[@]}" --chdir "${share:-$home}" -- "$@" || rc=$?
  # A set-id file the agent left in the folder would run as the agent outside the sandbox, later: its bits
  # are taken off. As the agent, not as root: the files are owned by the agent, and an owner may change
  # them. The folder belongs to the person, who could swap a folder in it for a link while this runs,
  # after find looks at a file and before chmod, which goes by the path again. As root, chmod would then
  # take the bits off whatever the link led to (the wrapper of sudo, say). As the agent, a link can lead
  # it only to what the agent could change anyway (the review of 2026-11-11, KNOWN-GAPS 6zz).
  if [ -n "$share" ]; then
    found=$(setpriv --reuid "$agent" --regid "$agent" --init-groups --no-new-privs \
      --inh-caps=-all --bounding-set=-all -- \
      find "$share" -xdev -user "$agent" -perm /6000 -type f -print -exec chmod ug-s {} + 2>/dev/null || true)
    [ -z "$found" ] || echo "torq agent run: set-id bits taken off what the agent left: $found" >&2
  fi
  exit "$rc"
' torq-agent-run "$share" "$fd" "$agent" "$home" "$@"

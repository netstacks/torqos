# torq-agent-run [--share-fd <n> <dir>] [--] [<command> …] — as the agent (`torq agent run` starts
# it, through sudo -u torq-agent). The program sees the machine as the agent may: every home hidden,
# the people's runtime directories (their sockets: ssh-agent, D-Bus) hidden, /tmp its own, and only
# the one directory shared with it, read-write. Its home is the agent's; it reaches the system through
# the broker's socket, as torqcli does — it proposes, and a person applies.
if [ "$(id -un)" != torq-agent ]; then
  echo "torq-agent-run: runs as the agent (start it with: torq agent run)" >&2
  exit 2
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
args=(
  --die-with-parent --dev-bind / /
  --tmpfs /home --tmpfs /run/user --tmpfs /tmp --tmpfs /root
  --clearenv
  --setenv HOME /var/lib/torq-agent --setenv USER torq-agent --setenv LOGNAME torq-agent
  --setenv SHELL /run/current-system/sw/bin/bash
  --setenv PATH /run/wrappers/bin:/run/current-system/sw/bin
  --setenv TERM "${TERM:-dumb}"
)
if [ -n "${LANG-}" ]; then
  args+=(--setenv LANG "$LANG")
fi
if [ -n "$share" ]; then
  # The directory comes open: torqcli opened it, as the person, and checked it is theirs. The agent
  # cannot reach it by its name, and needs no way into the home it is in; it appears where it is.
  case "$share" in
    /home/*/*) ;;
    *) echo "torq-agent-run: shares a directory inside a home, not $share" >&2; exit 2 ;;
  esac
  case "$fd" in
    '' | *[!0-9]*) echo "torq-agent-run: --share-fd needs a descriptor number" >&2; exit 2 ;;
  esac
  if [ ! -d "/proc/self/fd/$fd" ]; then
    echo "torq-agent-run: descriptor $fd is not an open directory" >&2
    exit 2
  fi
  args+=(--bind-fd "$fd" "$share" --chdir "$share")
else
  args+=(--chdir /var/lib/torq-agent)
fi
exec bwrap "${args[@]}" -- "$@"

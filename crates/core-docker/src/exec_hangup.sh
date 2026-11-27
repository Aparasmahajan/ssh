# Hang up what an OpsPilot exec session left running in a container.
#
# Run as: sh -c "$SCRIPT" sh <token>
#
# Closing a local terminal hangs up its session: the kernel sends SIGHUP to the
# foreground job and the shell passes it on to the rest. Closing a Docker or
# Kubernetes exec does neither. The attach stream ends, and `top` or `tail -f`
# keeps running in the container indefinitely.
#
# The exec wrapper exports OPSPILOT_SESSION=<token>.<its own pid> before it
# execs the shell, and everything the shell runs inherits it. A TTY exec makes
# that shell a session leader, so its pid is also its session id. This reads
# the pid back from any process still carrying the token, then sends SIGHUP
# to every process in that one session, which is what the terminal hangup
# would have done. Like a local terminal:
#   - `nohup` survives (it ignores SIGHUP);
#   - a daemon that called setsid survives (it carries the token, but it is
#     in a different session);
#   - a sudo'd command that dropped the variable is still caught by session.
# Never touched: this script's own session, and PID 1's, so a shell that
# was not given its own session cannot take the container's main process
# down with it.
#
# POSIX sh only: dash and busybox ash, tested by core-docker's unit tests.
# Needs `tr` and `cat`, which every image that has a shell also has.
# Prints the number of processes signalled.
#
# Shared verbatim with core-kube (include_str!), so there is one copy.

[ -n "$1" ] || exit 2
want="OPSPILOT_SESSION=$1"
nl='
'

# Session id of /proc/<pid> ($1). The comm field can contain spaces and
# parentheses, so cut at the LAST ") ". The fields after it are:
# state ppid pgrp session.
sid_of() {
    s=$(cat "$1/stat" 2>/dev/null) || return 1
    s=${s##*') '}
    set -- $s
    [ -n "$4" ] || return 1
    echo "$4"
}

self_sid=$(sid_of "/proc/$$")
init_sid=$(sid_of /proc/1)

# The session to hang up: the pid recorded by the wrapper. Any carrier will
# do, since they all inherited the same value.
target=
for d in /proc/[0-9]*; do
    e=$(tr '\0' '\n' 2>/dev/null < "$d/environ") || continue
    case "$nl$e" in
        *"$nl$want."*)
            v=${e#*"$want."}
            v=${v%%"$nl"*}
            case "$v" in '' | *[!0-9]*) continue ;; esac
            target=$v
            break
            ;;
    esac
done

n=0
case "$target" in
    '' | 0 | 1 | "$self_sid" | "$init_sid") ;;
    *)
        for d in /proc/[0-9]*; do
            s=$(sid_of "$d") || continue
            if [ "$s" = "$target" ]; then
                kill -HUP "${d#/proc/}" 2>/dev/null && n=$((n + 1))
            fi
        done
        ;;
esac
echo "$n"

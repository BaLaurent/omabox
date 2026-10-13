#!/bin/bash
# What starts a `run -d` job in the box and keeps its exit status (#215). Started by `run -d` as a
# session of its own (setsid -f), so nothing of the caller's ends it: it starts the command as a
# session of its own too (the job: its pid is its session's, as `run -d --replace` finds it), says
# that pid on fd 3, and waits for it to write its exit status (bash's: 128+N for signal N) to
# STATUS_FILE. The command's words come NUL-separated on fd 4, never as arguments here: a `pgrep -f`
# of the app's name in the box finds the app, not this.
# run-job.sh STATUS_FILE
set -u
mapfile -d '' -u 4 argv
exec 4<&-
[ ${#argv[@]} -gt 0 ] || exit 125
# A background job of a shell without job control ignores SIGINT and SIGQUIT: the job gets them back.
(trap - INT QUIT; exec 3>&-; exec setsid -- "${argv[@]}") &
pid=$!
echo "$pid" >&3
exec 3>&-
wait "$pid"
echo "$?" > "$1"

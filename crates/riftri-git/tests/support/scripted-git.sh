#!/bin/sh
# Stand-in for Git, reached through a `git` symlink in a scratch directory.
# Invocation N replays `<directory>/N.out` and ends as `<directory>/N.end`
# says (an exit code, or `signal-<number>`). Invocations with no such files
# run the real Git, so a test can corrupt one step of a multi-step operation.
# A scripted invocation reads none of its input unless `<directory>/N.drain`
# exists, in which case it reads all of it before replying.
directory=$(dirname "$0")
count=0
[ -f "$directory/count" ] && read -r count < "$directory/count"
echo $((count + 1)) > "$directory/count"
if [ ! -f "$directory/$count.end" ]; then
    exec git "$@"
fi
read -r end < "$directory/$count.end"
[ -f "$directory/$count.drain" ] && cat > /dev/null
cat "$directory/$count.out"
case $end in
    signal-*) kill -"${end#signal-}" $$ ;;
esac
exit "$end"

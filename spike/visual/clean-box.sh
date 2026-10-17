#!/usr/bin/env bash
# spike/visual/clean-box.sh NAME [UP OPTIONS...]
#
# A clean box for visual checks (#252), by hand from what omabox has today: the stock bar on one
# theme, a solid sentinel wallpaper, the clock frozen by a literal format, no pointer, no
# network, animations off. UP OPTIONS go to `omabox up` (a --plugin under test, say). The test
# subject's own setup (its server, its config) is the caller's, after this.
set -euo pipefail

name=$1; shift
SENTINEL='#ff00ff'
CLOCK="'Friday 12:00'"

omabox up "$name" --theme tokyo-night --stock-bar --no-pointer --net isolated --size 1920x1080 \
    --idle 0 "$@" >/dev/null
home="$(omabox path -b "$name")/home"

# The wallpaper: one colour no theme uses, so any of it inside a window or the bar is a hole.
mkdir -p "$home/.config/omarchy/backgrounds"
magick -size 1920x1080 "xc:$SENTINEL" "$home/.config/omarchy/backgrounds/sentinel.png"
ln -sfn "/home/sbx/.config/omarchy/backgrounds/sentinel.png" "$home/.local/state/omarchy/current/background"

# The clock: a literal format keeps the widget, its place and a realistic width, and never ticks.
shell="$home/.config/omarchy/shell.json"
jq --arg f "$CLOCK" '(.bar.layout[][] | select(.id == "omarchy.clock")) |= (.format = $f)' "$shell" \
    >"$shell.new" && mv "$shell.new" "$shell"

omabox lua -b "$name" 'hl.config({ animations = { enabled = false } })' >/dev/null
omabox restart-shell -b "$name" >/dev/null
echo "$name: clean box up ($home)"

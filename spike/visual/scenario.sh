#!/usr/bin/env bash
# spike/visual/scenario.sh NAME OUT APP_CLASS APP_CMD WIDGET_ID
#
# One run of the test scenario in a clean box (clean-box.sh, then the subject's own setup): four
# checkpoints, each taken twice a few seconds apart (a, b: what changes when nothing does,
# within one run), with the app's accessibility tree where it has one:
#   desk   the bar with the widget, before the app starts
#   app    the app's window
#   popup  the widget's popup (summoned), over the app
#   help   the app's shortcuts sheet (its `?` key)
# OUT/<checkpoint>-{a,b}.png, OUT/<checkpoint>-{a,b}.tree.
set -euo pipefail

name=$1 out=$2 app=$3 cmd=$4 widget=$5
mkdir -p "$out"

settle() { omabox wait -b "$name" --timeout 15s still --quiet 500ms >/dev/null; }

# checkpoint ID [--window SEL]: two shots (and trees, with --window), 3 s apart.
checkpoint() {
    local id=$1; shift
    local k
    for k in a b; do
        settle
        omabox shot -b "$name" "$@" -o "$out/$id-$k.png" >/dev/null 2>&1
        if [ $# -gt 0 ]; then omabox tree -b "$name" "$@" >"$out/$id-$k.tree" 2>/dev/null || true; fi
        # The delay is the measurement: time passing with nothing done.
        [ $k = b ] || sleep 3
    done
}

checkpoint desk

omabox run -b "$name" -d --wait -- "$cmd" >/dev/null 2>&1
omabox wait -b "$name" --timeout 20s window "class:$app" >/dev/null
checkpoint app --window "class:$app"

omabox run -b "$name" -- omarchy-shell shell summon "$widget" >/dev/null
checkpoint popup

omabox run -b "$name" -- omarchy-shell shell hide "$widget" >/dev/null
omabox keys -b "$name" --window "class:$app" --wait -t '?' >/dev/null
checkpoint help --window "class:$app"
echo "$name: $out"

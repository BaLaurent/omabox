#!/usr/bin/env bash
# spike/visual/compare.sh [-t PERCENT] RUN_DIR...
#
# For each checkpoint: within a run (a against b) and across runs (each run's a against the
# first run's a), the pixels that differ by more than PERCENT of full scale in grey (0: any
# difference), the regions they make (12 px apart counts as one), and the tree lines that differ.
set -euo pipefail

t=0
if [ "${1:-}" = -t ]; then t=$2; shift 2; fi
first=$1

# The difference image of two shots, grey, thresholded: white where they differ.
mask() { magick "$1" "$2" -alpha off -compose difference -composite -colorspace gray -threshold "$t%" "${@:3}"; }
pixels() { mask "$1" "$2" -format '%[fx:round(mean*w*h)]' info:; }
regions() {
    mask "$1" "$2" -morphology Dilate Square:12 -define connected-components:verbose=true \
        -connected-components 8 null: 2>&1 | awk 'NR>1 && $NF ~ /255|white/ {print $2}' | tr '\n' ' '
}
treediff() {
    if [ -f "$1" ] && [ -f "$2" ]; then diff "$1" "$2" | grep -c '^[<>]' || true; else echo -; fi
}
row() {
    printf '%-7s %-12s %8s %5s  %s\n' "$1" "$2" "$(pixels "$3" "$4")" "$(treediff "${3%.png}.tree" "${4%.png}.tree")" \
        "$(regions "$3" "$4")"
}

printf '%-7s %-12s %8s %5s  %s\n' checkpoint pair pixels tree regions
for shot in "$first"/*-a.png; do
    id=$(basename "$shot" -a.png)
    for run in "$@"; do
        [ -f "$run/$id-b.png" ] && row "$id" "$(basename "$run") a/b" "$run/$id-a.png" "$run/$id-b.png"
    done
    for run in "${@:2}"; do
        [ -f "$run/$id-a.png" ] && row "$id" "$(basename "$first")/$(basename "$run")" "$shot" "$run/$id-a.png"
    done
done

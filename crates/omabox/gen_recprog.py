#!/usr/bin/python3
# recprog.rs from bin/omabox (#324 wave 4b): record's inline programs, byte for byte, each the text
# between its quotes there. gen_recprog.py ROOT [OUT | --list] (--list: each program and a NUL, as
# `omabox _rust-jq --recprog` prints them; t_unit_rust_parity compares the two). Run rustfmt after.
import re, sys
root = sys.argv[1]
s = open(root + '/bin/omabox').read()
a = s.index('\ncmd_record() {'); b = s.index('\ncmd_shot() {')
rec = s[a:b]
out = []
def between(name, start, end, src=rec, nth=0):
    i = -1
    for _ in range(nth + 1):
        i = src.index(start, i + 1)
    j = src.index(end, i + len(start))
    txt = src[i + len(start):j]
    out.append((name, txt))
    return txt
# single-quoted programs: start marker ends with the opening quote, end marker begins with the closing one
between('RUNNING_SH', "box_sys /usr/bin/sh -c '", "' omabox-record ${REC_PID//,/ }")
between('START_SH', "mapfile -t pids < <(BOX_KEEP=3 box_sys /usr/bin/bash -c '", "' omabox-record \"${pairs[@]}\"")
i = rec.index("box_sys /usr/bin/sh -c 'for i in 1 2 3 4 5;")
out.append(('ALIVE_SH', rec[i+len("box_sys /usr/bin/sh -c '"):rec.index("' omabox-record \"${pids[@]}\"", i)]))
i = rec.index("$(box_sys /usr/bin/sh -c 'for d; do timeout 2 tail")
out.append(('TAILS_SH', rec[i+len("$(box_sys /usr/bin/sh -c '"):rec.index("' omabox-record \"${dirs[@]}\"", i)]))
between('AUDIO_SH', "apid=$(BOX_KEEP=3 box_sys /usr/bin/bash -c '", "' omabox-record \"$REC_DIR\" \"$AUDIO_SINK\"")
between('AUDIO_WAIT_SH', "t=$(box_sys /usr/bin/sh -c '", "' omabox-record \"$REC_DIR/audio.log\" \"$apid\"")
between('AUDIO_END_SH', "box_sys /usr/bin/sh -c 'p=$1;", "' omabox-record \"$1\" 2>/dev/null || true")
out[-1] = ('AUDIO_END_SH', 'p=$1;' + out[-1][1])
between('AUDIO_START_SED', "box_sys /usr/bin/sed -n '", "' \"$REC_DIR/audio.log\"")
i = rec.index("box_sys /usr/bin/sh -c 'for d; do l=$(head -c 200")
out.append(('FIRSTS_SH', rec[i+len("box_sys /usr/bin/sh -c '"):rec.index("' omabox-record \"${ds[@]}\"", i)]))
between('HOLD_SH', "/usr/bin/timeout -k 1 \"$left\" /usr/bin/bash -c '", "' omabox-record $((end + 5000000))")
between('STILL_SH', "local still='", "'\n")
i = rec.index("box_sys /usr/bin/sh -c 'mkdir -p \"${1%/*}\" && cat > \"$1\"'")
out.append(('WRITE_SH', 'mkdir -p "${1%/*}" && cat > "$1"'))
out.append(('SHEETDIR_SH', 'rm -rf "$1" && mkdir -p "$1"'))
assert 'rm -rf "$1" && mkdir -p "$1"' in rec
# awk and jq
between('BYTES_H_AWK', "bytes_h() { awk -v b=\"$1\" '", "'; }")
between('LAYOUT_JQ', "jq -r \"$JQ_MON\"'", "'\n}", src=rec[rec.index('record_layout() {'):])
between('GEO_MIN_JQ', "geo=$(jq -r \"$JQ_MON\"'", "' <<<\"$mons\")")
between('GEO_K_AWK', "head -n 1 <<<\"$layout\" | awk '", "')\"")
between('GEO_MAIN_AWK', "awk -v m=\"$mainn\" '", "')\"")
between('GEO_ONE_JQ', "geo=$(jq -r --arg n \"$mon\" '", "' <<<\"$mons\"")
between('STATUS_JQ', "--arg t \"${REC_START:-}\" \\\n    '", "'\n}")
between('MARKS_AWK', "awk -F '\\t' -v t0=\"$first\" -v cuts=\"$cuts\" -v fps=\"${REC_FPS:-30}\" '", "'\n  } > \"$tsv.part\"")
between('VTT_AWK', "awk -F '\\t' -v dur=\"$dur\" '", "' \"$tsv\" > \"$vtt.part\"")
between('CUT_INPUT_AWK', "'$1 ~ /^[0-9]{1,17}$/ && $2 ~", "' \"$input\"")
out[-1] = ('CUT_INPUT_AWK', '$1 ~ /^[0-9]{1,17}$/ && $2 ~' + out[-1][1])
between('CUT_MARKS_AWK', "awk -F '\\t' -v f=\"$first\" '", "' <<<\"$marks\"; } )")
between('CUT_AWK', "awk -v fps=\"$REC_FPS\" -v nb=\"$nb\" -v cut=\"$cut\" '", "' | sort -n -u)")
between('OVERLAY_AWK', "LC_ALL=C.UTF-8 awk -v first=\"$3\" -v x0=\"$4\" -v y0=\"$5\" -v k=\"$6\" -v W=\"$7\" -v H=\"$8\" -v dur=\"$9\" -v area=\"$area\" '", "'\n}")
between('SHEET_JQ', "read -r nb num den vw vh < <(jq -r '", "' <<<\"$j\"")
if sys.argv[2:3] == ['--list']:
    for _, txt in out:
        sys.stdout.write(txt + '\0')
    sys.exit(0)
with open(sys.argv[2] if len(sys.argv) > 2 else root + '/crates/omabox/src/recprog.rs', 'w') as f:
    f.write("//! record's programs that run as bin/omabox has them inline (#324 wave 4b): the shell its box runs,\n")
    f.write("//! awk's and jq's, byte for byte (t_unit_rust_parity's check of `_rust-jq --inline` holds them\n")
    f.write("//! to bin/omabox). Made from bin/omabox, not by hand.\n\n")
    for name, txt in out:
        h = '#' * 3
        while ('"' + h) in txt: h += '#'
        f.write(f"pub const {name}: &str = r{h}\"{txt}\"{h};\n")
    f.write("\n/// All of them, for `_rust-jq --inline`.\npub const ALL: &[&str] = &[\n")
    for name, _ in out:
        f.write(f"    {name},\n")
    f.write("];\n")


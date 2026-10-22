//! The jq programs bin/omabox keeps in variables, byte for byte (t_unit_rust_parity compares each
//! with bash's): the desktop's windows, layers and changes are jq's answers, so jq runs them.

macro_rules! layer_span_jq { () => { r#"
  def spans($m; $mon): (first($m[]? | select(.name == $mon)) // null) as $o
    | if $o == null or ($o.width // null) == null then false else
        (if ($o.transform // 0) % 2 == 1 then [$o.height, $o.width] else [$o.width, $o.height] end
         | map(. / ($o.scale // 1))) as [$mw, $mh]
        | ($o.reserved // null) as $r
        | if .x <= $o.x and .x + .w >= $o.x + $mw - 1 and .y <= $o.y and .y + .h >= $o.y + $mh - 1 then true
          elif $r == null then false
          else
          (.x <= $o.x + $r[0] and .x + .w >= $o.x + $mw - $r[2] - 1) as $fw
          | (.y <= $o.y + $r[1] and .y + .h >= $o.y + $mh - $r[3] - 1) as $fh
          | if $fh then (if .x <= $o.x then $r[0] == 0 elif .x + .w >= $o.x + $mw - 1 then $r[2] == 0 else true end)
            elif $fw then (if .y <= $o.y then $r[1] == 0 elif .y + .h >= $o.y + $mh - 1 then $r[3] == 0 else true end)
            else false end end end;"# }; }
macro_rules! win_sel_jq { () => { r#"
  def m($s; $loose):
    if $s == "active" then .active
    elif ($s | test("^(address:)?0x[0-9a-fA-F]+$")) then .address == ($s | sub("^address:"; "") | ascii_downcase)
    elif ($s | test("^pid:[0-9]+$")) then .pid == ($s[4:] | tonumber)
    elif ($s | test("^(class|title|initialClass|initialTitle):")) then
      ($s | capture("^(?<k>[A-Za-z]+):(?<v>.*)$")) as $kv | (.[$kv.k] // "") | test($kv.v; "i")
    else (.class | ascii_downcase) as $c | ($s | ascii_downcase) as $w
      | $c == $w or ($c | split(".") | last) == $w or (.title | ascii_downcase | contains($w))
        or ($loose and any($c | split("-")[]; . == $w)) end;
  def hits($loose): [.[] | select(. as $w | all($ARGS.positional[]; . as $s | $w | m($s; $loose)))];
  hits(false) as $h | if $h | length > 0 then $h else hits(true) end"# }; }
macro_rules! win_desc_jq { () => { r#"def n: "\(.address) \(.class) \(.title | tojson)";
  def d: "\(.address) \(.class) \(.title | tojson) at \(.at[0]),\(.at[1]) \(.size[0])x\(.size[1])" + (if .active then ", focused" else "" end);"# }; }
macro_rules! desk_facts_jq { () => { r#"
  def cut: if length > 40 then .[:39] + "…" else . end;
  def win: {address, class, title, workspace: (.workspace.name // null)};
  def wins: [.clients[]? | select(.mapped)];
  def lays: [.layers | objects | .[] | .levels? // {} | .[] | .[]
    | select(.w > 1 and .h > 1) | {ns: (if (.namespace // "") == "" then .address else .namespace end), address, at: {x, y, w, h}}];
  def shown: [.monitors[]? | .activeWorkspace.id, .specialWorkspace.id] | map(select(. != 0 and . != null));
  ($b | wins) as $bw | ($a | wins) as $aw | ($bw + $aw) as $all
  | def name: if (.class // "") == "" then "a window \"\(.title // "" | cut)\"" else "\(.class) \"\(.title | cut)\"" end
      + (. as $w | if any($all[]; .address != $w.address and .class == $w.class and .title == $w.title) then " (\(.address))" else "" end);
    def cname: (if (.class // "") == "" then "a window" else .class end)
      + (. as $w | if any($all[]; .address != $w.address and .class == $w.class) then " (\(.address))" else "" end);
  ($a.active.address // null) as $fa | ($b.active.address // null) as $fb
  | ($a | shown) as $ashown
  | [$aw[] | . as $w | select(all($bw[]; .address != $w.address))] as $opened
  | [ ($opened[] | (.address == $fa) as $f
        | {kind: "window_opened", window: win, focused: $f, rank: 1,
           text: ("window opened" + (. as $w | if any($ashown[]; . == $w.workspace.id) then "" else " on workspace \($w.workspace.name)" end)
             + (if $f then ", focused" else "" end) + ": \(name)")}),
      ($bw[] | . as $w | select(all($aw[]; .address != $w.address))
        | {kind: "window_closed", window: win, rank: 2, text: "window closed: \(name)"}),
      (($b | lays) as $bl | ($a | lays) as $al
        | ([$bl[].ns, $al[].ns] | unique[]) as $n
        | ([$bl[] | select(.ns == $n)] | length) as $nb | [$al[] | select(.ns == $n)] as $an
        | if ($an | length) > $nb then
            {kind: "layer_opened", namespace: $n,
             at: (first($an[] | . as $l | select(all($bl[]; .address != $l.address))) // $an[-1]).at,
             rank: 3, text: "layer \($n) opened"}
          elif ($an | length) < $nb then {kind: "layer_closed", namespace: $n, rank: 4, text: "layer \($n) closed"}
          else empty end),
      (if $fa != $fb then
         {kind: "focus", from: (if $fb then $b.active | win else null end), to: (if $fa then $a.active | win else null end),
          rank: (if $fa != null and any($opened[]; .address == $fa) then null else 5 end),
          text: ("focus: " + (if $fa then $a.active | name else "none" end))}
       else empty end),
      ($aw[] | . as $w | first($bw[] | select(.address == $w.address)) as $o
        | (if $o.title != $w.title then
             {kind: "title", window: ($w | win), from: $o.title, to: $w.title,
              rank: (if $w.address == $fa then 6 else 10 end), text: "\($w | cname) title now \($w.title | cut | tojson)"}
           else empty end),
          (if ($o.workspace.name // null) != ($w.workspace.name // null) then
             {kind: "window_moved", window: ($w | win), from: ($o.workspace.name // null), to: ($w.workspace.name // null),
              rank: 8, text: "\($w | name) moved to workspace \($w.workspace.name)"}
           else empty end),
          (if $o.floating != $w.floating or (($o.fullscreen // 0) > 0) != (($w.fullscreen // 0) > 0) then
             {kind: "window_state", window: ($w | win), floating: $w.floating, fullscreen: (($w.fullscreen // 0) > 0), rank: 9,
              text: ("\($w | name) " + ([
                (if (($o.fullscreen // 0) > 0) != (($w.fullscreen // 0) > 0) then (if ($w.fullscreen // 0) > 0 then "now fullscreen" else "out of fullscreen" end) else empty end),
                (if $o.floating != $w.floating then (if $w.floating then "now floating" else "now tiled" end) else empty end)] | join(", ")))}
           else empty end)),
      (($b.workspace.name // null) as $wb | ($a.workspace.name // null) as $wa
        | if $wb != $wa then {kind: "workspace", from: $wb, to: $wa, rank: 7, text: "workspace now \($wa)"} else empty end)
    ] as $all_changes
  | ($all_changes | map(select(.rank != null)) | sort_by(.rank) | map(.text)) as $texts
  | (reduce $texts[] as $t ({shown: [], open: true};
       if .open and (.shown | length) < 3 and ((.shown | length) == 0 or ((.shown | join("; ") | length) + 2 + ($t | length)) <= 160)
       then .shown += [$t] else .open = false end)) as $cap
  | (($texts | length) - ($cap.shown | length)) as $more
  | {changes: ($all_changes[:50] | map(del(.rank, .text))),
     line: ($cap.shown | join("; ") + (if $more > 0 then "; +\($more) more (omabox events --since \($since // "0"))" else "" end))}"# }; }
macro_rules! layer_list_jq { () => { r#"[to_entries[] | .key as $m | .value.levels // {} | to_entries[] | (.key | tonumber) as $lv
  | .value[] | {namespace, address, pid, x, y, w, h, monitor: $m,
    level: (["background", "bottom", "top", "overlay"][$lv] // "level \($lv)")}]"# }; }
macro_rules! jq_mon {
    () => {
        r#"def lw: if (.transform // 0) % 2 == 1 then .height / .scale else .width / .scale end;
def lh: if (.transform // 0) % 2 == 1 then .width / .scale else .height / .scale end; "#
    };
}

/// Whether a layer may take input on part of it only (#263, finding 398; #311, finding 421).
pub const LAYER_SPAN_JQ: &str = layer_span_jq!();

/// win_annotate's: the mapped windows, their place, cover and layers (finding 81, #263).
pub const WIN_JQ: &str = concat!(
    layer_span_jq!(),
    r#"
  def overlaps($o): .at[0] < $o.at[0] + $o.size[0] and $o.at[0] < .at[0] + .size[0]
    and .at[1] < $o.at[1] + $o.size[1] and $o.at[1] < .at[1] + .size[1];
  def above($o; $t):
    if $o.workspace.id < 0 and $t.workspace.id > 0 then true
    elif $o.workspace.id != $t.workspace.id then false
    elif ($o.fullscreen > 0) != ($t.fullscreen > 0) then $o.fullscreen > 0
    elif $o.floating != $t.floating then $o.floating
    else $o.z > $t.z end;
  [$l | objects | to_entries[] | .key as $mon | .value | .levels? // {}
   | (.["2"] // [] | map(. + {layer: "top"})), (.["3"] // [] | map(. + {layer: "overlay"}))
   | .[] | select(.w > 1 and .h > 1) | {namespace, pid, layer, at: [.x, .y], size: [.w, .h], certain: (spans($m; $mon) | not)}] as $L
  | ([$m[] | .activeWorkspace.id, .specialWorkspace.id] | map(select(. != 0))) as $shown
  | [$c | map(select(.mapped)) | to_entries[] | .value + {z: .key}
     | .onscreen = (.workspace.id as $w | any($shown[]; . == $w) and (.hidden | not))] as $all
  | [$all[] | . as $t | {address, stableId, class, title, initialClass, initialTitle, pid,
      workspace: .workspace.name, at, size, floating, fullscreen: (.fullscreen > 0), xwayland,
      active: (.address == $a.address), onscreen,
      cover: [$all[] | select(.address != $t.address and .onscreen and above(.; $t) and overlaps($t))
              | {address, class, title, at, size}]}
     + if $l == null then {} else {layers: (if $t.onscreen | not then [] else
         any($all[]; .onscreen and .workspace.id == $t.workspace.id and .fullscreen >= 2) as $fs
         | [$L[] | select((.layer == "overlay" or ($fs | not)) and overlaps($t))] end)} end]"#
);

/// A window selector's matches (#20, #244).
pub const WIN_SEL_JQ: &str = win_sel_jq!();

/// wait window's words for a window: n what it is, d where it is too.
pub const WIN_DESC_JQ: &str = win_desc_jq!();

/// What changed on the desktop between two snapshots (#274).
pub const DESK_FACTS_JQ: &str = desk_facts_jq!();

/// What got the input, from a snapshot (findings 398, 421); `(WIN)` stands for WIN_JQ.
pub const DESK_TARGET_JQ: &str = concat!(
    layer_span_jq!(),
    r#"
  def cut: if length > 40 then .[:39] + "…" else . end;
  def win: {address, class, title, workspace: ((.workspace | objects | .name) // .workspace)};
  def name: (if (.class // "") == "" then "a window" else .class end) + " \"\(.title // "" | cut)\"";
  def target($k; $w; $n; $at; $t): {kind: $k, window: $w, namespace: $n, at: $at, certain: true, text: $t};
  $s.clients as $c | $s.monitors as $m | $s.active as $a | $s.layers as $l
  | if $what == "keys" then
      if $kbl != "" then target("layer"; null; ($kbl | split(", ")[0]); null; "the keys went to layer \($kbl) (it has the keyboard)")
      elif ($a.address // null) then target("window"; ($a | win); null; null; "the keys went to \($a | name) (focused)")
      else target("none"; null; null; null; "the keys went to no window (none focused)") end
    elif $what == "run" then
      (first($c[]? | select(.mapped and .pid == $pid)) // null) as $w
      | if $w then target("window"; ($w | win); null; null; "its window is \($w | name)")
        else target("none"; null; null; null; "no window from pid \($pid) yet") end
    else
      (if $what == "drag" then "the drag from \($x),\($y) started" elif $what == "scroll" then "the scroll at \($x),\($y) was"
       else "the click at \($x),\($y) was" end) as $say
      | def at($o): $x >= $o.x and $x < $o.x + $o.w and $y >= $o.y and $y < $o.y + $o.h;
      [$l | objects | to_entries[] | .key as $mon | .value | .levels? // {} | to_entries[] | (.key | tonumber) as $lv | .value | reverse[]
        | select(.w > 1 and .h > 1 and at(.))
        | {lv: $lv, ns: (if (.namespace // "") == "" then .address else .namespace end), span: ($lv >= 2 and spans($m; $mon))}] as $L
      | ([(WIN) | .[] | select(.onscreen and at({x: .at[0], y: .at[1], w: .size[0], h: .size[1]})
            and all(.cover[]; at({x: .at[0], y: .at[1], w: .size[0], h: .size[1]}) | not))] | first // null) as $w
      | ([$L[] | select(.lv >= 2)] | sort_by(-.lv)) as $tops
      | ($tops | map(select(.span | not)) | first // null) as $top
      | ($tops | map(select(.span)) | first // null) as $maybe
      | if $top then target("layer"; null; $top.ns; null; "\($say) on layer \($top.ns)"
            + (if $w then " (over \($w | name))" else "" end))
        else
          (if $w then target("window"; ($w | win); null; {x: ($x - $w.at[0]), y: ($y - $w.at[1])};
              "\($say) on \($w | name) (its \($x - $w.at[0]),\($y - $w.at[1]))")
           elif ($L | map(select(.lv < 2)) | length) > 0 then
             ($L | map(select(.lv < 2)) | sort_by(-.lv) | first) as $b | target("layer"; null; $b.ns; null; "\($say) on no window (layer \($b.ns))")
           else target("none"; null; null; null; "\($say) on no window or layer") end)
          | if $maybe then .certain = false
              | .text += ", or possibly on layer \($maybe.ns) (it spans the screen: if one of its items, a toast, is there)"
            else . end
        end
    end"#
);

/// The layers of `hyprctl -j layers`, one object each.
pub const LAYER_LIST_JQ: &str = layer_list_jq!();

/// A monitor's logical size, turned with its transform (finding 311).
pub const JQ_MON: &str = jq_mon!();

/// A program by its bash name, for the suite's check that these are bash's.
pub fn by_name(n: &str) -> Option<&'static str> {
    Some(match n {
        "LAYER_SPAN_JQ" => LAYER_SPAN_JQ,
        "WIN_JQ" => WIN_JQ,
        "WIN_SEL_JQ" => WIN_SEL_JQ,
        "WIN_DESC_JQ" => WIN_DESC_JQ,
        "DESK_FACTS_JQ" => DESK_FACTS_JQ,
        "DESK_TARGET_JQ" => DESK_TARGET_JQ,
        "LAYER_LIST_JQ" => LAYER_LIST_JQ,
        "JQ_MON" => JQ_MON,
        _ => return None,
    })
}

/// Inline programs of bin/omabox's functions, byte for byte (the suite checks each is in the
/// script as it is).
/// win_lines: a window a line.
pub const WIN_LINES_JQ: &str = r#"if length == 0 then "  (none)" else .[] | "  \(.address)  \(.class)  \(.title | tojson)  workspace \(.workspace)" end"#;
/// win_desc: `0x... class "title"`.
pub const WIN_DESC_ONE_JQ: &str = r#""\(.address) \(.class) \(.title | tojson)""#;
/// layer_at: where a layer is drawn, a 1x1 one not (finding 146).
pub const LAYER_AT_JQ: &str = r#"first(.. | objects | select(.namespace? == $n and .w > 1 and .h > 1)
    | "\(.x),\(.y) \(.w)x\(.h)") // empty"#;
/// win_answer focused (after WIN_DESC_JQ).
pub const WIN_FOCUSED_JQ: &str = r#"
        (first(.[] | select(.active)) // null) as $f
        | if length == 0 then "no"
          elif $f == null then if length == 1 then "no \(.[0] | n) is there" else "no \(length) matching, none focused" end
          else "yes \($f | d)" + (if length > 1 then " (1 of \(length) matching)" else "" end) end"#;
/// win_answer present (after WIN_DESC_JQ).
pub const WIN_PRESENT_JQ: &str = r#"
        if length == 0 then "no" elif length == 1 then "yes \(.[0] | d)"
        else "yes \(length) windows: \(map(d) | join("; "))" end"#;
/// poll_until --json.
pub const POLL_JSON_JQ: &str = r#"{result: $res, condition: ($cond | sub("_probe$"; "") | sub("^win$"; "window")), message: $msg, elapsed: ($t | tonumber / 1000),
        detail: (if $d == "" then null else $d end)}"#;
/// cursor_now: "X Y".
pub const CURSOR_JQ: &str = r#"select((.x | type) == "number" and (.y | type) == "number") | "\(.x | floor) \(.y | floor)""#;
/// settle_end --json.
pub const SETTLE_JSON_JQ: &str = r#"
      def secs: if . == "" or . == "-" then null else tonumber / 1000 end;
      def box: if . == "-" then null else split(",") | map(tonumber) | {x: .[0], y: .[1], w: .[2], h: .[3]} end;
      {result: $res, condition: $cond, message: $msg, elapsed: ($t | secs), first_change: ($first | secs),
       last_change: ($last | secs), change: ($ch | box), late_changes: ($late | box),
       ignored: (($ig | box) as $b | if $b then $b + {why: $why} else null end),
       frames: (if $frames == "" then null else $frames | tonumber end)}
      + if $facts == "" then {} else {changes: $changes, target: $target, events_since: $evs} end"#;
/// desk_snap: the five answers as one object.
pub const SNAP_JQ: &str = r#"select(length == 5 and (.[0] | type) == "array" and (.[1] | type) == "object"
    and (.[2] | type) == "object" and (.[4] | type) == "array")
    | {events: ($s | tonumber? // null), clients: .[0], layers: .[1], active: .[2], workspace: .[3], monitors: .[4]}"#;
/// windows --json.
pub const WINDOWS_JSON_JQ: &str = r#"def r: . + {x: .at[0], y: .at[1], w: .size[0], h: .size[1]};
      map(r | .cover |= map(r) | if .layers then .layers |= map(r) else . end)"#;
/// windows_table.
pub const WINDOWS_TABLE_JQ: &str = r#"def pad($n): tostring | . + (" " * ($n - length) // "");
    .[] | [(.address | pad(16)), (.pid | pad(8)), (.workspace | pad(10)),
      ((if .onscreen | not then "off-screen" elif .cover != [] then "covered" else "on-screen" end)
        + (if .active then ",active" else "" end) | pad(20)),
      ("\(.size[0])x\(.size[1]) at \(.at[0]),\(.at[1])" | pad(22)), (.class | pad(16)), (.title | tojson)] | join(" ")"#;
/// windows_layers.
pub const WINDOWS_LAYERS_JQ: &str = r#"[.[] | .address as $a | .layers[]? | {k: [.namespace, .layer, .at, .size, (.certain != false)], a: $a}]
    | group_by(.k)[] | .[0].k as [$n, $l, $at, $sz, $c]
    | "layer \($n | tojson) (\($l) \($sz[0])x\($sz[1]) at \($at[0]),\($at[1])) "
      + if $c then "is over \(map(.a) | join(", ")): where it takes input, input there goes to it"
        else "spans the screen, possibly over \(map(.a) | join(", ")): input goes to it only where one of its items (a toast, a dock icon) is, elsewhere to the window" end"#;
/// layer_select: the layers SEL names.
pub const LAYER_HITS_JQ: &str = r#"[.[] | select(
      if ($s | startswith("address:")) then .address == ($s | ltrimstr("address:"))
      elif ($s | startswith("pid:")) then (.pid | tostring) == ($s | ltrimstr("pid:"))
      else .namespace == $s end)]"#;
/// layer_lines.
pub const LAYER_LINES_JQ: &str = r#"if length == 0 then "  (none)" else .[] | "  \(.address)  \(.namespace | tojson)  pid \(.pid)  \(.level) \(.w)x\(.h) at \(.x),\(.y) on \(.monitor)" end"#;
/// layer_desc.
pub const LAYER_DESC_JQ: &str =
    r#""\(.namespace | tojson) (pid \(.pid), \(.w)x\(.h) at \(.x),\(.y))""#;
/// layer_read: its pid and place.
pub const LAYER_FIELDS_JQ: &str = r#""\(.pid) \(.w) \(.h) \(.x) \(.y)""#;
/// layer_read: its pid and size (after LAYER_LIST_JQ).
pub const LAYER_TIED_JQ: &str = r#" | map(select(.pid == $l.pid and .w == $l.w and .h == $l.h))"#;
/// layer_read: the one of them holding the keyboard.
pub const LAYER_KB_JQ: &str = r#"($kb | split(", ")) as $k | [.[] | select(.namespace as $n | .address as $a
      | any($k[]; . == $n or . == $a))] | if length == 1 then .[0].address else "" end"#;
/// tree: its app's other windows of its size (#302).
pub const TREE_TWINS_JQ: &str =
    r#"[.[] | select(.pid == $w.pid and .address != $w.address and .size == $w.size)] | length"#;
/// extent (after JQ_MON).
pub const EXTENT_JQ: &str = r#"[.[] | select(.disabled | not)] |
    "\(map(.x + lw) | max | floor)x\(map(.y + lh) | max | floor)""#;
/// main_screen, interactive.
pub const MAIN_SCREEN_I_JQ: &str =
    r#"map(select((.disabled | not) and (.name | startswith("WAYLAND-")))) | .[0]"#;
/// main_screen, headless.
pub const MAIN_SCREEN_H_JQ: &str =
    r#"map(select((.disabled | not) and (.name == "HEADLESS-2" or .name == "WAYLAND-1"))) | .[0]"#;
/// box_screen (after main_screen).
pub const BOX_SCREEN_JQ: &str =
    r#" // empty | "\(.name) \(.width)x\(.height)@\(.refreshRate * 100 | round / 100)""#;
/// win_scale.
pub const WIN_SCALE_JQ: &str = r#"first(.[] | select(.address == $a) | .monitor) as $id | first($m[] | select(.id == $id) | .scale) // 1"#;
/// A window by its address.
pub const BY_ADDRESS_JQ: &str = r#"first(.[] | select(.address == $a)) // empty"#;
/// pixel: a point on no monitor (#161; after JQ_MON).
pub const PIXEL_OFF_JQ: &str = r#"[.[] | select(.disabled | not)] as $m |
      [range(0; $p | length; 2) | [$p[.], $p[. + 1]] | select(. as [$x, $y] | $x < $lw and $y < $lh and ($m | all(.[];
        $x < .x or $y < .y or $x >= .x + (lw | floor) or $y >= .y + (lh | floor))))] |
      if length == 0 or ($m | length) == 0 then empty
      else "\(.[0][0]),\(.[0][1]) is not on any monitor (\($m | map("\(.name) \(.x),\(.y) \(lw | floor)x\(lh | floor)") | join(", ")))" end"#;
/// wait --window: its place as X,Y,W,H.
pub const WIN_REGION_JQ: &str = r#""\(.at[0]),\(.at[1]),\(.size[0]),\(.size[1])""#;
/// A window's size, WxH.
pub const WIN_SIZE_JQ: &str = r#""\(.size[0])x\(.size[1])""#;
/// pixel: a window's size, W H.
pub const WIN_SIZE2_JQ: &str = r#""\(.size[0]) \(.size[1])""#;

/// layer_select: the ones drawn (over 1x1, finding 146).
pub const LAYER_BIG_JQ: &str = r#"map(select(.w > 1 and .h > 1))"#;

/// win_blocked: why a point of a window cannot be clicked, or nothing.
pub const WIN_BLOCKED_JQ: &str = r#"(.at[0] + $x) as $px | (.at[1] + $y) as $py
    | if .onscreen | not then "not on screen (workspace \(.workspace))"
      else first(.cover[] | select($px >= .at[0] and $px < .at[0] + .size[0] and $py >= .at[1] and $py < .at[1] + .size[1])
        | "covered there by \(.address) \(.class) \(.title | tojson)") // empty end"#;
/// win_layer: the layer surface over a point of a window (#263, findings 398, 421).
pub const WIN_LAYER_JQ: &str = r#"(.at[0] + $x) as $px | (.at[1] + $y) as $py
    | [.layers[]? | select($px >= .at[0] and $px < .at[0] + .size[0] and $py >= .at[1] and $py < .at[1] + .size[1])]
    | select(length > 0) | (map(select(.certain != false)) | last) // last
    | "\(if .certain == false then "possible" else "certain" end) \(.namespace | tojson) \(.layer) \(.size[0])x\(.size[1]) at \(.at[0]),\(.at[1])""#;
/// win_point: the point inside the window.
pub const WIN_INSIDE_JQ: &str = r#"$x < .size[0] and $y < .size[1]"#;
/// win_point: its screen point.
pub const WIN_AT_JQ: &str = r#""\(.at[0] + $x) \(.at[1] + $y)""#;
/// win_raise: the active window's address.
pub const ACTIVE_ADDR_JQ: &str = r#".address // empty"#;

/// zero_note: the outputs listed 0x0 (#213).
pub const ZERO_NOTE_JQ: &str =
    r#"[.[] | select((.disabled | not) and (.width == 0 or .height == 0)) | .name] | join(", ")"#;
/// shot --json (#273, audit J2): what the shot is.
pub const SHOT_SAID_JQ: &str = r#"
    def n: if . == "" then null else tonumber end;
    ($p != "") as $img
    | {path: (if $img then $p else null end), kind: $k,
       region: (if $img then {x: ($x | n), y: ($y | n), w: ($w | n), h: ($h | n)} else null end),
       image: (if $img then {w: ($iw | n), h: ($ih | n)} else null end),
       scale: (if $img then (($iw | n) / ($w | n) * 10000 | round / 10000) else null end),
       window: ($win | if . == null then null else {address, class, title, workspace, x: .at[0], y: .at[1], w: .size[0], h: .size[1]} end),
       monitor: (if $mon == "" then null else $mon end), fit: ($fit | n), zoom: ($zoom | n),
       changed: (if $ch == 1 then {result: $res, since: (if $since == "" then null else $since end),
         why: (if $why == "" then null else $why end)} else null end)}"#;
/// shot --monitor (#122): that monitor's part of the layout (after JQ_MON).
pub const SHOT_MON_JQ: &str = r#"first(.[] | select(.name == $n and (.disabled | not))) |
      "\(.x) \(.y) \(lw | floor) \(lh | floor)""#;
/// shot --monitor: the monitors there are.
pub const MON_NAMES_JQ: &str = r#"map(.name) | join(", ")"#;
/// shot --monitor: the one named, for zero_note.
pub const MON_NAMED_JQ: &str = r#"map(select(.name == $n))"#;
/// shot --fit (#227): the highest scale of the outputs captured (after JQ_MON).
pub const SHOT_SCALE_JQ: &str = r#"[.[] | select((.disabled | not) and .x < $x + $w and $x < .x + lw and .y < $y + $h and $y < .y + lh) | .scale]
         | max // 1"#;
/// shot --window: whether it is on screen, what covers it.
pub const WIN_STATE_JQ: &str = r#"if .onscreen | not then ", not on screen"
      elif .cover != [] then ", covered by \(.cover[0].address) \(.cover[0].class) \(.cover[0].title | tojson)"
        + (if (.cover | length) > 1 then " and \(.cover | length - 1) more" else "" end)
      else "" end"#;
/// shot --window: where it is, X,Y.
pub const WIN_AT2_JQ: &str = r#""\(.at[0]),\(.at[1])""#;
/// calibrate: a full shot's size (finding 299; after JQ_MON).
pub const CALIB_SIZE_JQ: &str = r#"[.[] | select(.disabled | not)] | (map(.scale) | max) as $s |
         "\(map(.x + lw) | max * $s + 0.5 | floor)x\(map(.y + lh) | max * $s + 0.5 | floor)""#;
/// calibrate --answer --json (#273, audit J8): the verdict.
pub const CALIB_JSON_JQ: &str = r#"
      def num: tonumber? // null;
      ($r | split(" ")) as $v
      | {result: (if $v[0] == "one" or $v[0] == "fit" or $v[0] == "up" then $v[0] else "none" end),
         card: {w: ($w | num), h: ($h | num)}, marks: $n, scale: null, scale_x: null, scale_y: null, fit: null,
         fit_size: null, error_px: null, worst: null, tolerance_px: null, off: {}, shot_fit: ($cur | num)}
      + if $v[0] == "one" then {scale: 1, error_px: ($v[1] | num), worst: $v[2]}
        elif $v[0] == "fit" then {scale: ($v[1] | num), scale_x: ($v[2] | num), scale_y: ($v[3] | num), fit: ($v[4] | num),
          fit_size: $v[5], error_px: ($v[6] | num), worst: $v[7]}
        elif $v[0] == "up" then {scale: ($v[1] | num)}
        elif $v[0] == "off" then {scale: (($v[1] | num) as $s | if $s == 0 then null else $s end), tolerance_px: ($v[2] | num),
          off: ([$v[3:][] | select(test("^[A-Z]:[0-9]+$")) | split(":") | {(.[0]): (.[1] | tonumber)}] | add // {})}
        else {} end"#;

/// calib_marks (#188): one mark in each cell of a 3 x 2 grid, at a random point of its middle (awk's
/// rand: the same seed, the same marks).
pub const CALIB_MARKS_AWK: &str = r#"BEGIN {
    srand(seed)
    for (i = 0; i < n; i++) p[i] = i
    for (i = n - 1; i > 0; i--) { j = int(rand() * (i + 1)); t = p[i]; p[i] = p[j]; p[j] = t }
    for (i = 0; i < n; i++) {
      x = w * (i % 3 + 0.2 + 0.6 * rand()) / 3; y = h * (int(i / 3) + 0.2 + 0.6 * rand()) / 2
      if (x < 0.08 * w) x = 0.08 * w; if (x > 0.92 * w) x = 0.92 * w
      if (y < 0.08 * h) y = 0.08 * h; if (y > 0.92 * h) y = 0.92 * h
      printf "%c %d %d\n", 65 + p[i], x, y
    }}"#;
/// calib_fit: which scale maps the readings onto the marks (awk).
pub const CALIB_FIT_AWK: &str = r#"
  function hyp(a, b) { return sqrt(a * a + b * b) }
  BEGIN {
    long = w > h ? w : h; short = w > h ? h : w; tol = long / 160; if (tol < 8) tol = 8
    split(marks, ml, "\n"); for (i in ml) if (split(ml[i], f, " ") == 3) { mx[f[1]] = f[2]; my[f[1]] = f[3] }
    n = 0; split(reads, rl, "\n")
    for (i = 1; i in rl; i++) if (split(rl[i], f, " ") == 3 && (f[1] in mx)) { n++; L[n] = f[1]; rx[n] = f[2]; ry[n] = f[3] }
    if (n == 0) { print "none"; exit 1 }
    m = 0; worst = L[1]
    for (i = 1; i <= n; i++) { e = hyp(rx[i] - mx[L[i]], ry[i] - my[L[i]]); if (e > m) { m = e; worst = L[i] } }
    if (m <= tol) { printf "one %d %s\n", m + 0.5, worst; exit 0 }
    k = 0
    for (i = 1; i <= n; i++) { r[++k] = rx[i] / mx[L[i]]; r[++k] = ry[i] / my[L[i]] }
    for (i = 2; i <= k; i++) { v = r[i]; for (j = i - 1; j >= 1 && r[j] > v; j--) r[j + 1] = r[j]; r[j + 1] = v }
    s0 = k % 2 ? r[(k + 1) / 2] : (r[k / 2] + r[k / 2 + 1]) / 2
    if (s0 <= 0) { printf "off 0 %d", tol; for (i = 1; i <= n; i++) printf " %s:%d", L[i], hyp(rx[i] - mx[L[i]], ry[i] - my[L[i]]) + 0.5; print ""; exit 1 }
    # (None near it: all of them.)
    for (pass = 1; pass <= 2 && !dx; pass++)
      for (i = 1; i <= n; i++) {
        x = mx[L[i]]; y = my[L[i]]
        if (pass == 1 && hyp(rx[i] / s0 - x, ry[i] / s0 - y) > 3 * tol) continue
        cx += x * rx[i]; dx += x * x; cy += y * ry[i]; dy += y * y
      }
    s = (cx + cy) / (dx + dy); sx = cx / dx; sy = cy / dy
    out = ""
    for (i = 1; i <= n; i++) {
      e = hyp(rx[i] / s - mx[L[i]], ry[i] / s - my[L[i]])
      if (e > tol) out = out sprintf(" %s:%d", L[i], e + 0.5)
    }
    if (out != "" || s < 0.2) { printf "off %.3f %d%s\n", s, tol, out; exit 1 }
    if (s > 1) { printf "up %.3f\n", s; exit 1 }
    N = int(s * long / 8 + 0.5) * 8; k = N / long; S = int(short * k + 0.5)
    m = 0; worst = L[1]
    for (i = 1; i <= n; i++) { e = hyp(rx[i] / k - mx[L[i]], ry[i] / k - my[L[i]]); if (e > m) { m = e; worst = L[i] } }
    printf "fit %.3f %.3f %.3f %d %s %d %s\n", s, sx, sy, N, w >= h ? N "x" S : S "x" N, m + 0.5, worst
  }"#;

/// Every inline program (and the awk ones), for `_rust-jq --inline` (the rest are a path, .kind, .[0], length).
pub const INLINE: [&str; 47] = [
    CALIB_MARKS_AWK,
    CALIB_FIT_AWK,
    ZERO_NOTE_JQ,
    SHOT_SAID_JQ,
    SHOT_MON_JQ,
    MON_NAMES_JQ,
    MON_NAMED_JQ,
    SHOT_SCALE_JQ,
    WIN_STATE_JQ,
    WIN_AT2_JQ,
    CALIB_SIZE_JQ,
    CALIB_JSON_JQ,
    WIN_BLOCKED_JQ,
    WIN_LAYER_JQ,
    WIN_INSIDE_JQ,
    WIN_AT_JQ,
    ACTIVE_ADDR_JQ,
    LAYER_BIG_JQ,
    WIN_LINES_JQ,
    WIN_DESC_ONE_JQ,
    LAYER_AT_JQ,
    WIN_FOCUSED_JQ,
    WIN_PRESENT_JQ,
    POLL_JSON_JQ,
    CURSOR_JQ,
    SETTLE_JSON_JQ,
    SNAP_JQ,
    WINDOWS_JSON_JQ,
    WINDOWS_TABLE_JQ,
    WINDOWS_LAYERS_JQ,
    LAYER_HITS_JQ,
    LAYER_LINES_JQ,
    LAYER_DESC_JQ,
    LAYER_FIELDS_JQ,
    LAYER_TIED_JQ,
    LAYER_KB_JQ,
    TREE_TWINS_JQ,
    EXTENT_JQ,
    MAIN_SCREEN_I_JQ,
    MAIN_SCREEN_H_JQ,
    BOX_SCREEN_JQ,
    WIN_SCALE_JQ,
    BY_ADDRESS_JQ,
    PIXEL_OFF_JQ,
    WIN_REGION_JQ,
    WIN_SIZE_JQ,
    WIN_SIZE2_JQ,
];

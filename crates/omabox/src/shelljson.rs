//! The box shell's shell.json, made from the user's or Omarchy's (#324 wave 5a): shell_json_filter,
//! place_plugin, center_split. jq runs each program as bash's omabox wrote it (#324).

use crate::cat;
use crate::jq::{self, Args, Ran};
use crate::shell;

// CENTER_SPLIT_JQ: Omarchy 4.5's migration of a bar.centerAnchor (1791969355, #293, finding 418).
macro_rules! center_split_jq { () => { r#"def center_split:
  def entry_id: if type == "object" then (.id // "" | tostring) else tostring end;
  def list: if type == "array" then . else [] end;
  if (.bar | type) == "object" and (.bar | has("centerAnchor")) and ((.bar.id // "" | tostring) | IN("", "omarchy.bar"))
  then (.bar.centerAnchor | if type == "string" then . else "" end) as $anchor
    | (if (.bar.layout | type) == "object" then .bar.layout.center | list else [] end) as $center
    | (first(range(0; $center | length) | select($anchor != "" and ($center[.] | entry_id) == $anchor)) // null) as $index
    | if $index != null then
        .bar.layout.centerBegin = (.bar.layout.centerBegin | list) + $center[0:$index]
        | .bar.layout.center = [$center[$index]]
        | .bar.layout.centerEnd = $center[$index + 1:] + (.bar.layout.centerEnd | list)
      else . end
    | del(.bar.centerAnchor)
  else . end;"# }; }

/// shell_json_filter's program, after CENTER_SPLIT_JQ.
const FILTER_JQ: &str = r#"
      def wid: if type == "object" then .id else . end;
      def keep: wid as $i | ($i | startswith("omarchy.")) or ($keep | index($i));
      def pager: wid as $i | $pagers | index($i);
      def ws: {id: "omarchy.workspaces"};
      if $split then center_split else . end
      | if $notify and .disabledPlugins then .disabledPlugins -= ["omarchy.notifications"] else . end
      | .idle = {screensaver: 1000000, lock: 1000000}
      | if (.bar.layout? | type) == "object"
          and ([.bar.layout[]?[]? | wid] | index("omarchy.workspaces") | not)
          and ([$keep[] | select(. as $k | $pagers | index($k))] | length == 0)
        then (first(.bar.layout | to_entries[] | .key as $s | .value | arrays | to_entries[]
                | select(.value | pager) | [$s, .key]) // null) as $at
          | if $at then .bar.layout[$at[0]][$at[1]] = ws
            else .bar.layout.left = ((.bar.layout.left // []) as $l | ($l | map(wid) | index("omarchy.menu")) as $m
              | if $m then $l[:$m + 1] + [ws] + $l[$m + 1:] else [ws] + $l end) end
        else . end
      | if (.bar.layout? | type) == "object" then .bar.layout |= map_values(map(select(keep))) else . end
      | if .plugins then .plugins |= map(select(keep)) else . end
      # omarchy<4.5: a bar.centerAnchor naming a widget left out of the box (none after center_split)
      | (.bar.centerAnchor? // "") as $a
      | if $a != "" and (($a | startswith("omarchy.")) or ($keep | index($a)) | not)
        then del(.bar.centerAnchor) else . end"#;

/// --desk-shell's (#206): the desk's shell.json as it is, idle held off, omabox's own widget out.
const DESK_JQ: &str = r#".idle = {screensaver: 1000000, lock: 1000000}
      | if $split then center_split else . end
      | if $keep | index("chaves.omabox") then . else
          walk(if type == "array" then map(select((if type == "object" then .id else . end) != "chaves.omabox")) else . end)
          # omarchy<4.5: an anchor (none after center_split)
          | if .bar.centerAnchor? == "chaves.omabox" then del(.bar.centerAnchor) else . end end"#;

/// --clean's frozen clock (#252, finding 357).
pub const CLEAN_CLOCK_JQ: &str = r#"(.bar.layout[]?[]? | select(.id == "omarchy.clock")) |= (.format = $f | .verticalFormat = $v)"#;

/// place_plugin's.
const PLACE_JQ: &str = r#"
    def in_bar: [(.bar.layout // {})[]?[]? | if type == "object" then .id else . end] | index($id);
    def in_mounted: [(.plugins // [])[] | select(.id != $id and (.id as $p | $mounted | index($p)))
      | .. | if type == "object" then .id? elif type == "string" then . else empty end] | index($id);
    if ($kinds | index("bar")) then .bar.id = $id
    elif ($kinds | index("bar-widget")) then
      if in_bar or in_mounted then . else .bar.layout[$sec] = ((.bar.layout[$sec] // []) + [{id: $id}]) end
    elif ([(.plugins // [])[].id] | index($id)) then .
    else .plugins = ((.plugins // []) + [{id: $id}]) end"#;

/// workspace_widgets': a bar widget whose manifest says "workspace".
const WS_JQ: &str = r#"select(.barWidget and ([.name, .description, .barWidget.displayName?, .barWidget.description?,
          (.barWidget.aliases? // [])[]?] | map(strings) | any(test("workspace"; "i")))) | .id | strings"#;

fn tf(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

/// `jq ARGS < INPUT`: the file's bytes as they are (no here-string newline), jq's stderr dropped.
fn jq_in(words: Vec<Vec<u8>>, input: &[u8]) -> Ran {
    jq::tool("jq", &words, Some(input), jq::Err::Null)
}

/// shell_json_filter KEEP NOTIFY PAGERS [SPLIT] < INPUT: the user's shell.json made fit for a box:
/// only built-in widgets and the mounted plugins (KEEP, a JSON array of ids), the built-in
/// notifications back on unless a mounted plugin serves them, idle held off, a bar left with no
/// workspace numbers given omarchy.workspaces (#21) unless a mounted plugin is one (PAGERS), and on
/// a split centre (SPLIT, 4.5) Omarchy's migration of an anchor first.
pub fn shell_json_filter(
    keep: &[u8],
    notify: bool,
    pagers: &[u8],
    split: bool,
    input: &[u8],
) -> Ran {
    let w = Args::new(&[])
        .kv("--argjson", "keep", keep)
        .kv("--argjson", "notify", tf(notify))
        .kv("--argjson", "pagers", pagers)
        .kv("--argjson", "split", tf(split))
        .arg(cat!(center_split_jq!(), FILTER_JQ))
        .into_words();
    jq_in(w, input)
}

/// --desk-shell's shell.json (#206): FILE as it is, idle held off, Omarchy's centre split when the
/// box's Omarchy has it, omabox's own widget out of every list unless KEEP has it.
pub fn desk_shell_json(keep: &[u8], split: bool, file: &[u8]) -> Ran {
    Args::new(&[])
        .kv("--argjson", "keep", keep)
        .kv("--argjson", "split", tf(split))
        .arg(cat!(center_split_jq!(), DESK_JQ))
        .arg(file)
        .run_file(jq::Err::Null)
}

/// --clean's clock frozen in FILE (#252): its text a literal format, so it never ticks. jq's own
/// words on stderr when it cannot.
pub fn clean_clock(file: &[u8]) -> Ran {
    Args::new(&[])
        .kv("--arg", "f", crate::upspec::CLEAN_CLOCK)
        .kv("--arg", "v", crate::upspec::CLEAN_CLOCK_VERTICAL)
        .arg(CLEAN_CLOCK_JQ)
        .arg(file)
        .run_file(jq::Err::Inherit)
}

/// place_plugin ID SECTION KINDS MOUNTED < INPUT: a mounted plugin where its manifest says, unless
/// the layout has it (in the bar, or in a mounted plugin's own layout: finding 151).
pub fn place_plugin(id: &[u8], sec: &[u8], kinds: &[u8], mounted: &[u8], input: &[u8]) -> Ran {
    let w = Args::new(&[])
        .kv("--arg", "id", id)
        .kv("--arg", "sec", sec)
        .kv("--argjson", "kinds", kinds)
        .kv("--argjson", "mounted", mounted)
        .arg(PLACE_JQ)
        .into_words();
    // (its stderr the caller's, as bash's place_plugin has it)
    jq::tool("jq", &w, Some(input), jq::Err::Inherit)
}

/// widget_section SECTION SPLIT: where a mounted widget whose manifest names SECTION (or none) goes.
/// On a split centre one naming the centre, none or no section goes beside the clock, in centerEnd
/// (finding 418); before 4.5, the centre.
pub fn widget_section(sec: &[u8], split: bool) -> Vec<u8> {
    if split {
        match sec {
            b"left" | b"centerBegin" | b"centerEnd" | b"right" | b"tray" => sec.to_vec(),
            _ => b"centerEnd".to_vec(),
        }
    } else if sec.is_empty() {
        b"center".to_vec()
    } else {
        sec.to_vec()
    }
}

/// center_split: whether the box's Omarchy (OMARCHY, else the installed one) splits the bar's centre.
pub fn center_split(omarchy: &[u8]) -> bool {
    let o: &[u8] = if omarchy.is_empty() {
        b"/usr/share/omarchy"
    } else {
        omarchy
    };
    std::fs::read(shell::path(&cat!(o, "/shell/plugins/bar/Bar.qml")))
        .is_ok_and(|t| crate::exit::contains(&t, b"centerBegin"))
}

/// workspace_widgets: the ids of the workspace widgets among the mounted plugins (DIRS) and the
/// user's (~/.config/omarchy/plugins), as a JSON array: a bar widget whose manifest says
/// "workspace". A plugin left out of the box is only known by its manifest.
pub fn workspace_widgets(dirs: &[Vec<u8>]) -> Ran {
    let mut ms: Vec<Vec<u8>> = dirs.iter().map(|d| cat!(d, "/manifest.json")).collect();
    // (`printf '%s\n' "${PLUGINS[@]}"` of none is one empty line)
    if ms.is_empty() {
        ms.push(b"/manifest.json".to_vec());
    }
    let pd = cat!(shell::env_or_empty("HOME"), "/.config/omarchy/plugins");
    for n in shell::glob(&shell::path(&pd), false) {
        ms.push(cat!(pd, "/", n, "/manifest.json"));
    }
    let mut ids = Vec::new();
    for m in ms {
        if !shell::path(&m).is_file() {
            continue;
        }
        let r = Args::new(&["-c", WS_JQ]).arg(&m).run_file(jq::Err::Null);
        ids.extend_from_slice(&r.out);
    }
    jq::tool(
        "jq",
        &[b"-s".as_slice(), b"unique"],
        Some(&ids),
        jq::Err::Inherit,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections() {
        assert_eq!(widget_section(b"", true), b"centerEnd");
        assert_eq!(widget_section(b"center", true), b"centerEnd");
        assert_eq!(widget_section(b"right", true), b"right");
        assert_eq!(widget_section(b"", false), b"center");
        assert_eq!(widget_section(b"right", false), b"right");
    }

    #[test]
    fn filter_runs() {
        let r = shell_json_filter(
            b"[\"x.y\"]",
            true,
            b"[]",
            false,
            br#"{"bar":{"layout":{"left":[{"id":"omarchy.menu"},"a.b","x.y"]}},"disabledPlugins":["omarchy.notifications"]}"#,
        );
        assert!(r.ok());
        let v: serde_json::Value = serde_json::from_slice(&r.out).unwrap_or_default();
        assert_eq!(
            v["bar"]["layout"]["left"],
            serde_json::json!([{"id":"omarchy.menu"},{"id":"omarchy.workspaces"},"x.y"])
        );
        assert_eq!(v["disabledPlugins"], serde_json::json!([]));
    }
}

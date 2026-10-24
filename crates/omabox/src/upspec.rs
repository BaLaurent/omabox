//! up's option values (#324 wave 5a): mounts, seeds, addresses, ports, --clean's, as up reads them.
#![allow(dead_code)] // (until cmd/up.rs uses them)

// up --clean (#252, finding 357): what it sets, and what --keep can take back. The theme is Omarchy's
// default; the wallpaper a colour no theme uses, so any of it inside a window or the bar is a hole (a
// 16x16 PNG of it, scaled); the clock a literal Qt format, which keeps its widget, place and width.
// The locale and timezone (finding 363): English in UTF-8 and UTC for the whole session.
pub const CLEAN_THEME: &str = "tokyo-night";
pub const CLEAN_WALLPAPER: &str = "#ff00ff";
pub const CLEAN_WALLPAPER_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAGUlEQVQoz2P8z/CfgRTARJLqUQ2jGoaUBgBKTgIeHXO4xgAAAABJRU5ErkJggg==";
pub const CLEAN_CLOCK: &str = "'Friday 12:00'";
pub const CLEAN_CLOCK_VERTICAL: &str = "'12'\n'\u{2014}'\n'00'";
pub const CLEAN_TZ: &str = "UTC";
pub const CLEAN_KEEPS: &str = "animations bar clock fonts locale pointer wallpaper";

// omabox-keyboard: type on the Wayland display in $WAYLAND_DISPLAY through a virtual keyboard.
//
//   omabox-keyboard [--layout us] [--variant V] [--model M] [--options O] [--delay MS]  COMBO | -t TEXT | -T | -s MS | -m MODS | -p MS | down:KEY | up:KEY ...
//   omabox-keyboard [--layout ...] --hold
//
// COMBO is super+space, ctrl+shift+t, Return, a, F5, ctrl++, super+/ ... (modifiers: super ctrl shift
// alt altgr; a key is a keysym name or a single character). With modifiers a letter is the key, not
// the shifted symbol: SUPER+W is super+w, as in Hyprland's binds; shift+w for SUPER+SHIFT+W. On its own
// an uppercase letter types it (A is shift+a). -t types TEXT (newline and tab as Return and Tab; any
// other control character is refused, exit 2); -T types the next NUL-terminated text read from stdin
// (a secret: never in argv, where the process list shows it; one trailing \r dropped); -s sleeps.
// -m MODS (ctrl, shift+alt, ...) presses those modifiers and keeps them down to the end of the run,
// under whatever comes after; -p MS prints "paused" and waits for a line (or the end) on stdin, MS at
// most: `omabox click --mod` clicks there with the pointer tool (#25). Held modifiers are released
// at the end of the run, on a timed-out pause, and on SIGTERM, SIGINT or SIGHUP: never left down.
// down:KEY presses KEY and leaves it down after the run, up:KEY (in this run or a later one) lets go
// of it (#230): a holder, forked off by the first down:, keeps a keyboard of its own on the seat while
// any key is held, answering on $XDG_RUNTIME_DIR/omabox-held-keys, and ends with the last up:. KEY is
// one key, as in a COMBO without modifiers (a letter is its key: down:A holds a).
// Tokens run left to right in one connection, all checked before it connects. Exit 1 if the
// compositor goes away mid-run.
// --hold keeps an idle keyboard on the seat until the compositor goes away (NOTES finding 41).
//
// Why not wtype: wtype uploads a keymap of its own and numbers keys from keycode 9. Hyprland replaces
// a virtual keyboard's keymap with its configured layout, so those keycodes are read as other keys
// (the first one as Escape). This tool uses real evdev keycodes looked up in the same XKB layout the
// compositor applies, so binds and clients see what a physical keyboard would send. A character the
// layout lacks (Ü on us) is bound to a spare keycode in the keymap this keyboard uploads (finding 55).
#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include <wayland-client.h>
#include <xkbcommon/xkbcommon.h>

#include "virtual-keyboard-unstable-v1-client-protocol.h"
#include "../common/box.h"
#include "../common/roundtrip.h"

static struct wl_seat *seat;
static struct zwp_virtual_keyboard_manager_v1 *manager;
static struct wl_display *display;
static struct zwp_virtual_keyboard_v1 *kbd;
static struct xkb_keymap *keymap;
static unsigned delay_ms = 12;

static void global(void *data, struct wl_registry *reg, uint32_t name, const char *iface, uint32_t version) {
    (void)data; (void)version;
    if (!strcmp(iface, wl_seat_interface.name) && !seat)
        seat = wl_registry_bind(reg, name, &wl_seat_interface, 1);
    else if (!strcmp(iface, zwp_virtual_keyboard_manager_v1_interface.name))
        manager = wl_registry_bind(reg, name, &zwp_virtual_keyboard_manager_v1_interface, 1);
}
static void global_remove(void *data, struct wl_registry *reg, uint32_t name) { (void)data; (void)reg; (void)name; }
static const struct wl_registry_listener registry_listener = {global, global_remove};

static uint32_t now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint32_t)(ts.tv_sec * 1000 + ts.tv_nsec / 1000000);
}
static void sleep_ms(long ms) {
    struct timespec ts = {ms / 1000, (ms % 1000) * 1000000L};
    nanosleep(&ts, NULL);
}

static void usage(void) {
    fprintf(stderr, "usage: omabox-keyboard [--layout L] [--variant V] [--model M] [--options O] [--delay MS] [--hold | (COMBO | -t TEXT | -T | -s MS | -m MODS | -p MS | down:KEY | up:KEY)...]\n");
    exit(2);
}

// A whole decimal number in [lo, hi], or usage(): no junk ("abc" was 0), no negatives wrapped into
// huge sleeps (-s -1 slept 49 days).
static long num(const char *s, long lo, long hi) {
    char *end;
    long v = strtol(s, &end, 10);
    if (!*s || *end || v < lo || v > hi) { fprintf(stderr, "omabox-keyboard: bad number '%s'\n", s); usage(); }
    return v;
}

// A lost compositor (box gone, protocol error) is a failure, not "keys sent" (finding 64).
static void sync_or_die(void) {
    int r = omabox_roundtrip(display, 10000);   // a stopped compositor never answers (finding 177)
    if (r == -2) { fprintf(stderr, "omabox-keyboard: lost the compositor (no answer in 10 s)\n"); exit(3); }
    if (r < 0) { fprintf(stderr, "omabox-keyboard: lost the compositor\n"); exit(1); }
}

// A keysym's first (keycode, modifiers) in layout 0 of the keymap, lowest level first. The modifiers
// that reach a level come from the key's type: Shift for level 2 on most keys, AltGr (Mod5) for levels
// 3 and 4 on layouts that have them. Levels needing a modifier this tool cannot press are skipped.
// Keycodes up to 255 first: X11 keycodes are 8 bits, so Xwayland cannot give an X client a key above
// that (evdev keymaps run to 709; the us layout has € only on keycode 443).
struct hit { xkb_keycode_t code; xkb_mod_mask_t mask; };
static xkb_mod_mask_t pressable;
#define X11_MAX_KEYCODE 255
static int find_keysym_in(xkb_keysym_t sym, struct hit *out, xkb_keycode_t min, xkb_keycode_t max) {
    for (xkb_level_index_t level = 0; level < 4; level++)
        for (xkb_keycode_t kc = min; kc <= max; kc++) {
            if (level >= xkb_keymap_num_levels_for_key(keymap, kc, 0)) continue;
            const xkb_keysym_t *syms;
            int n = xkb_keymap_key_get_syms_by_level(keymap, kc, 0, level, &syms);
            if (n != 1 || syms[0] != sym) continue;
            xkb_mod_mask_t masks[8];
            size_t nm = xkb_keymap_key_get_mods_for_level(keymap, kc, 0, level, masks, 8);
            for (size_t i = 0; i < nm; i++)
                if (!(masks[i] & ~pressable)) { out->code = kc; out->mask = masks[i]; return 1; }
        }
    return 0;
}
static int find_keysym_x11(xkb_keysym_t sym, struct hit *out) {
    xkb_keycode_t min = xkb_keymap_min_keycode(keymap), max = xkb_keymap_max_keycode(keymap);
    return find_keysym_in(sym, out, min, max < X11_MAX_KEYCODE ? max : X11_MAX_KEYCODE);
}
static int find_keysym(xkb_keysym_t sym, struct hit *out) {
    return find_keysym_x11(sym, out) ||
           find_keysym_in(sym, out, X11_MAX_KEYCODE + 1, xkb_keymap_max_keycode(keymap));
}

struct mod { const char *names[4]; const char *xkb; const char *keysym; };
static const struct mod mods[] = {
    {{"super", "logo", "win", "mod"}, XKB_MOD_NAME_LOGO, "Super_L"},   // mod: Hyprland's name for SUPER
    {{"ctrl", "control"}, XKB_MOD_NAME_CTRL, "Control_L"},
    {{"shift"}, XKB_MOD_NAME_SHIFT, "Shift_L"},
    {{"alt"}, XKB_MOD_NAME_ALT, "Alt_L"},
    {{"altgr"}, "Mod5", "ISO_Level3_Shift"},
};
#define NMODS (sizeof(mods) / sizeof(mods[0]))

static int mod_index(const char *name) {
    for (size_t m = 0; m < NMODS; m++)
        for (int i = 0; i < 4 && mods[m].names[i]; i++)
            if (!strcasecmp(name, mods[m].names[i])) return (int)m;
    return -1;
}
static uint32_t mod_mask(int m) { return 1u << xkb_keymap_mod_get_index(keymap, mods[m].xkb); }
static unsigned held_for(xkb_mod_mask_t mask) {
    unsigned held = 0;
    for (size_t m = 0; m < NMODS; m++) if (mask & mod_mask((int)m)) held |= 1u << m;
    return held;
}

// evdev codes are XKB keycodes minus 8.
static void key(xkb_keycode_t code, int down) {
    zwp_virtual_keyboard_v1_key(kbd, now_ms(), code - 8, down ? WL_KEYBOARD_KEY_STATE_PRESSED : WL_KEYBOARD_KEY_STATE_RELEASED);
}
static void set_mods(uint32_t depressed) { zwp_virtual_keyboard_v1_modifiers(kbd, depressed, 0, 0, 0); }

// Hyprland makes a keyboard the active one on its first event after another's (a new one's first of
// all, finding 14), re-sending keymap and enter to the focused client, and the event that caused the
// switch is lost to it. Spent on a modifiers event that changes nothing before the first key, and again
// after the holder's keyboard (down:, up:) had a turn.
static int spent;
static void spend(uint32_t depressed) {
    if (spent) return;
    set_mods(depressed);
    sync_or_die();
    sleep_ms(20);
    spent = 1;
}

// The modifiers -m holds down (bitmask over mods[]), their keys, and the modifier mask they make.
static unsigned kept;
static xkb_keycode_t kept_code[NMODS];
static uint32_t kept_mask;

// Each modifier in `held` has a key in the layout (its keycode in codes[]), or 0, said.
static int mod_keys(unsigned held, xkb_keycode_t *codes) {
    for (size_t m = 0; m < NMODS; m++) {
        struct hit h;
        if (!(held & (1u << m))) continue;
        if (!find_keysym(xkb_keysym_from_name(mods[m].keysym, 0), &h)) {
            fprintf(stderr, "omabox-keyboard: no key for modifier %s in this layout\n", mods[m].names[0]);
            return 0;
        }
        codes[m] = h.code;
    }
    return 1;
}

// Press the modifiers in `held` (bitmask over mods[]), tap `code`, release in reverse. With run = 0 it
// only checks that every modifier has a key in the layout. Modifiers -m holds stay down throughout.
static int chord(unsigned held, xkb_keycode_t code, int run) {
    uint32_t mask = kept_mask;
    xkb_keycode_t mk[NMODS] = {0};
    held &= ~kept;
    if (!mod_keys(held, mk)) return 0;
    if (!run) return 1;
    spend(mask);
    for (size_t m = 0; m < NMODS; m++) {
        if (!(held & (1u << m))) continue;
        key(mk[m], 1);
        set_mods(mask |= mod_mask((int)m));
    }
    key(code, 1);
    sync_or_die();
    key(code, 0);
    for (int m = (int)NMODS - 1; m >= 0; m--) {
        if (!(held & (1u << m))) continue;
        key(mk[m], 0);
        set_mods(mask &= ~mod_mask(m));
    }
    sync_or_die();
    sleep_ms(delay_ms);
    return 1;
}

// -m: press these modifiers and keep them down. release_held lets go of all of them, once.
static void hold_mods(unsigned held) {
    spend(kept_mask);
    held &= ~kept;
    for (size_t m = 0; m < NMODS; m++) {
        if (!(held & (1u << m))) continue;
        key(kept_code[m], 1);
        set_mods(kept_mask |= mod_mask((int)m));
        kept |= 1u << m;
    }
    sync_or_die();
}
static void release_held(void) {
    if (!kept) return;
    for (int m = (int)NMODS - 1; m >= 0; m--) {
        if (!(kept & (1u << m))) continue;
        key(kept_code[m], 0);
        set_mods(kept_mask &= ~mod_mask(m));
    }
    kept = 0;
    sync_or_die();
}

// A run that holds modifiers ends by releasing them on these signals too (SIGKILL cannot be caught).
static volatile sig_atomic_t stop;
static void on_signal(int sig) { stop = sig; }

// -p MS: "paused" on stdout, then a line or the end of stdin, MS at most. 0 when it timed out or a
// signal came.
static int pause_for(long ms) {
    printf("paused\n");
    fflush(stdout);
    struct timespec t0, t;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    for (;;) {
        if (stop) return 0;
        clock_gettime(CLOCK_MONOTONIC, &t);
        long left = ms - ((t.tv_sec - t0.tv_sec) * 1000 + (t.tv_nsec - t0.tv_nsec) / 1000000);
        if (left <= 0) { fprintf(stderr, "omabox-keyboard: nothing on stdin after %ld ms: letting go\n", ms); return 0; }
        struct pollfd p = {0, POLLIN, 0};
        int r = poll(&p, 1, (int)left);
        if (r < 0 && errno == EINTR) continue;
        if (r < 0) return 0;
        if (r == 0) continue;
        char c;
        ssize_t n = read(0, &c, 1);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0 || c == '\n') return 1;   // the end of stdin is "go on" too: whoever held it is gone
    }
}

static const struct { const char *alias, *name; } aliases[] = {
    {"enter", "Return"}, {"esc", "Escape"}, {"del", "Delete"}, {"pgup", "Prior"}, {"pageup", "Prior"},
    {"pgdn", "Next"}, {"pagedown", "Next"}, {"backspace", "BackSpace"}, {"bs", "BackSpace"},
    // a modifier pressed on its own
    {"shift", "Shift_L"}, {"ctrl", "Control_L"}, {"control", "Control_L"}, {"alt", "Alt_L"},
    {"super", "Super_L"}, {"logo", "Super_L"}, {"win", "Super_L"}, {"mod", "Super_L"}, {"altgr", "ISO_Level3_Shift"},
};

// One UTF-8 character, strictly: no stray continuation bytes, overlongs, surrogates or values past
// U+10FFFF (Latin-1 bytes are "bad UTF-8", not garbage code points).
static int decode(const unsigned char **pp, uint32_t *cp) {
    const unsigned char *p = *pp;
    int len;
    uint32_t min;
    if (!*p) return 0;   // an empty token: no character, and not a step past its NUL
    if (*p < 0x80) { *cp = *p; len = 1; min = 0; }
    else if (*p >= 0xC2 && *p <= 0xDF) { *cp = *p & 0x1f; len = 2; min = 0x80; }
    else if ((*p >> 4) == 14) { *cp = *p & 0x0f; len = 3; min = 0x800; }
    else if (*p >= 0xF0 && *p <= 0xF4) { *cp = *p & 0x07; len = 4; min = 0x10000; }
    else return 0;
    for (int i = 1; i < len; i++) {
        if ((p[i] & 0xC0) != 0x80) return 0;
        *cp = (*cp << 6) | (p[i] & 0x3f);
    }
    if (*cp < min || *cp > 0x10FFFF || (*cp >= 0xD800 && *cp <= 0xDFFF)) return 0;
    *pp = p + len;
    return 1;
}

// -m's MODS: ctrl, shift+alt, ... (the modifier names of a COMBO, nothing else) as a bitmask, 0 if bad.
static unsigned parse_mods(const char *tok) {
    char buf[256], *save = NULL;
    unsigned held = 0;
    if (snprintf(buf, sizeof(buf), "%s", tok) >= (int)sizeof(buf)) return 0;
    for (char *name = strtok_r(buf, "+", &save); name; name = strtok_r(NULL, "+", &save)) {
        int m = mod_index(name);
        if (m < 0) { fprintf(stderr, "omabox-keyboard: -m: '%s' is not a modifier (super ctrl shift alt altgr)\n", name); return 0; }
        held |= 1u << m;
    }
    if (!held) fprintf(stderr, "omabox-keyboard: -m: no modifier in '%s'\n", tok);
    return held;
}

// A key's name (a keysym name, an alias, a single character) as its keysym, NoSymbol if none; *as is
// the name it was looked up by (an alias's keysym name).
static xkb_keysym_t key_sym(const char *name, const char **as) {
    for (size_t i = 0; i < sizeof(aliases) / sizeof(aliases[0]); i++)
        if (!strcasecmp(name, aliases[i].alias)) name = aliases[i].name;
    *as = name;
    xkb_keysym_t sym = xkb_keysym_from_name(name, XKB_KEYSYM_NO_FLAGS);
    if (sym == XKB_KEY_NoSymbol) sym = xkb_keysym_from_name(name, XKB_KEYSYM_CASE_INSENSITIVE);
    // A single character is its own keysym: + - / are not keysym names (plus, minus, slash are).
    const unsigned char *p = (const unsigned char *)name;
    uint32_t cp;
    if (sym == XKB_KEY_NoSymbol && decode(&p, &cp) && !*p) sym = xkb_utf32_to_keysym(cp);
    return sym;
}

static int do_combo(const char *tok, int run) {
    char buf[256];
    if (snprintf(buf, sizeof(buf), "%s", tok) >= (int)sizeof(buf)) { fprintf(stderr, "omabox-keyboard: token too long\n"); return 0; }
    unsigned held = 0;
    char *keyname = buf, *plus;
    while ((plus = strchr(keyname, '+')) && plus[1]) {  // "ctrl++" means ctrl and '+'
        *plus = 0;
        int m = mod_index(keyname);
        if (m < 0) { fprintf(stderr, "omabox-keyboard: unknown modifier '%s' in %s\n", keyname, tok); return 0; }
        held |= 1u << m;
        keyname = plus + 1;
    }
    const char *as;
    xkb_keysym_t sym = key_sym(keyname, &as);
    keyname = (char *)as;
    uint32_t cp;
    // With modifiers a letter names the key, as Hyprland's binds do (SUPER+W is not SUPER+SHIFT+W).
    if (held) sym = xkb_keysym_to_lower(sym);
    struct hit h;
    if (sym == XKB_KEY_NoSymbol || !find_keysym(sym, &h)) {
        // A character on no key of the layout: -t gives it a spare keycode (finding 176).
        const unsigned char *q = (const unsigned char *)keyname;
        int one = sym != XKB_KEY_NoSymbol && !held && decode(&q, &cp) && !*q;
        fprintf(stderr, "omabox-keyboard: no key for '%s' in this layout%s%s%s\n", keyname,
                one ? " (omabox keys -t '" : "", one ? keyname : "", one ? "' types characters outside the layout)" : "");
        return 0;
    }
    return chord(held | held_for(h.mask), h.code, run); // A, question, ...: the shifted symbol
}

// Characters in -t TEXT that the layout cannot produce get a keycode of their own: one the layout
// leaves empty, bound to that keysym in an extra `key` line of the symbols section. The keyboard then
// uploads that keymap, and a client decodes the spare keycode as the character.
static int text_keysym(uint32_t cp) { return cp == '\n' ? XKB_KEY_Return : cp == '\t' ? XKB_KEY_Tab : (int)xkb_utf32_to_keysym(cp); }
// A character on no key up to 255 gets a spare there, and spares up to 255 come first (X11, above);
// only when those run out, higher ones.
#define MAXWANT 128
static struct xkb_keymap *with_spares(struct xkb_context *ctx, int argc, char **argv, int first) {
    xkb_keysym_t want[MAXWANT];
    size_t nwant = 0, dropped = 0;
    for (int a = first; a < argc; a++) {
        if (strcmp(argv[a], "-t") || a + 1 >= argc) continue;
        const unsigned char *p = (const unsigned char *)argv[++a];
        uint32_t cp;
        while (*p && decode(&p, &cp)) {
            xkb_keysym_t sym = (xkb_keysym_t)text_keysym(cp);
            struct hit h;
            if (sym == XKB_KEY_NoSymbol || find_keysym_x11(sym, &h)) continue;
            size_t k = 0;
            while (k < nwant && want[k] != sym) k++;
            if (k == nwant) { if (nwant < MAXWANT) want[nwant++] = sym; else dropped++; }
        }
    }
    if (dropped) fprintf(stderr, "omabox-keyboard: more than %d characters outside the layout; the rest cannot be typed\n", MAXWANT);
    if (!nwant) return NULL;
    char *text = xkb_keymap_get_as_string(keymap, XKB_KEYMAP_FORMAT_TEXT_V1);
    char *sec = text ? strstr(text, "xkb_symbols") : NULL, *end = sec ? strstr(sec, "\n};") : NULL;
    if (!end) { free(text); return NULL; }
    size_t used = (size_t)(end - text), cap = strlen(text) + nwant * 128 + 1;
    char *out = malloc(cap);
    if (!out) { perror("omabox-keyboard: malloc"); exit(1); }
    memcpy(out, text, used);
    size_t k = 0, high = 0;
    xkb_keycode_t min = xkb_keymap_min_keycode(keymap), max = xkb_keymap_max_keycode(keymap);
    for (int pass = 0; pass < 2; pass++)
        for (xkb_keycode_t kc = pass ? X11_MAX_KEYCODE + 1 : min; kc <= (pass ? max : (max < X11_MAX_KEYCODE ? max : X11_MAX_KEYCODE)) && k < nwant; kc++) {
            const char *kname = xkb_keymap_key_get_name(keymap, kc);
            if (!kname || xkb_keymap_num_layouts_for_key(keymap, kc) != 0) continue;
            struct hit h;   // out of low spares, a character the layout has above 255 keeps its key
            while (pass && k < nwant && find_keysym(want[k], &h)) { k++; high++; }
            if (k == nwant) break;
            char sname[64];
            if (xkb_keysym_get_name(want[k], sname, sizeof(sname)) <= 0) { k++; continue; }
            int n = snprintf(out + used, cap - used, "\n\tkey <%s> { [ %s ] };", kname, sname);
            if (n < 0 || (size_t)n >= cap - used) break;
            used += (size_t)n;
            k++;
            high += pass;
        }
    snprintf(out + used, cap - used, "%s", end);
    free(text);
    struct xkb_keymap *km = xkb_keymap_new_from_string(ctx, out, XKB_KEYMAP_FORMAT_TEXT_V1, XKB_KEYMAP_COMPILE_NO_FLAGS);
    free(out);
    if (high) fprintf(stderr, "omabox-keyboard: %zu character(s) on keycodes above 255: X11 (Xwayland) apps will not see them\n", high);
    if (k < nwant) fprintf(stderr, "omabox-keyboard: no spare keycode for %zu character(s)\n", nwant - k);
    return km;
}

// -t TEXT may hold newline and tab (Return, Tab), no other control character: xkb maps them to keys
// (0x08 BackSpace, 0x0D Return, 0x1B Escape, 0x7F Delete), so a stray one would edit, submit or close
// instead of typing. Checked for every text before anything is sent; 0 refuses the run (exit 2).
static int text_controls_ok(int argc, char **argv, int first) {
    for (int a = first; a < argc; a++) {
        if (strcmp(argv[a], "-t") || a + 1 >= argc) continue;
        const unsigned char *s = (const unsigned char *)argv[++a];
        for (const unsigned char *p = s; *p; p++)
            if ((*p < 0x20 && *p != '\n' && *p != '\t') || *p == 0x7f) {
                fprintf(stderr, "omabox-keyboard: text has a control character (U+%04X) at byte %td; only newline and tab are typed\n", *p, p - s);
                return 0;
            }
    }
    return 1;
}

static int do_text(const char *s, int run) {
    const unsigned char *p = (const unsigned char *)s;
    uint32_t cp;
    while (*p) {
        if (!decode(&p, &cp)) { fprintf(stderr, "omabox-keyboard: bad UTF-8 in text\n"); return 0; }
        struct hit h;
        if (!find_keysym((xkb_keysym_t)text_keysym(cp), &h)) {
            fprintf(stderr, "omabox-keyboard: cannot type U+%04X with this layout (no key and no spare keycode)\n", cp);
            return 0;
        }
        if (!chord(held_for(h.mask), h.code, run)) return 0;
    }
    return 1;
}

// Connects to the compositor and puts a keyboard with this keymap on its seat (kbd); 0, said, if not.
static int kbd_connect(void) {
    display = wl_display_connect(NULL);
    if (!display) { fprintf(stderr, "omabox-keyboard: cannot connect to Wayland display\n"); return 0; }
    struct wl_registry *reg = wl_display_get_registry(display);
    wl_registry_add_listener(reg, &registry_listener, NULL);
    sync_or_die();
    if (!seat || !manager) { fprintf(stderr, "omabox-keyboard: compositor lacks wl_seat or zwp_virtual_keyboard_manager_v1\n"); return 0; }

    kbd = zwp_virtual_keyboard_manager_v1_create_virtual_keyboard(manager, seat);
    char *km = xkb_keymap_get_as_string(keymap, XKB_KEYMAP_FORMAT_TEXT_V1);
    if (!km) { fprintf(stderr, "omabox-keyboard: cannot serialise the keymap\n"); return 0; }
    size_t size = strlen(km) + 1;
    int fd = memfd_create("omabox-keymap", MFD_CLOEXEC);
    if (fd < 0 || ftruncate(fd, (off_t)size) < 0) { perror("omabox-keyboard: memfd"); return 0; }
    char *map = mmap(NULL, size, PROT_WRITE, MAP_SHARED, fd, 0);
    if (map == MAP_FAILED) { perror("omabox-keyboard: mmap"); return 0; }
    memcpy(map, km, size);
    munmap(map, size);
    free(km);
    zwp_virtual_keyboard_v1_keymap(kbd, WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1, fd, (uint32_t)size);
    sync_or_die();
    close(fd);
    return 1;
}

// --- held keys (#230) -------------------------------------------------------------------------------
// down:KEY and up:KEY go to a holder: this tool forked off by the first down:, in the background (the
// box's, as `omabox keys` runs a run with them in the box's pid namespace: it ends with the box). It
// keeps a keyboard of its own on the seat, presses and releases keys on it as asked, one request a
// connection on a unix socket ("down CODE", "up CODE" -> "ok", "held", "not-held NAMES"), and ends
// once nothing is held, letting go of everything first (on SIGTERM, SIGINT and SIGHUP too). Hyprland
// joins the modifiers of every keyboard on the seat, for its binds and for the client: SUPER held
// there and `keys a` from another keyboard fire SUPER+A, shift held there types an A (NOTES).
#define HELD_MAX 32
static int is_held_tok(const char *t) { return !strncmp(t, "down:", 5) || !strncmp(t, "up:", 3); }

// The socket, and beside it the keys its holder has down (held_state: one keycode a line), so that the
// keys of a holder killed outright (SIGKILL), still down for Hyprland, can be let go once.
static char held_state[sizeof(((struct sockaddr_un *)0)->sun_path) + 8];
static int held_addr(struct sockaddr_un *a) {
    const char *rt = getenv("XDG_RUNTIME_DIR");
    memset(a, 0, sizeof(*a));
    a->sun_family = AF_UNIX;
    if (!rt || *rt != '/' || snprintf(a->sun_path, sizeof(a->sun_path), "%s/omabox-held-keys", rt) >= (int)sizeof(a->sun_path)) {
        fprintf(stderr, "omabox-keyboard: no XDG_RUNTIME_DIR for the held keys' socket\n");
        return 0;
    }
    snprintf(held_state, sizeof(held_state), "%s.keys", a->sun_path);
    return 1;
}

// down:KEY / up:KEY's key: one key, a letter its key (down:A holds a), no symbol that needs a modifier.
static int held_code(const char *tok, xkb_keycode_t *code) {
    const char *name = strchr(tok, ':') + 1, *as;
    if (!*name) { fprintf(stderr, "omabox-keyboard: %s: which key? (down:XF86Tools, down:super, down:a)\n", tok); return 0; }
    xkb_keysym_t sym = key_sym(name, &as);
    struct hit h;
    if (sym == XKB_KEY_NoSymbol && strchr(name + 1, '+')) {
        fprintf(stderr, "omabox-keyboard: %s: one key each (down:ctrl down:c), not a combo\n", tok);
        return 0;
    }
    if (sym != XKB_KEY_NoSymbol) sym = xkb_keysym_to_lower(sym);
    if (sym == XKB_KEY_NoSymbol || !find_keysym(sym, &h)) { fprintf(stderr, "omabox-keyboard: %s: no key for '%s' in this layout\n", tok, as); return 0; }
    if (h.mask) { fprintf(stderr, "omabox-keyboard: %s: '%s' takes a modifier on this layout: hold the modifier and its key, each with a down:\n", tok, as); return 0; }
    *code = h.code;
    return 1;
}

// The keysym names of the keys in codes, space-separated, in out.
static void held_names(const xkb_keycode_t *codes, int n, char *out, size_t len) {
    size_t at = 0;
    out[0] = 0;
    for (int k = 0; k < n && at < len; k++) {
        const xkb_keysym_t *syms;
        char name[64] = "?";
        if (xkb_keymap_key_get_syms_by_level(keymap, codes[k], 0, 0, &syms) > 0) xkb_keysym_get_name(syms[0], name, sizeof(name));
        int w = snprintf(out + at, len - at, "%s%s", k ? " " : "", name);
        if (w < 0) break;
        at += (size_t)w;
    }
}

// The holder (in the forked child): its socket bound (lfd); it writes a byte to `ready` once it is on
// the seat, or exits, saying why on the caller's stderr. Never returns.
static int held_ino_ok;
static ino_t held_ino;
static struct sockaddr_un held_sa;
static void held_unlink(void) {
    struct stat st;   // only its own socket (and keys): a new holder's may be there by now
    if (held_ino_ok && stat(held_sa.sun_path, &st) == 0 && st.st_ino == held_ino) { unlink(held_state); unlink(held_sa.sun_path); }
    held_ino_ok = 0;
}
// The holder's keys, in held_state (replaced whole: a reader never sees half of it).
static void held_save(const xkb_keycode_t *codes, int n) {
    char tmp[sizeof(held_state) + 8];
    snprintf(tmp, sizeof(tmp), "%s.new", held_state);
    FILE *f = fopen(tmp, "we");
    if (!f) return;
    for (int k = 0; k < n; k++) fprintf(f, "%u\n", codes[k]);
    if (fclose(f) == 0) rename(tmp, held_state); else unlink(tmp);
}
// The keycodes in a held_state file (at most HELD_MAX), how many.
static int held_read(const char *path, xkb_keycode_t *codes) {
    FILE *f = fopen(path, "re");
    if (!f) return 0;
    int n = 0;
    unsigned c;
    while (n < HELD_MAX && fscanf(f, "%u", &c) == 1)
        if (c >= xkb_keymap_min_keycode(keymap) && c <= xkb_keymap_max_keycode(keymap)) codes[n++] = c;
    fclose(f);
    return n;
}
static void push_state(struct xkb_state *st) {
    zwp_virtual_keyboard_v1_modifiers(kbd, xkb_state_serialize_mods(st, XKB_STATE_MODS_DEPRESSED),
        xkb_state_serialize_mods(st, XKB_STATE_MODS_LATCHED), xkb_state_serialize_mods(st, XKB_STATE_MODS_LOCKED),
        xkb_state_serialize_layout(st, XKB_STATE_LAYOUT_EFFECTIVE));
}
static void held_serve(int lfd, int ready) {
    setsid();
    struct sigaction sa = {0};
    sa.sa_handler = on_signal;   // no SA_RESTART: poll ends, everything is let go
    sigaction(SIGTERM, &sa, NULL);
    sigaction(SIGINT, &sa, NULL);
    sigaction(SIGHUP, &sa, NULL);
    signal(SIGPIPE, SIG_IGN);
    atexit(held_unlink);
    // The caller's connection (a run with keys of its own) is not this one's.
    if (display) close(wl_display_get_fd(display));
    display = NULL; kbd = NULL; seat = NULL; manager = NULL;
    if (!kbd_connect()) exit(1);
    struct xkb_state *st = xkb_state_new(keymap);
    if (!st) { fprintf(stderr, "omabox-keyboard: no keyboard state\n"); exit(1); }
    spent = 0;
    spend(0);
    // From here nothing of the caller's: its stdout and stderr would hold its pipes.
    int null = open("/dev/null", O_RDWR | O_CLOEXEC);
    if (null >= 0) { dup2(null, 0); dup2(null, 1); dup2(null, 2); if (null > 2) close(null); }
    if (write(ready, "1", 1) != 1) exit(1);
    close(ready);
    xkb_keycode_t held[HELD_MAX];
    int n = 0, wfd = wl_display_get_fd(display);
    for (;;) {
        if (stop) break;
        wl_display_flush(display);
        struct pollfd p[2] = {{wfd, POLLIN, 0}, {lfd, POLLIN, 0}};
        if (poll(p, 2, -1) < 0) { if (errno == EINTR) continue; break; }
        if (p[0].revents && wl_display_dispatch(display) == -1) exit(1);   // the compositor went: nothing to let go
        if (!(p[1].revents & POLLIN)) continue;
        int c = accept4(lfd, NULL, NULL, SOCK_CLOEXEC);
        if (c < 0) continue;
        struct timeval tv = {2, 0};
        setsockopt(c, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
        char req[64], out[HELD_MAX * 64 + 16];
        size_t len = 0;
        ssize_t r;
        while (len < sizeof(req) - 1 && (r = read(c, req + len, sizeof(req) - 1 - len)) > 0) {
            len += (size_t)r;
            if (memchr(req, '\n', len)) break;
        }
        req[len] = 0;
        unsigned code = 0;
        int k = -1, down = sscanf(req, "down %u", &code) == 1, up = !down && sscanf(req, "up %u", &code) == 1;
        int codes = !down && !up && !strncmp(req, "codes", 5);
        for (int j = 0; j < n; j++) if (held[j] == code) k = j;
        if (codes) {   // what is held, as keycodes: a run checks its down:/up: against it before sending anything
            size_t at = (size_t)snprintf(out, sizeof(out), "codes");
            for (int j = 0; j < n; j++) at += (size_t)snprintf(out + at, sizeof(out) - at, " %u", held[j]);
            snprintf(out + at, sizeof(out) - at, "\n");
        } else if ((down || up) && (code < xkb_keymap_min_keycode(keymap) || code > xkb_keymap_max_keycode(keymap))) {
            snprintf(out, sizeof(out), "bad\n");
        } else if (down && k >= 0) {
            snprintf(out, sizeof(out), "held\n");
        } else if (down && n == HELD_MAX) {
            snprintf(out, sizeof(out), "full\n");
        } else if (down || (up && k >= 0)) {
            if (down) { held[n++] = code; held_save(held, n); }   // saved before it is down, gone once it is up
            push_state(st);   // spent (finding 14): another keyboard may have had a turn since
            sync_or_die();
            sleep_ms(20);
            key(code, down);
            xkb_state_update_key(st, code, down ? XKB_KEY_DOWN : XKB_KEY_UP);
            push_state(st);
            sync_or_die();
            if (up) { held[k] = held[--n]; held_save(held, n); }
            if (!n) held_unlink();   // a down: from now on starts a new holder
            snprintf(out, sizeof(out), "ok\n");
        } else {
            char names[HELD_MAX * 64];
            held_names(held, n, names, sizeof(names));
            snprintf(out, sizeof(out), "%s %s\n", up ? "not-held" : "list", names);
        }
        send(c, out, strlen(out), MSG_NOSIGNAL);
        // The run that let go of the last key keeps its connection until this holder ends (exit
        // closes it): it reads to the end, so it returns once the holder is gone, keyboard and all
        // (finding 512).
        if (!n && !codes) break;
        close(c);
    }
    // Let go of whatever is still down (a signal), then of the keyboard.
    held_unlink();
    close(lfd);
    while (n > 0) {
        key(held[--n], 0);
        xkb_state_update_key(st, held[n], XKB_KEY_UP);
        push_state(st);
    }
    sync_or_die();
    sleep_ms(30);   // the compositor handles the last release before the device goes away
    zwp_virtual_keyboard_v1_destroy(kbd);
    sync_or_die();
    exit(0);
}

// Starts a holder on a socket of its own, or finds one already answering there (a stale socket, a
// holder killed outright, is taken over). 0, said, if neither.
static int held_start(void) {
    if (!held_addr(&held_sa)) return 0;
    int lfd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (lfd < 0) { perror("omabox-keyboard: socket"); return 0; }
    if (bind(lfd, (struct sockaddr *)&held_sa, sizeof(held_sa)) < 0) {
        int c = errno == EADDRINUSE ? socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0) : -1;
        if (c >= 0 && connect(c, (struct sockaddr *)&held_sa, sizeof(held_sa)) == 0) { close(c); close(lfd); return 1; }
        if (c >= 0) close(c);
        if (errno != ECONNREFUSED || unlink(held_sa.sun_path) < 0 || bind(lfd, (struct sockaddr *)&held_sa, sizeof(held_sa)) < 0) {
            fprintf(stderr, "omabox-keyboard: cannot hold keys: %s: %s\n", held_sa.sun_path, strerror(errno));
            close(lfd);
            return 0;
        }
    }
    struct stat st;
    if (listen(lfd, 8) < 0 || stat(held_sa.sun_path, &st) < 0) { perror("omabox-keyboard: listen"); close(lfd); return 0; }
    held_ino = st.st_ino;
    int pfd[2];
    if (pipe2(pfd, O_CLOEXEC) < 0) { perror("omabox-keyboard: pipe"); close(lfd); return 0; }
    fflush(stdout);
    fflush(stderr);
    pid_t pid = fork();
    if (pid == 0) {
        close(pfd[0]);
        if (fork() != 0) _exit(0);   // the holder is not this run's child: reaped by the box's init
        held_ino_ok = 1;
        held_serve(lfd, pfd[1]);
    }
    close(lfd);
    close(pfd[1]);
    if (pid < 0) { perror("omabox-keyboard: fork"); close(pfd[0]); unlink(held_sa.sun_path); return 0; }
    waitpid(pid, NULL, 0);
    // A byte once it is on the seat; the end of the pipe if it is gone (it said why).
    struct pollfd p = {pfd[0], POLLIN, 0};
    char c = 0;
    if (poll(&p, 1, 15000) <= 0 || read(pfd[0], &c, 1) != 1 || c != '1') {
        close(pfd[0]);
        fprintf(stderr, "omabox-keyboard: the key holder did not start\n");
        return 0;
    }
    close(pfd[0]);
    return 1;
}

// One request to the holder and its one-line answer: 1 answered, 0 no answer (it was ending), -1 no
// holder (errno).
static int held_ask(const char *req, char *reply, size_t len) {
    if (!held_addr(&held_sa)) exit(1);
    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (fd < 0) return -1;
    if (connect(fd, (struct sockaddr *)&held_sa, sizeof(held_sa)) < 0) { int e = errno; close(fd); errno = e; return -1; }
    struct timeval tv = {15, 0};
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    send(fd, req, strlen(req), MSG_NOSIGNAL);
    size_t at = 0;
    ssize_t r;
    while (at < len - 1 && (r = read(fd, reply + at, len - 1 - at)) > 0) at += (size_t)r;
    close(fd);
    reply[at] = 0;
    char *nl = strchr(reply, '\n');
    if (!nl) return 0;
    *nl = 0;
    return 1;
}

// A holder killed outright (SIGKILL) leaves its keys down for Hyprland, and a later press and release
// of one fired its release bind twice (finding 347). The first run to find its socket dead takes its
// keys (rename: only one run does) and releases each once, on this run's keyboard or one of its own
// for the moment, said on stderr. How many it let go (in gone).
static int held_dead(void) {
    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (fd < 0) return 0;
    int dead = connect(fd, (struct sockaddr *)&held_sa, sizeof(held_sa)) < 0 && (errno == ECONNREFUSED || errno == ENOENT);
    close(fd);
    return dead;
}
static xkb_keycode_t gone[HELD_MAX];   // the killed holder's keys this run let go: an up: of one is done
static int ngone;
static int held_stale(void) {
    char claim[sizeof(held_state) + 16];
    struct stat st;
    xkb_keycode_t *codes = gone;
    if (ngone || !getenv("XDG_RUNTIME_DIR") || !held_addr(&held_sa)) return 0;
    if (stat(held_state, &st) < 0 || !st.st_size || !held_dead()) return 0;
    snprintf(claim, sizeof(claim), "%s.%d", held_state, (int)getpid());
    if (rename(held_state, claim) < 0) return 0;   // another run took them
    int n = held_read(claim, codes);
    unlink(claim);
    if (!n) return 0;
    int own = !display;
    if (own && !kbd_connect()) exit(1);
    spent = 0;
    spend(kept_mask);
    for (int k = 0; k < n; k++) key(codes[k], 0);
    sync_or_die();
    sleep_ms(20);
    if (own) {
        zwp_virtual_keyboard_v1_destroy(kbd);
        sync_or_die();
        wl_display_disconnect(display);
        display = NULL; kbd = NULL; seat = NULL; manager = NULL;
    }
    spent = 0;
    char names[HELD_MAX * 64];
    held_names(codes, n, names, sizeof(names));
    fprintf(stderr, "omabox-keyboard: a key holder had been killed with %s down: let go\n", names);
    return ngone = n;
}

// What is held, for the check before anything is sent (tokens, run = 0): the holder's keys, or a
// killed holder's (*dead: let go before this run's down: and up:, so an up: of one is that release and
// a down: of one presses it again). 0 if the holder did not answer.
static int held_now(xkb_keycode_t *codes, int *n, int *dead) {
    char reply[HELD_MAX * 16 + 16];
    *n = 0;
    *dead = 0;
    int r = held_ask("codes\n", reply, sizeof(reply));
    if (r < 0) { *n = held_read(held_state, codes); *dead = 1; return 1; }
    if (r == 0 || strncmp(reply, "codes", 5)) return 0;
    char *p = reply + 5, *end;
    while (*n < HELD_MAX) {
        unsigned long c = strtoul(p, &end, 10);
        if (end == p) break;
        codes[(*n)++] = (xkb_keycode_t)c;
        p = end;
    }
    return 1;
}

// down:/up:, checked against what is held and the run's earlier down:/up: (so `keys a up:super` with
// nothing held types nothing). The holder's own answer still decides when the run sends them.
static xkb_keycode_t sim[HELD_MAX];
static int nsim = -1, simdead;
static unsigned simlive;   // bit k: sim[k] is down by a live holder or this run (else a killed one's)
static int held_check(const char *tok, xkb_keycode_t code) {
    int down = tok[0] == 'd', k = -1;
    if (nsim < 0) {
        if (!held_now(sim, &nsim, &simdead)) { nsim = -1; return 1; }
        simlive = simdead ? 0 : ~0u;
    }
    for (int j = 0; j < nsim; j++) if (sim[j] == code) k = j;
    const char *name = tok + (down ? 5 : 3);
    if (down && k >= 0 && !(simlive & (1u << k))) { simlive |= 1u << k; return 1; }
    if (down && k >= 0) { fprintf(stderr, "omabox-keyboard: %s: held already (up:%s lets go)\n", tok, name); return 0; }
    if (down && nsim == HELD_MAX) { fprintf(stderr, "omabox-keyboard: %s: %d keys held already\n", tok, HELD_MAX); return 0; }
    if (!down && k < 0 && !nsim) { fprintf(stderr, "omabox-keyboard: %s: no key is held\n", tok); return 0; }
    if (!down && k < 0) {
        char names[HELD_MAX * 64];
        held_names(sim, nsim, names, sizeof(names));
        fprintf(stderr, "omabox-keyboard: %s: not held (held: %s)\n", tok, names);
        return 0;
    }
    if (down) { simlive |= 1u << nsim; sim[nsim++] = code; }
    else {
        nsim--;
        sim[k] = sim[nsim];
        simlive = (simlive & ~(1u << k)) | (((simlive >> nsim) & 1u) << k);
    }
    return 1;
}

static int do_held(const char *tok, int run) {
    xkb_keycode_t code;
    if (!held_code(tok, &code)) return 0;
    if (!run) return held_check(tok, code);
    int down = tok[0] == 'd';
    char req[32], reply[HELD_MAX * 64 + 16];
    snprintf(req, sizeof(req), "%s %u\n", down ? "down" : "up", code);
    int r = -1;
    for (int tries = 0; tries < 3; tries++) {
        r = held_ask(req, reply, sizeof(reply));
        if (r > 0) break;
        int dead = r < 0 && (errno == ENOENT || errno == ECONNREFUSED);
        if (dead) held_stale();
        if (r < 0 && !down) break;
        if (dead && !held_start()) return 0;
    }
    spent = 0;   // the holder's keyboard had a turn: this run's next key spends one again
    for (int j = 0; j < ngone; j++) {
        if (gone[j] != code) continue;
        gone[j] = gone[--ngone];   // let go already; a down: of it (pressed again) leaves no up: to spare
        if (r < 0 && !down) return 1;   // a killed holder's key: its release was this up:
        break;
    }
    if (r < 0 && !down) {
        fprintf(stderr, "omabox-keyboard: %s: no key is held\n", tok);
        return 0;
    }
    if (r <= 0) { fprintf(stderr, "omabox-keyboard: %s: the key holder did not answer\n", tok); return 0; }
    if (!strcmp(reply, "ok")) return 1;
    const char *k = tok + (down ? 5 : 3);
    if (!strcmp(reply, "held")) fprintf(stderr, "omabox-keyboard: %s: held already (up:%s lets go)\n", tok, k);
    else if (!strncmp(reply, "not-held ", 9)) fprintf(stderr, "omabox-keyboard: %s: not held (held: %s)\n", tok, reply + 9);
    else if (!strcmp(reply, "full")) fprintf(stderr, "omabox-keyboard: %s: %d keys held already\n", tok, HELD_MAX);
    else fprintf(stderr, "omabox-keyboard: %s: the key holder said '%s'\n", tok, reply);
    return 0;
}

// Whether a run is down:, up: (one at least) and -s alone: nothing to type on a keyboard of its own.
// (A run of -s alone still puts one on the seat: its first event clears modifiers left down, finding
// 112.)
static int held_only(int argc, char **argv, int i) {
    int any = 0;
    for (; i < argc; i++) {
        if (!strcmp(argv[i], "-s")) i++;
        else if (is_held_tok(argv[i])) any = 1;
        else return 0;
    }
    return any;
}

// One pass over the tokens: with run = 0 it only checks them (before connecting, so a bad token never
// leaves half a sequence typed); with run = 1 it sends them.
static int tokens(int argc, char **argv, int i, int run) {
    for (; i < argc; i++) {
        if (!strcmp(argv[i], "-t")) {
            if (i + 1 >= argc) usage();
            if (!do_text(argv[++i], run)) return 0;
        } else if (!strcmp(argv[i], "-s")) {
            if (i + 1 >= argc) usage();
            long ms = num(argv[++i], 0, 600000);
            if (run) { if (display) sync_or_die(); sleep_ms(ms); }
        } else if (!strcmp(argv[i], "-m")) {
            if (i + 1 >= argc) usage();
            unsigned held = parse_mods(argv[++i]);
            if (!held || !mod_keys(held, kept_code)) return 0;
            if (run) hold_mods(held);
        } else if (!strcmp(argv[i], "-p")) {
            if (i + 1 >= argc) usage();
            long ms = num(argv[++i], 1, 600000);
            if (run) { sync_or_die(); if (!pause_for(ms)) return 0; }
        } else if (is_held_tok(argv[i])) {
            if (!do_held(argv[i], run)) return 0;
        } else if (!do_combo(argv[i], run)) return 0;
        if (run && stop) return 0;
    }
    return 1;
}

// Each -T becomes -t with the next NUL-terminated text from stdin (omabox keys --pass): a new argv in
// memory, so /proc/PID/cmdline never shows it. All of stdin is read before anything is checked or typed.
#define MAX_STDIN (1 << 20)
static int takes_value(const char *t) { return !strcmp(t, "-t") || !strcmp(t, "-s") || !strcmp(t, "-m") || !strcmp(t, "-p"); }
static char **stdin_texts(int *argc, char **argv, int first) {
    int want = 0;
    for (int a = first; a < *argc; a++) {
        if (takes_value(argv[a]) && a + 1 < *argc) a++;
        else if (!strcmp(argv[a], "-T")) want++;
    }
    if (!want) return argv;
    char *buf = malloc(MAX_STDIN + 1);
    char **out = malloc(sizeof(char *) * (size_t)(*argc + want + 1));
    if (!buf || !out) { perror("omabox-keyboard: malloc"); exit(1); }
    size_t len = 0;
    ssize_t n;
    while (len < MAX_STDIN && (n = read(0, buf + len, MAX_STDIN - len)) != 0) {
        if (n < 0) { perror("omabox-keyboard: stdin"); exit(1); }
        len += (size_t)n;
    }
    if (len == MAX_STDIN) { fprintf(stderr, "omabox-keyboard: -T: more than %d bytes on stdin\n", MAX_STDIN); exit(2); }
    buf[len] = 0;
    size_t at = 0;
    int o = 0;
    for (int a = 0; a < *argc; a++) {
        if (a >= first && takes_value(argv[a]) && a + 1 < *argc) {
            out[o++] = argv[a++];
        } else if (a >= first && !strcmp(argv[a], "-T")) {
            char *nul = at < len ? memchr(buf + at, 0, len - at) : NULL;
            if (!nul) { fprintf(stderr, "omabox-keyboard: -T: no NUL-terminated text left on stdin\n"); exit(2); }
            // A value pasted with a Windows line ending: its \r is not part of the secret.
            if (nul > buf + at && nul[-1] == '\r') nul[-1] = 0;
            out[o++] = "-t";
            out[o++] = buf + at;
            at = (size_t)(nul - buf) + 1;
            continue;
        }
        out[o++] = argv[a];
    }
    out[o] = NULL;
    *argc = o;
    return out;
}

int main(int argc, char **argv) {
    // Only ever inside a box (omabox runs it there, in the box's mount namespace): run from a host
    // shell, WAYLAND_DISPLAY is the user's real desktop (it happened once: a parse check clicked it).
    if (!omabox_inside_box()) {
        fprintf(stderr, "%s: only runs inside an omabox box (use omabox keys/click/pointer)\n", argv[0]);
        return 2;
    }
    unsetenv("WAYLAND_SOCKET");   // only the socket in WAYLAND_DISPLAY, never an inherited connection
    // The box's keyboard settings come as options (omabox keys reads them from its Hyprland); the
    // caller's XKB_DEFAULT_* must not change what the keycodes mean.
    struct xkb_rule_names names = {.rules = "evdev", .layout = "us", .variant = "", .model = "", .options = ""};
    int i = 1, hold = 0;
    for (; i < argc; i++) {
        const char **opt = !strcmp(argv[i], "--layout") ? &names.layout : !strcmp(argv[i], "--variant") ? &names.variant
                         : !strcmp(argv[i], "--model") ? &names.model : !strcmp(argv[i], "--options") ? &names.options : NULL;
        if (!strcmp(argv[i], "--hold")) hold = 1;
        else if (opt) { if (i + 1 >= argc) usage(); *opt = argv[++i]; }
        else if (!strcmp(argv[i], "--delay")) { if (i + 1 >= argc) usage(); delay_ms = (unsigned)num(argv[++i], 0, 10000); }
        else break;
    }
    if (hold ? i < argc : i >= argc) usage();
    // -p waits on stdin and -T reads secrets from it: not both. A run that holds modifiers (-m) lets go
    // of them on a signal too, and a closed stdout (SIGPIPE) must not end it with them down.
    int holds = 0, pauses = 0, secrets = 0;
    for (int a = i; a < argc; a++) {
        if (takes_value(argv[a])) { holds |= !strcmp(argv[a], "-m"); pauses |= !strcmp(argv[a], "-p"); a++; }
        else secrets |= !strcmp(argv[a], "-T");
    }
    if (pauses && secrets) { fprintf(stderr, "omabox-keyboard: -p and -T both read stdin: not both\n"); return 2; }
    if (holds) {
        struct sigaction sa = {0};
        sa.sa_handler = on_signal;   // no SA_RESTART: a pause's poll or a sleep ends early
        sigaction(SIGTERM, &sa, NULL);
        sigaction(SIGINT, &sa, NULL);
        sigaction(SIGHUP, &sa, NULL);
        signal(SIGPIPE, SIG_IGN);
    }
    if (!hold) argv = stdin_texts(&argc, argv, i);

    if (!hold && !text_controls_ok(argc, argv, i)) return 2;

    struct xkb_context *ctx = xkb_context_new(XKB_CONTEXT_NO_ENVIRONMENT_NAMES);
    keymap = ctx ? xkb_keymap_new_from_names(ctx, &names, XKB_KEYMAP_COMPILE_NO_FLAGS) : NULL;
    if (!keymap) { fprintf(stderr, "omabox-keyboard: cannot build keymap for layout '%s'\n", names.layout); return 1; }
    for (size_t m = 0; m < NMODS; m++) pressable |= mod_mask((int)m);
    struct xkb_keymap *spares = hold ? NULL : with_spares(ctx, argc, argv, i);
    if (spares) { xkb_keymap_unref(keymap); keymap = spares; }
    if (!tokens(argc, argv, i, 0)) return 1;

    // A run of down:, up: and -s alone types nothing here: no keyboard of its own on the seat.
    if (!hold && held_only(argc, argv, i)) return tokens(argc, argv, i, 1) ? 0 : 1;
    if (!kbd_connect()) return 1;
    // A box has no keyboard of its own. With none on the seat, Hyprland drops keyboard focus changes
    // ("setKeyboardFocus without a valid keyboard set"), so a device that comes later finds a shell
    // panel without focus and its keys go nowhere. An idle one held for the box's life prevents it.
    if (hold) { while (wl_display_dispatch(display) != -1) {} return 0; }
    // Its first event spent before the first key (finding 14); also what clears modifiers a killed
    // run left down (finding 112), in a run that types nothing.
    spend(0);
    held_stale();   // a killed holder's keys, let go before this run's (finding 347)
    int ok = tokens(argc, argv, i, 1);
    release_held();
    if (stop) { fprintf(stderr, "omabox-keyboard: stopped by signal %d; modifiers released\n", (int)stop); ok = 0; }
    sync_or_die();
    sleep_ms(30); // let the compositor handle the last release before the device goes away
    zwp_virtual_keyboard_v1_destroy(kbd);
    sync_or_die();
    wl_display_disconnect(display);
    return ok ? 0 : 1;
}

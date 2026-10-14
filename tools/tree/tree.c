// omabox-tree: one window's accessibility (AT-SPI) tree as compact text, for `omabox tree` (#178).
//
//   omabox-tree --pid PID [--title TITLE] [--size WxH] [--find TEXT] [--json] [--all]
//
// Run by `omabox tree` inside the box (box_exec: the box's session bus and pid numbering), never on the
// host, where it would read the real desktop's apps: it refuses outside a box. PID is the window's
// process as the box's Hyprland has it, which is the AT-SPI application's (NOTES finding 244); TITLE
// picks that application's window (its frame's name) when it has several; WxH is the window's size,
// and what lies wholly outside it (a long page scrolled out) is left out.
//
// A line is `role "name" 'text' =value [states] @x,y wxh`, indented two spaces a level; the bounds are
// in window coordinates, what `omabox click --window` takes. Anonymous containers, hidden nodes, a
// control's own label and wrappers repeating their parent's name are left out (--all keeps them).
// --find TEXT prints only the lines holding TEXT (any case; not in the bounds), each after its
// printed ancestors: `list "Options" > check box "Dark mode" [checked] @12,40 140x24`.
//
// Before reading, it sets org.a11y.Status.IsEnabled (what a screen reader sets): Qt apps export their
// tree only then, and follow it live; GTK4 exports always. One that just turned it on waits up to 3 s
// for PID's application to register again.
//
// Exit: 0 printed; 1 --find matched nothing; 3 no tree for that window (said why); 4 the box has no
// accessibility bus; 2 usage, or outside a box.
#define _GNU_SOURCE
#include <atspi/atspi.h>
#include <gio/gio.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <time.h>

#include "../common/box.h"

#define BUDGET 4000      // nodes visited at most (a long page is tens of thousands)
#define MAX_CHILDREN 500 // children of one node read at most
#define MAX_DEPTH 64

static int opt_all, opt_json, win_w = -1, win_h = -1;
static const char *find;
static int nodes, lines, matched, truncated;
static GString *out;

// The printed ancestors of the node being read, for --find's path.
static char *path[MAX_DEPTH];
static int path_n;

static const struct { AtspiStateType st; const char *word; } SHOWN[] = {
    {ATSPI_STATE_FOCUSED, "focused"},   {ATSPI_STATE_CHECKED, "checked"},
    {ATSPI_STATE_SELECTED, "selected"}, {ATSPI_STATE_PRESSED, "pressed"},
    {ATSPI_STATE_EXPANDED, "expanded"}, {ATSPI_STATE_INDETERMINATE, "mixed"},
    {ATSPI_STATE_EDITABLE, "editable"}, {ATSPI_STATE_READ_ONLY, "readonly"},
    {ATSPI_STATE_MODAL, "modal"},       {ATSPI_STATE_ACTIVE, "active"},
    {ATSPI_STATE_BUSY, "busy"},         {ATSPI_STATE_INVALID_ENTRY, "invalid"},
};
static const char *CONTAINERS[] = {"filler", "panel", "section", "unknown", "grouping", "scroll pane", "viewport",
    "layered pane", "split pane", "redundant object", "invalid", "internal frame", "block quote", "static",
    "landmark", "form", "paragraph", NULL};
static const char *TEXT_ROLES[] = {"text", "entry", "password text", "terminal", "label", "static", "paragraph",
    "heading", "document text", "editbar", NULL};
static const char *CHECKABLE[] = {"check box", "radio button", "toggle button", "check menu item",
    "radio menu item", "switch", NULL};

static int in(const char *s, const char **set) {
    for (; *set; set++)
        if (strcmp(s, *set) == 0) return 1;
    return 0;
}

// One line of text: U+FFFC (an embedded child) and private-use glyphs (icon fonts) out, runs of
// white space as one space, at most n characters.
static char *clean(const char *s, glong n) {
    GString *g = g_string_new(NULL);
    int space = 0;
    for (const char *p = s ? s : ""; *p; p = g_utf8_next_char(p)) {
        gunichar c = g_utf8_get_char_validated(p, -1);
        if (c == (gunichar)-1 || c == (gunichar)-2) break;
        if (c == 0xFFFC || (c >= 0xE000 && c <= 0xF8FF)) continue;
        if (g_unichar_isspace(c)) { space = g->len > 0; continue; }
        if (space) g_string_append_c(g, ' ');
        space = 0;
        g_string_append_unichar(g, c);
    }
    if (g_utf8_strlen(g->str, -1) > n) {
        char *end = g_utf8_offset_to_pointer(g->str, n - 1);
        g_string_truncate(g, end - g->str);
        g_string_append(g, "…");
    }
    return g_string_free(g, FALSE);
}

static void json_str(GString *g, const char *s) {
    g_string_append_c(g, '"');
    for (const unsigned char *p = (const unsigned char *)s; *p; p++) {
        if (*p == '"' || *p == '\\') g_string_append_printf(g, "\\%c", *p);
        else if (*p < 0x20) g_string_append_printf(g, "\\u%04x", *p);
        else g_string_append_c(g, *p);
    }
    g_string_append_c(g, '"');
}

static char *role_of(AtspiAccessible *a) {
    char *r = atspi_accessible_get_role_name(a, NULL);
    return r ? r : g_strdup("?");
}
static char *name_of(AtspiAccessible *a, glong n) {
    char *raw = atspi_accessible_get_name(a, NULL), *s = clean(raw, n);
    g_free(raw);
    return s;
}

// The text a shot would be read for, when the name does not carry it.
static char *text_of(AtspiAccessible *a, const char *role) {
    if (!in(role, TEXT_ROLES)) return g_strdup("");
    AtspiText *t = atspi_accessible_get_text_iface(a);
    if (!t) return g_strdup("");
    gint n = atspi_text_get_character_count(t, NULL);
    char *raw = n > 0 ? atspi_text_get_text(t, 0, MIN(n, 200), NULL) : NULL, *s = clean(raw, 80);
    g_free(raw);
    g_object_unref(t);
    return s;
}

static char *value_of(AtspiAccessible *a) {
    AtspiValue *v = atspi_accessible_get_value_iface(a);
    if (!v) return NULL;
    GError *e = NULL;
    char *txt = atspi_value_get_text(v, NULL), *r = NULL;
    if (txt && *txt) r = clean(txt, 40);
    else {
        gdouble cur = atspi_value_get_current_value(v, &e);
        if (!e) r = g_strdup_printf("%g", cur);
    }
    g_clear_error(&e);
    g_free(txt);
    g_object_unref(v);
    return r;
}

// A closed combo box's choice: its (hidden) menu's selected item, as Chromium has it.
static char *combo_choice(AtspiAccessible *a) {
    gint n = atspi_accessible_get_child_count(a, NULL);
    for (int i = 0; i < MIN(n, 4); i++) {
        AtspiAccessible *menu = atspi_accessible_get_child_at_index(a, i, NULL);
        if (!menu) continue;
        gint m = atspi_accessible_get_child_count(menu, NULL);
        for (int j = 0; j < MIN(m, 200); j++) {
            AtspiAccessible *it = atspi_accessible_get_child_at_index(menu, j, NULL);
            if (!it) continue;
            AtspiStateSet *ss = atspi_accessible_get_state_set(it);
            int sel = ss && atspi_state_set_contains(ss, ATSPI_STATE_SELECTED);
            if (ss) g_object_unref(ss);
            if (sel) {
                char *s = name_of(it, 40);
                g_object_unref(it);
                g_object_unref(menu);
                if (*s) return s;
                g_free(s);
                return NULL;
            }
            g_object_unref(it);
        }
        g_object_unref(menu);
    }
    return NULL;
}

static int extents(AtspiAccessible *a, int b[4]) {
    AtspiComponent *c = atspi_accessible_get_component_iface(a);
    if (!c) return 0;
    GError *e = NULL;
    AtspiRect *r = atspi_component_get_extents(c, ATSPI_COORD_TYPE_WINDOW, &e);
    g_object_unref(c);
    if (e || !r) { g_clear_error(&e); g_free(r); return 0; }
    b[0] = r->x, b[1] = r->y, b[2] = r->width, b[3] = r->height;
    g_free(r);
    return 1;
}

// Whether TEXT is in s, any case.
static int holds(const char *s, const char *text) {
    char *a = g_utf8_casefold(s, -1), *b = g_utf8_casefold(text, -1);
    int r = strstr(a, b) != NULL;
    g_free(a);
    g_free(b);
    return r;
}

static void emit(int depth, const char *role, const char *name, const char *text, const char *val,
                 GPtrArray *states, int *b) {
    GString *body = g_string_new(role);
    if (*name) g_string_append_printf(body, " \"%s\"", name);
    if (*text) g_string_append_printf(body, " '%s'", text);
    if (val) g_string_append_printf(body, " =%s", val);
    if (states->len) {
        g_string_append(body, " [");
        for (guint i = 0; i < states->len; i++) g_string_append_printf(body, "%s%s", i ? " " : "", (char *)states->pdata[i]);
        g_string_append_c(body, ']');
    }
    lines++;
    int hit = !find || holds(body->str, find);
    if (hit) {
        matched++;
        if (opt_json) {
            g_string_append(out, out->len ? ",\n " : "[");
            g_string_append_printf(out, "{\"depth\": %d, \"role\": ", depth);
            json_str(out, role);
            g_string_append(out, ", \"name\": ");
            json_str(out, name);
            if (*text) { g_string_append(out, ", \"text\": "); json_str(out, text); }
            if (val) { g_string_append(out, ", \"value\": "); json_str(out, val); }
            g_string_append(out, ", \"states\": [");
            for (guint i = 0; i < states->len; i++) {
                if (i) g_string_append(out, ", ");
                json_str(out, states->pdata[i]);
            }
            g_string_append_c(out, ']');
            if (b) g_string_append_printf(out, ", \"x\": %d, \"y\": %d, \"w\": %d, \"h\": %d", b[0], b[1], b[2], b[3]);
            if (find) {
                g_string_append(out, ", \"path\": [");
                for (int i = 1; i < path_n; i++) {
                    if (i > 1) g_string_append(out, ", ");
                    json_str(out, path[i]);
                }
                g_string_append_c(out, ']');
            }
            g_string_append_c(out, '}');
        } else {
            if (find) {
                // The window itself (path[0]) is the one asked for: not repeated on every line.
                for (int i = 1; i < path_n; i++) g_string_append_printf(out, "%s > ", path[i]);
            } else {
                for (int i = 0; i < depth; i++) g_string_append(out, "  ");
            }
            g_string_append(out, body->str);
            if (b) g_string_append_printf(out, " @%d,%d %dx%d", b[0], b[1], b[2], b[3]);
            g_string_append_c(out, '\n');
        }
    }
    g_string_free(body, TRUE);
}

static void walk(AtspiAccessible *a, int depth, const char *parent_name) {
    if (nodes >= BUDGET) { truncated = 1; return; }
    nodes++;
    char *role = role_of(a), *name = name_of(a, 80);
    AtspiStateSet *ss = atspi_accessible_get_state_set(a);
#define HAS(st) (ss && atspi_state_set_contains(ss, (st)))
    int b[4], hasb = extents(a, b) && b[2] > 0 && b[3] > 0;
    char *text = NULL, *val = NULL;
    GPtrArray *states = g_ptr_array_new();
    gint n = 0;
    int printed = 0, skip = 0;

    if (ss && !HAS(ATSPI_STATE_SHOWING) && !HAS(ATSPI_STATE_VISIBLE) && depth > 0 && !opt_all) skip = 1;
    // Wholly outside the window (scrolled out of a long page): a shot would not show it either.
    else if (depth > 0 && hasb && win_w > 0 && !opt_all &&
             (b[0] >= win_w || b[1] >= win_h || b[0] + b[2] <= 0 || b[1] + b[3] <= 0)) {
        skip = 1;
    }
    if (skip) goto done;

    val = value_of(a);
    if (!val && strcmp(role, "combo box") == 0) val = combo_choice(a);
    if (val && strcmp(val, name) == 0) g_clear_pointer(&val, g_free);
    text = text_of(a, role);
    if (strcmp(text, name) == 0) *text = 0;
    for (size_t i = 0; i < G_N_ELEMENTS(SHOWN); i++)
        if (HAS(SHOWN[i].st)) g_ptr_array_add(states, (gpointer)SHOWN[i].word);
    if (HAS(ATSPI_STATE_CHECKABLE) && !HAS(ATSPI_STATE_CHECKED) && in(role, CHECKABLE))
        g_ptr_array_add(states, "unchecked");
    if (ss && !HAS(ATSPI_STATE_SENSITIVE) && !HAS(ATSPI_STATE_ENABLED)) g_ptr_array_add(states, "disabled");
    n = atspi_accessible_get_child_count(a, NULL);
    if (n < 0) n = 0;

    if (!opt_all && !*name && !*text && n == 1 &&
        (!strcmp(role, "list item") || !strcmp(role, "table cell") || !strcmp(role, "menu item"))) {
        // An unnamed row whose one child is its label: named after it.
        AtspiAccessible *ch = atspi_accessible_get_child_at_index(a, 0, NULL);
        if (ch) {
            char *cr = role_of(ch);
            if (!strcmp(cr, "label") && atspi_accessible_get_child_count(ch, NULL) == 0) {
                g_free(name);
                name = name_of(ch, 80);
                n = 0;
            }
            g_free(cr);
            g_object_unref(ch);
        }
    }
    int anon = !*name && !val && !*text && states->len == 0;
    printed = opt_all || depth == 0 || !(anon && in(role, CONTAINERS));
    if (printed && !opt_all && depth > 0 && *name && !strcmp(name, parent_name) && !val && !*text &&
        states->len == 0 && n)
        printed = 0; // a wrapper repeating its parent's name (an icon's section, a button's)
    if (!opt_all && (!strcmp(role, "label") || !strcmp(role, "static")) && *name && !n) {
        // A button's own label, a paragraph's runs of text: the parent already says it.
        char *bare = g_strdup(name);
        size_t l = strlen(bare);
        if (l >= 3 && !strcmp(bare + l - 3, "…")) bare[l - 3] = 0;
        int dup = !strcmp(name, parent_name) || strstr(parent_name, bare) != NULL;
        g_free(bare);
        if (dup && *parent_name) goto done;
    }

    if (printed) {
        emit(path_n, role, name, text, val, states, hasb ? b : NULL);
        if (path_n < MAX_DEPTH) path[path_n++] = *name ? g_strdup_printf("%s \"%s\"", role, name) : g_strdup(role);
    }
    for (int i = 0; i < MIN(n, MAX_CHILDREN) && nodes < BUDGET; i++) {
        AtspiAccessible *ch = atspi_accessible_get_child_at_index(a, i, NULL);
        if (!ch) continue;
        walk(ch, depth + 1, printed ? (*name ? name : text) : parent_name);
        g_object_unref(ch);
    }
    if (n > MAX_CHILDREN) truncated = 1;
    if (printed && path_n > 0) g_free(path[--path_n]);
done:
#undef HAS
    g_ptr_array_free(states, TRUE);
    if (ss) g_object_unref(ss);
    g_free(role);
    g_free(name);
    g_free(text);
    g_free(val);
}

// The box's a11y bus answers (at-spi-bus-launcher on its session bus); and IsEnabled set true.
// Returns -1 without a bus, else whether IsEnabled was off (and is on now).
static int a11y_bus(void) {
    GError *e = NULL;
    GDBusConnection *c = g_bus_get_sync(G_BUS_TYPE_SESSION, NULL, &e);
    if (!c) {
        fprintf(stderr, "omabox-tree: no session bus (%s)\n", e->message);
        g_clear_error(&e);
        return -1;
    }
    GVariant *r = g_dbus_connection_call_sync(c, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus", "GetAddress", NULL,
                                              G_VARIANT_TYPE("(s)"), G_DBUS_CALL_FLAGS_NONE, 5000, NULL, &e);
    if (!r) {
        fprintf(stderr, "omabox-tree: the box has no accessibility bus (%s)\n", e->message);
        g_clear_error(&e);
        g_object_unref(c);
        return -1;
    }
    const char *addr;
    g_variant_get(r, "(&s)", &addr);
    int none = !*addr;
    g_variant_unref(r);
    if (none) {
        fprintf(stderr, "omabox-tree: the box has no accessibility bus (an empty address)\n");
        g_object_unref(c);
        return -1;
    }
    int was_off = 1;
    r = g_dbus_connection_call_sync(c, "org.a11y.Bus", "/org/a11y/bus", "org.freedesktop.DBus.Properties", "Get",
                                    g_variant_new("(ss)", "org.a11y.Status", "IsEnabled"), G_VARIANT_TYPE("(v)"),
                                    G_DBUS_CALL_FLAGS_NONE, 5000, NULL, NULL);
    if (r) {
        GVariant *v;
        g_variant_get(r, "(v)", &v);
        if (g_variant_is_of_type(v, G_VARIANT_TYPE_BOOLEAN) && g_variant_get_boolean(v)) was_off = 0;
        g_variant_unref(v);
        g_variant_unref(r);
    }
    if (was_off) {
        r = g_dbus_connection_call_sync(c, "org.a11y.Bus", "/org/a11y/bus", "org.freedesktop.DBus.Properties", "Set",
                                        g_variant_new("(ssv)", "org.a11y.Status", "IsEnabled", g_variant_new_boolean(TRUE)),
                                        NULL, G_DBUS_CALL_FLAGS_NONE, 5000, NULL, &e);
        if (!r) {
            fprintf(stderr, "omabox-tree: could not set org.a11y.Status.IsEnabled (%s): Qt apps will have no tree\n", e->message);
            g_clear_error(&e);
            was_off = 0;
        } else g_variant_unref(r);
    }
    g_object_unref(c);
    return was_off;
}

// PID's applications on the a11y bus: those with something under them (Qt leaves the application it
// registered before IsEnabled, empty, beside the one it registers after). *any: one with PID at all.
static GPtrArray *apps_of(guint pid, int *any, char **toolkit) {
    GPtrArray *found = g_ptr_array_new_with_free_func(g_object_unref);
    AtspiAccessible *desk = atspi_get_desktop(0);
    gint n = atspi_accessible_get_child_count(desk, NULL);
    for (int i = 0; i < n; i++) {
        AtspiAccessible *app = atspi_accessible_get_child_at_index(desk, i, NULL);
        if (!app) continue;
        GError *e = NULL;
        guint p = atspi_accessible_get_process_id(app, &e);
        if (!e && p == pid) {
            *any = 1;
            if (toolkit && !*toolkit) *toolkit = atspi_accessible_get_toolkit_name(app, NULL);
            if (atspi_accessible_get_child_count(app, NULL) > 0) { g_ptr_array_add(found, app); app = NULL; }
        }
        g_clear_error(&e);
        if (app) g_object_unref(app);
    }
    g_object_unref(desk);
    return found;
}

static double now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec + ts.tv_nsec / 1e9;
}

int main(int argc, char **argv) {
    if (!omabox_inside_box()) {
        fprintf(stderr, "omabox-tree: only runs inside an omabox box\n");
        return 2;
    }
    long pid = -1;
    const char *title = NULL;
    for (int i = 1; i < argc; i++) {
        const char *a = argv[i], *v = i + 1 < argc ? argv[i + 1] : NULL;
        if (!strcmp(a, "--all")) opt_all = 1;
        else if (!strcmp(a, "--json")) opt_json = 1;
        else if (!strcmp(a, "--pid") && v) pid = strtol(argv[++i], NULL, 10);
        else if (!strcmp(a, "--title") && v) title = argv[++i];
        else if (!strcmp(a, "--find") && v) find = argv[++i];
        else if (!strcmp(a, "--size") && v) {
            if (sscanf(argv[++i], "%dx%d", &win_w, &win_h) != 2) win_w = win_h = -1;
        } else {
            fprintf(stderr, "usage: omabox-tree --pid PID [--title TITLE] [--size WxH] [--find TEXT] [--json] [--all]\n");
            return 2;
        }
    }
    if (pid <= 0) {
        fprintf(stderr, "omabox-tree: --pid PID is needed\n");
        return 2;
    }
    if (find && !*find) find = NULL;

    int was_off = a11y_bus();
    if (was_off < 0) return 4;
    if (atspi_init() < 0) {
        fprintf(stderr, "omabox-tree: could not reach the accessibility bus\n");
        return 4;
    }
    atspi_set_timeout(2000, 4000);

    int any = 0;
    char *toolkit = NULL;
    GPtrArray *apps = apps_of(pid, &any, &toolkit);
    // Turned on just now: a Qt app registers again (with its tree) a moment later.
    for (double end = now() + 3; was_off && apps->len == 0 && now() < end;) {
        g_ptr_array_unref(apps);
        for (double t = now() + 0.1; now() < t;) {
            if (!g_main_context_iteration(NULL, FALSE)) g_usleep(10000);
        }
        apps = apps_of(pid, &any, toolkit ? NULL : &toolkit);
    }
    if (apps->len == 0) {
        if (any) fprintf(stderr, "no accessibility tree: its application (pid %ld) is on the bus with no window under it\n", pid);
        else fprintf(stderr, "no accessibility tree: pid %ld is not on the box's accessibility bus\n", pid);
        return 3;
    }

    // The window: the frame named TITLE; else one whose name and TITLE hold each other; else the only one.
    AtspiAccessible *win = NULL, *loose = NULL;
    int frames = 0, loose_n = 0;
    GString *names = g_string_new(NULL);
    for (guint i = 0; i < apps->len && !win; i++) {
        AtspiAccessible *app = apps->pdata[i];
        gint n = atspi_accessible_get_child_count(app, NULL);
        for (int j = 0; j < MIN(n, 200) && !win; j++) {
            AtspiAccessible *f = atspi_accessible_get_child_at_index(app, j, NULL);
            if (!f) continue;
            char *raw = atspi_accessible_get_name(f, NULL), *nm = raw ? raw : g_strdup("");
            frames++;
            g_string_append_printf(names, "%s\"%s\"", names->len ? ", " : "", nm);
            if (title && !strcmp(nm, title)) win = g_object_ref(f);
            else if (title && *nm && (strstr(title, nm) || strstr(nm, title))) {
                if (!loose) loose = g_object_ref(f);
                loose_n++;
            }
            if (!loose && !title) loose = g_object_ref(f), loose_n = 1;
            g_free(nm);
            g_object_unref(f);
        }
    }
    if (!win && loose_n == 1) win = g_object_ref(loose);
    if (!win && frames == 1 && apps->len == 1) win = atspi_accessible_get_child_at_index(apps->pdata[0], 0, NULL);
    if (!win) {
        fprintf(stderr, "no accessibility tree for the window \"%s\": its application's windows are %s\n", title ? title : "",
               names->str);
        return 3;
    }

    out = g_string_new(NULL);
    walk(win, 0, "");
    if (opt_json) g_string_append(out, out->len ? "]\n" : "[]\n");
    fputs(out->str, stdout);
    // Chromium and Electron export the page only when started with --force-renderer-accessibility.
    if (toolkit && !strcasecmp(toolkit, "Chromium") && lines <= 3)
        fprintf(stderr, "omabox-tree: a Chromium or Electron window shows its page in the tree only when "
                        "started with --force-renderer-accessibility\n");
    if (truncated) fprintf(stderr, "omabox-tree: stopped at %d nodes (or %d children of one)\n", BUDGET, MAX_CHILDREN);
    if (find && !matched) {
        fprintf(stderr, "omabox-tree: nothing in the window holds \"%s\" (%d lines)\n", find, lines);
        return 1;
    }
    return 0;
}

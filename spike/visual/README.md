# Visual checks: how much changes when nothing does (#252)

Measured 2026-10-16 (omabox 0.7.0 at 19dfe42, Hyprland 0.56.2, Omarchy's stock bar on tokyo-night,
1920x1080, NVIDIA RTX 5070 Ti; the AMD iGPU afterwards, below), before building anything. The subject: a Qt Quick app with a bar widget of
its own (a Quickshell plugin), so one half has an accessibility tree and the other has none, with
a server of the box's own holding fixed data (nine items, nothing running).

- `clean-box.sh NAME [UP OPTIONS]`: the clean profile from what omabox has today. Stock bar, one
  theme, a solid `#ff00ff` sentinel wallpaper, the clock frozen with a literal Qt format
  (`'Friday 12:00'`: it keeps its place and width), `--no-pointer`, `--net isolated`, animations
  off (`hl.config`).
- `scenario.sh NAME OUT APP_CLASS APP_CMD WIDGET_ID`: four checkpoints (the desk with the
  widget, the app's window, the widget's popup over it, the app's shortcuts sheet), each shot
  twice 3 s apart, with the tree where there is one.
- `compare.sh [-t PERCENT] RUN...`: the pixels that differ (in grey, above PERCENT), their
  regions, and the tree lines that differ, within a run and against the first run.
- `treediff.py BASE NEW`: two trees element by element: moved, resized, appeared, vanished,
  state changed.

## Results

**Within one run: nothing.** At every checkpoint the two shots 3 s apart were pixel-identical,
and so were the trees (seven runs).

**Across fresh boxes, exact pixels: ~2,000 differ on every text-heavy checkpoint.** They are
single pixels (median blob 1 px, largest 13) on nearly every glyph, the same glyphs each time.
Two starts of the app **in one box** differ the same way (1,073 and 1,809 px), so the source is
the app's own process, not the box. Narrowed down:

| app started 3-4 times in one box with | pixels differing between starts |
|---|---|
| defaults (Qt Quick on the GPU, Vulkan/GL) | 748-2,179 |
| `QSG_RENDER_LOOP=basic` | 1,789-2,179 |
| `LIBGL_ALWAYS_SOFTWARE=1 QSG_RHI_BACKEND=opengl` (llvmpipe) | 914-1,898 |
| `QT_QUICK_BACKEND=software` | 0 |
| `QML_DISABLE_DISTANCEFIELD=1` (GPU) | 0 |

Qt Quick's distance-field text is not repeatable from one start to the next: the glyph atlas
fills in whatever order the text arrives. `QML_DISABLE_DISTANCEFIELD=1` makes it exact on
the GPU, but it renders text natively, which is a little different from what users see
(55,922 px against the default, not visible at 2x). The noise is also faint: **every pixel of
it is below 10% of full scale**, and a 10% threshold removes all of it:

| diff | blobs | largest | at 10% |
|---|---|---|---|
| distance-field noise, two starts | 1,017 | 13 px | none left |
| a 300x40 text block moved 1 px right (synthetic) | 40 | 122 px | 60 blobs (still at 25%) |
| one glyph painted over (synthetic) | 1 | 105 px | kept |

**At 10%, what is left between boxes is content, and only content:**
- the server's listening port, random per profile, in the app's header (fixed by the
  subject's setup: a test pins its own data)
- "added … HH:MM": the server's clock when the box added the items. It is the same within a
  minute, and differs in a run 20 minutes later (22 px). Only a pinned clock fixes that
  (libfaketime is not installed here; nothing pins time yet).
- disk free space would be next (not seen: it did not change in that hour).

With the port pinned, two fresh boxes were **identical at 10% on all four checkpoints**, desk and
popup included. The trees never differed across boxes (the header's port label is not in the
tree).

**A real regression is caught and named.** The app built from a copy with one spacing changed
(a toolbar row's `spacing` from 2 to 4 logical px) against the baseline: pixel regions only
around the toolbar (and the clock region above), and the tree said exactly what happened (the
row is anchored right, so each earlier button moves further left):

```
moved -6,+0           button "9"  @1681,16 20x28
moved -6,+0           text "filter …"  @1504,22 175x16
moved -6,+0           button  @1715,19 27x22
moved -6,+0           button "add …"  @1756,16 28x28
moved -4,+0           button "settings · ,"  @1788,16 28x28
moved -2,+0           button "more · ctrl+."  @1820,16 28x28
```

**The sentinel works for windows and the bar, not for Quickshell popups.** 0 magenta pixels in
the bar and in the app's window shot; the magenta in gaps between windows is expected, so the
check counts only inside a surface. The widget's popup is drawn inside a full-screen layer
(`omarchy-keyboard-panel`, 1920x1080), so Hyprland does not know the popup's own rectangle.
For a popup, the rectangle has to come from pixels (what changed when it opened).

**The widget has no tree**, as finding 244 found for Quickshell: its half of the check is
pixels only, which worked as well (identical at 10%).

**A threshold misses subtle colour.** A row brightened by 3 levels (1.2%, text included; a
fill on the wrong percentage looks like this): at 10% nothing is left; exactly, one blob of 5,000
px. The noise, exactly, never made a blob over 13 px (every pair measured: restarts, GL, basic
loop, across boxes, all four checkpoints).

## What this means for `check`

- Compare exact pixels, the rendering users see (no `QML_DISABLE_DISTANCEFIELD`, which would
  also change the bar's and every plugin's text: the shell is Qt Quick), and drop isolated blobs
  under ~32 px (2.5x the largest noise), counted in the report rather than hidden.
  `QML_DISABLE_DISTANCEFIELD=1` as an opt-in strict mode for a change of a few pixels. Not
  measured yet: the smallest real change (a 1 px dot, a short line) and the noise on another GPU.
- Run a new baseline twice: a region that differs between the two is unstable (a port, a time),
  to pin in the test's data or mask, said when the baseline is made, not on the first failure.
- Name changes from the tree where there is one; pixels and regions where there is not.
- Clean profile as above; a pinned clock is the gap left.

**Afterwards, with `check` built (finding 359):** the AMD iGPU (`OMABOX_RENDER_NODE=renderD128`):
three runs identical, the largest noise blob 7 px (NVIDIA: 13). The same scenario on AMD against an
NVIDIA baseline: 203 regions in the app: baselines are per GPU. And a flaky layout in the subject: in
one start of eight its status column (square and word) sat 3-4 px left of where the other seven had
it, which two runs could not tell apart from the right one (hence three runs and a majority).

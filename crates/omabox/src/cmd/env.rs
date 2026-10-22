//! `omabox env`: exports for host-side Wayland tools.

use crate::args::name_arg;
use crate::boxes::{box_env, need_box};
use crate::cat;
use crate::exit::{Ctx, Res, out};
use crate::select::select_box;
use crate::shell::quote_q;

pub fn run(ctx: &mut Ctx, args: &[Vec<u8>]) -> Res<()> {
    // (name_arg says why; its words are still being read: a usage error.)
    let name = name_arg(ctx, "env", args).map_err(|_| ctx.refused())?;
    select_box(ctx, &name)?;
    need_box(ctx)?;
    // The box bus is private and its Hyprland socket path too long to reach directly: for those use
    // `omabox run` and `omabox hyprctl`. Plain Wayland clients (wl-paste, wlr-randr, ...) work with
    // this. The socket is in the box's runtime dir: a tool pointed at it talks to whatever the box
    // put there.
    let Some(wd) = box_env(&ctx.d, "WAYLAND_DISPLAY") else {
        return Err(ctx.die(cat!("box '", ctx.name, "' has no WAYLAND_DISPLAY")));
    };
    out(cat!(
        "export WAYLAND_DISPLAY=",
        quote_q(&cat!(ctx.d, "/run/", wd)),
        "\nunset DISPLAY WAYLAND_SOCKET HYPRLAND_INSTANCE_SIGNATURE\n"
    ))
}

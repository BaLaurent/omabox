//! `omabox host -- CMD [ARG...]`: one command on the user's real desktop, from a guarded shell, when
//! the user asked for it (finding 67). The one way omabox runs something there unsilenced (S8's
//! exception), through host::HostAsked.

use crate::exit::{Ctx, Res};
use crate::host::HostAsked;

pub fn run(ctx: &mut Ctx, args: &[Vec<u8>]) -> Res<()> {
    let argv = match args.first() {
        Some(a) if a == b"--" => &args[1..],
        _ => args,
    };
    if argv.is_empty() {
        return Err(ctx.die("host: nothing to run (omabox host -- CMD ...)"));
    }
    ctx.parsed();
    HostAsked::user_asked(ctx)?.exec(ctx, argv)
}

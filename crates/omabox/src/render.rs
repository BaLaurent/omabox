//! Where a box renders (#324 wave 5a): its render node, an interactive box's, and NVIDIA's device
//! nodes for it (S1: render nodes only).
//!
//! S1 by construction (#355): a box's GPU goes in only as one of the types here, each made only from a
//! path of its kind. A RenderNode is `/dev/dri/renderD<digits>` and nothing else, so the seat's card
//! nodes (`/dev/dri/card*`, which Hyprland would take the seat through: finding 5) can never be one;
//! an NvidiaDev is `/dev/nvidia<digits>`, `/dev/nvidiactl` or `/dev/nvidia-uvm`; NVIDIA's EGL wants the
//! card's path to exist (finding 294), which a CardPlaceholder gives as an empty file of the box dir's,
//! read-only at `/dev/dri/cardN`, never the device.

#![allow(dead_code)] // (until cmd/up.rs calls it)

use crate::cat;
use crate::exit::{Ctx, Res, say};
use crate::gpu::{conf_gpu, gpu_is, gpu_kind, render_driver, render_slot};
use crate::shell;
use std::path::PathBuf;

/// Where render nodes and their devices are (bash's DRI, SYSDRM, SYSPCI).
pub const DRI: &str = "/dev/dri";
pub const SYSDRM: &str = "/sys/class/drm";
pub const SYSPCI: &str = "/sys/bus/pci/devices";

/// A GPU render node, `/dev/dri/renderD<digits>`: the only DRM node a box gets (S1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderNode(Vec<u8>);

impl RenderNode {
    /// P when it is a render node's path, as written (no `..`, no trailing slash): else None.
    pub fn new(p: &[u8]) -> Option<RenderNode> {
        shell::re_match("^/dev/dri/renderD[0-9]+$", p).then(|| RenderNode(p.to_vec()))
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn path(&self) -> PathBuf {
        shell::path(&self.0)
    }
    /// Its driver (render_driver).
    pub fn driver(&self) -> Vec<u8> {
        render_driver(&self.0)
    }
    /// Its GPU's PCI slot (render_slot).
    pub fn slot(&self) -> Vec<u8> {
        render_slot(&self.0)
    }
    /// The GPU is NVIDIA's (its driver is nvidia).
    pub fn is_nvidia(&self) -> bool {
        self.driver() == b"nvidia"
    }
}

/// One of NVIDIA's userspace device nodes a box's GPU needs: `/dev/nvidia<digits>`, `/dev/nvidiactl`
/// or `/dev/nvidia-uvm` (CUDA's, #149). Never a DRM node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NvidiaDev(Vec<u8>);

impl NvidiaDev {
    pub fn new(p: &[u8]) -> Option<NvidiaDev> {
        (p == b"/dev/nvidiactl"
            || p == b"/dev/nvidia-uvm"
            || shell::re_match("^/dev/nvidia[0-9]+$", p))
        .then(|| NvidiaDev(p.to_vec()))
    }
    pub fn ctl() -> NvidiaDev {
        NvidiaDev(b"/dev/nvidiactl".to_vec())
    }
    pub fn uvm() -> NvidiaDev {
        NvidiaDev(b"/dev/nvidia-uvm".to_vec())
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn path(&self) -> PathBuf {
        shell::path(&self.0)
    }
}

/// An empty file of the box dir's (`$D/card-placeholder`), to be bound read-only at `/dev/dri/cardN`:
/// NVIDIA's Wayland EGL stats the GPU's primary node before its render node and gives up without it
/// (#204, finding 294); it never opens it. Never the device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardPlaceholder {
    src: Vec<u8>,
    dest: Vec<u8>,
}

impl CardPlaceholder {
    /// `: > "$D/card-placeholder"`, for card CARD (`card<digits>`, nvidia_card's): the file made (or
    /// emptied) and checked to be a regular, empty file of D's, then the bind's two paths.
    pub fn make(d: &[u8], card: &[u8]) -> std::io::Result<CardPlaceholder> {
        let bad = || std::io::Error::from(std::io::ErrorKind::InvalidInput);
        if !shell::re_match("^card[0-9]+$", card) {
            return Err(bad());
        }
        let src = cat!(d, "/card-placeholder");
        if !src.starts_with(b"/")
            || [b"/dev/".as_slice(), b"/sys/", b"/proc/"]
                .iter()
                .any(|x| src.starts_with(x))
        {
            return Err(bad());
        }
        std::fs::File::create(shell::path(&src))?;
        let m = std::fs::symlink_metadata(shell::path(&src))?;
        if !m.file_type().is_file() || m.len() != 0 {
            return Err(bad());
        }
        Ok(CardPlaceholder {
            src,
            dest: cat!(DRI, "/", card),
        })
    }
    /// The empty file, on the host.
    pub fn src(&self) -> &[u8] {
        &self.src
    }
    /// Where the box sees it: `/dev/dri/cardN`.
    pub fn dest(&self) -> &[u8] {
        &self.dest
    }
}

/// `access(P, MODE) == 0`: `[ -r P ]`, `[ -w P ]` as test(1) asks them.
fn access(p: &[u8], mode: libc::c_int) -> bool {
    std::ffi::CString::new(p.to_vec()).is_ok_and(|c| {
        // SAFETY: c is a valid C string for the call.
        unsafe { libc::access(c.as_ptr(), mode) == 0 }
    })
}

/// `[ -r P ] && [ -w P ]`.
fn rw(p: &[u8]) -> bool {
    access(p, libc::R_OK) && access(p, libc::W_OK)
}

/// `for n in "$DRI"/renderD*`: the render nodes' paths, in byte order (finding 446).
fn render_globbed() -> Vec<Vec<u8>> {
    let Ok(rd) = std::fs::read_dir(DRI) else {
        return Vec::new();
    };
    let mut v: Vec<Vec<u8>> = rd
        .flatten()
        .map(|e| std::os::unix::ffi::OsStringExt::into_vec(e.file_name()))
        .filter(|n| n.starts_with(b"renderD"))
        .map(|n| cat!(DRI, "/", n))
        .collect();
    v.sort();
    v
}

/// conf_get gpu: the `gpu` setting (auto by default).
fn conf_want() -> Vec<u8> {
    crate::conf::conf_get("gpu", conf_gpu, b"auto")
}

/// render_node: the GPU render node a headless box renders with: $OMABOX_RENDER_NODE; else the first
/// usable one the `gpu` setting names (#156, finding 214); else, and when that GPU is not there (an
/// NVIDIA card handed to a VM), the first usable one, said with why (finding 237). Only ever a render
/// node, never a card node: that is the seat (finding 5).
pub fn render_node(ctx: &Ctx) -> Res<RenderNode> {
    let mut n = shell::env_or_empty("OMABOX_RENDER_NODE");
    if n.is_empty() {
        let want = conf_want();
        let mut fell = false;
        if want != b"auto" {
            n = render_globbed()
                .into_iter()
                .find(|x| rw(x) && gpu_is(x, &want))
                .unwrap_or_default();
            fell = n.is_empty();
        }
        if n.is_empty() {
            n = render_globbed()
                .into_iter()
                .find(|x| rw(x))
                .unwrap_or_default();
        }
        if fell && !n.is_empty() {
            let node = n.rsplit(|&c| c == b'/').next().unwrap_or_default();
            say(cat!(
                "gpu ",
                want,
                " (omabox config gpu): none usable here (",
                gpu_why_not(&want),
                "), so the first GPU: ",
                render_driver(&n),
                " ",
                render_slot(&n),
                " (",
                node,
                ")"
            ));
        }
    }
    match RenderNode::new(&n) {
        Some(r) if rw(&n) => Ok(r),
        _ => Err(ctx.die(cat!(
            "no usable GPU render node (",
            DRI,
            "/renderD*); set OMABOX_RENDER_NODE"
        ))),
    }
}

/// render_nodes: every usable render node, for an interactive box, whose Hyprland renders on the GPU
/// the desktop's compositor names, not necessarily render_node's (finding 95). Render nodes only.
pub fn render_nodes() -> Vec<RenderNode> {
    render_globbed()
        .into_iter()
        .filter(|n| rw(n))
        .filter_map(|n| RenderNode::new(&n))
        .collect()
}

/// render_fallback N: the `gpu` setting names a GPU and N is not it, and OMABOX_RENDER_NODE did not
/// choose.
pub fn render_fallback(n: &RenderNode) -> bool {
    if !shell::env_or_empty("OMABOX_RENDER_NODE").is_empty() {
        return false;
    }
    let want = conf_want();
    want != b"auto" && !gpu_is(n.as_bytes(), &want)
}

/// pci_driver DEV: the driver a PCI device (its dir in SYSPCI) is bound to, or nothing.
fn pci_driver(dev: &[u8]) -> Vec<u8> {
    let d = cat!(dev, "/driver");
    if !shell::path(&d).exists() {
        return Vec::new();
    }
    crate::repo::readlink_f(&d)
        .map(|p| shell::basename(&p))
        .unwrap_or_default()
}

/// `$(cat FILE)`.
fn read_subst(p: &[u8]) -> Vec<u8> {
    std::fs::read(shell::path(p))
        .map(|v| shell::subst(&v))
        .unwrap_or_default()
}

/// pci_kind DEV: by its driver, else (vfio-pci, or none) by its vendor, so an NVIDIA card handed to a
/// VM is still one (finding 237).
fn pci_kind(dev: &[u8]) -> Vec<u8> {
    let k = gpu_kind(&pci_driver(dev));
    if !k.is_empty() {
        return k.to_vec();
    }
    match read_subst(&cat!(dev, "/vendor")).as_slice() {
        b"0x10de" => b"nvidia".to_vec(),
        b"0x1002" => b"amd".to_vec(),
        b"0x8086" => b"intel".to_vec(),
        _ => Vec::new(),
    }
}

/// slot_node SLOT: the render node of the GPU at PCI SLOT, or nothing.
fn slot_node(slot: &[u8]) -> Vec<u8> {
    render_globbed()
        .into_iter()
        .find(|n| shell::path(n).exists() && render_slot(n) == slot)
        .unwrap_or_default()
}

/// pci_why DEV: why a box cannot render on the GPU at DEV, or nothing when it can.
fn pci_why(dev: &[u8]) -> Vec<u8> {
    let drv = pci_driver(dev);
    let node = slot_node(&shell::basename(dev));
    if drv.is_empty() {
        b"no driver".to_vec()
    } else if node.is_empty() {
        cat!("bound to ", drv)
    } else if !rw(&node) {
        cat!("no access to ", node)
    } else {
        Vec::new()
    }
}

/// gpu_why_not WANT: why no usable GPU is the one WANT names (a kind or a slot), each such GPU with
/// its reason ("0000:01:00.0: bound to vfio-pci"), or that there is none.
pub fn gpu_why_not(want: &[u8]) -> Vec<u8> {
    let mut why: Vec<u8> = Vec::new();
    let mut devs: Vec<Vec<u8>> = std::fs::read_dir(SYSPCI)
        .map(|rd| {
            rd.flatten()
                .map(|e| std::os::unix::ffi::OsStringExt::into_vec(e.file_name()))
                .filter(|n| n.first() != Some(&b'.'))
                .collect()
        })
        .unwrap_or_default();
    devs.sort();
    for name in devs {
        let dev = cat!(SYSPCI, "/", name);
        if !read_subst(&cat!(dev, "/class")).starts_with(b"0x03") {
            continue;
        }
        let ok = match want {
            b"nvidia" | b"amd" | b"intel" => pci_kind(&dev) == want,
            _ => name == want,
        };
        if !ok {
            continue;
        }
        let w = pci_why(&dev);
        if !why.is_empty() {
            why.extend_from_slice(b"; ");
        }
        let w = if w.is_empty() {
            b"usable now".to_vec()
        } else {
            w
        };
        why.extend_from_slice(&cat!(name, ": ", w));
    }
    if !why.is_empty() {
        return why;
    }
    match want {
        b"nvidia" | b"amd" | b"intel" => cat!("no ", want, " GPU on this machine"),
        _ => cat!("no GPU at ", want),
    }
}

/// nvidia_device RENDER [CREATE]: the /dev/nvidiaN of RENDER's GPU (nvidia_node); CREATE false
/// leaves a missing node missing. Never a DRM card.
pub fn nvidia_device(ctx: &Ctx, render: &RenderNode, create: bool) -> Res<NvidiaDev> {
    let slot = render.slot();
    let p = nvidia_node(ctx, &slot, create, b"/proc/driver/nvidia", b"/dev")?;
    NvidiaDev::new(&p).ok_or_else(|| {
        ctx.die(cat!(
            "NVIDIA GPU ",
            slot,
            ": ",
            p,
            " is not one of NVIDIA's device nodes"
        ))
    })
}

/// nvidia_node SLOT CREATE PROC DEV: the DEV/nvidiaN of the GPU at PCI SLOT (PROC and DEV stand in for
/// /proc/driver/nvidia and /dev in the unit tests). A GPU switched to the nvidia driver at runtime can
/// lack its nodes until something creates them (finding 122): NVIDIA's setuid helper, nvidia-modprobe,
/// is asked once for each missing one, unless CREATE is false. /dev/nvidiactl is minor 255: `-c MINOR`
/// makes only /dev/nvidiaMINOR (finding 254).
pub fn nvidia_node(ctx: &Ctx, slot: &[u8], create: bool, proc: &[u8], dev: &[u8]) -> Res<Vec<u8>> {
    let info = cat!(proc, "/gpus/", slot, "/information");
    let text = match std::fs::read(shell::path(&info)) {
        Ok(t) if access(&info, libc::R_OK) => t,
        _ => return Err(ctx.die(cat!("NVIDIA GPU ", slot, " has no ", info))),
    };
    let minor = device_minor(&text);
    if minor.is_empty() || !minor.iter().all(u8::is_ascii_digit) {
        return Err(ctx.die(cat!("NVIDIA GPU ", slot, ": no device minor in ", info)));
    }
    let mut miss = nvidia_missing(dev, &minor);
    if create && !miss.is_empty() && crate::exec::executable_on_path(b"nvidia-modprobe") {
        for m in &miss {
            let mut c = crate::sig::command("nvidia-modprobe");
            c.arg("-c")
                .arg(shell::path(m))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            let _ = crate::sig::run_status(c);
        }
        miss = nvidia_missing(dev, &minor);
    }
    if !miss.is_empty() {
        let (mut what, mut run) = (Vec::new(), Vec::new());
        for m in &miss {
            if m == b"255" {
                what.extend_from_slice(&cat!(" ", dev, "/nvidiactl"));
            } else {
                what.extend_from_slice(&cat!(" ", dev, "/nvidia", m));
            }
            run.extend_from_slice(&cat!(" `nvidia-modprobe -c ", m, "`"));
        }
        return Err(ctx.die(cat!(
            "NVIDIA GPU ",
            slot,
            " has no usable",
            what,
            " (run",
            run,
            ", NVIDIA's helper from nvidia-utils, no root needed), then try again"
        )));
    }
    Ok(cat!(dev, "/nvidia", minor))
}

/// `awk -F: '/^Device Minor:/ {gsub(/[[:space:]]/, "", $2); print $2}'`: each such line's second
/// field, white space out, one a line.
fn device_minor(text: &[u8]) -> Vec<u8> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    for l in text.split(|&c| c == b'\n') {
        if !l.starts_with(b"Device Minor:") {
            continue;
        }
        let f = l.split(|&c| c == b':').nth(1).unwrap_or_default();
        out.push(
            f.iter()
                .copied()
                .filter(|c| !c.is_ascii_whitespace())
                .collect(),
        );
    }
    out.join(&b'\n')
}

/// nvidia_missing DEV MINOR: the minors of the nodes nvidia_node needs that DEV lacks, the GPU's and
/// 255 (nvidiactl).
fn nvidia_missing(dev: &[u8], minor: &[u8]) -> Vec<Vec<u8>> {
    use std::os::unix::fs::FileTypeExt;
    let chr = |p: Vec<u8>| {
        std::fs::metadata(shell::path(&p)).is_ok_and(|m| m.file_type().is_char_device())
    };
    let mut v = Vec::new();
    if !chr(cat!(dev, "/nvidia", minor)) {
        v.push(minor.to_vec());
    }
    if !chr(cat!(dev, "/nvidiactl")) {
        v.push(b"255".to_vec());
    }
    v
}

/// nvidia_card RENDER: the name (cardN) of the primary node of RENDER's GPU, the path NVIDIA's EGL
/// gives as its device file (EGL_DRM_DEVICE_FILE_EXT); None when sysfs names none.
pub fn nvidia_card(render: &RenderNode) -> Option<Vec<u8>> {
    nvidia_card_in(render.as_bytes(), SYSDRM.as_bytes())
}

/// nvidia_card RENDER SYS (SYS stands in for /sys/class/drm in the unit tests). The glob's names in
/// byte order (finding 446).
fn nvidia_card_in(render: &[u8], sys: &[u8]) -> Option<Vec<u8>> {
    let node = render.rsplit(|&c| c == b'/').next().unwrap_or_default();
    let dir = cat!(sys, "/", node, "/device/drm");
    shell::glob(&shell::path(&dir), false)
        .into_iter()
        .find(|n| shell::re_match("^card[0-9]+$", n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn render_nodes_only() {
        for ok in [&b"/dev/dri/renderD128"[..], b"/dev/dri/renderD0"] {
            assert!(RenderNode::new(ok).is_some(), "{ok:?}");
        }
        for bad in [
            &b"/dev/dri/card0"[..],
            b"/dev/dri/card1",
            b"/dev/input/event3",
            b"/run/seatd.sock",
            b"/dev/dri/renderD",
            b"/dev/dri/renderD12a",
            b"/dev/dri/renderD128/../card0",
            b"/dev/dri/renderD128/",
            b"/dev/dri/renderD128\n",
            b"/dev/dri//renderD128",
            b"dev/dri/renderD128",
            b"/dev/dri/renderD\xd9\xa1",
        ] {
            assert!(RenderNode::new(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn nvidia_nodes_only() {
        for ok in [
            &b"/dev/nvidia0"[..],
            b"/dev/nvidia12",
            b"/dev/nvidiactl",
            b"/dev/nvidia-uvm",
        ] {
            assert!(NvidiaDev::new(ok).is_some(), "{ok:?}");
        }
        for bad in [
            &b"/dev/nvidia"[..],
            b"/dev/nvidia-modeset",
            b"/dev/nvidia-uvm-tools",
            b"/dev/dri/card0",
            b"/dev/nvidia0/../input",
            b"/dev/nvidiactl\n",
        ] {
            assert!(NvidiaDev::new(bad).is_none(), "{bad:?}");
        }
    }

    proptest! {
        // No path either type accepts is the seat's: a card node, an input device, seatd's socket,
        // the system bus, a runtime dir, or one that climbs out with `..`.
        #[test]
        fn no_seat_path_is_a_box_gpu_node(tail in "[a-zA-Z0-9/._-]{0,24}", pre in prop::sample::select(vec![
            "/dev/dri/renderD", "/dev/dri/card", "/dev/input/", "/run/seatd", "/run/dbus/", "/run/user/",
            "/dev/nvidia", "/dev/", "",
        ])) {
            let p = format!("{pre}{tail}");
            let r = RenderNode::new(p.as_bytes()).map(|r| r.0);
            let n = NvidiaDev::new(p.as_bytes()).map(|n| n.0);
            for ok in [r, n].into_iter().flatten() {
                let s = String::from_utf8_lossy(&ok).into_owned();
                prop_assert!(!s.contains("card") && !s.contains("input") && !s.contains("seatd"));
                prop_assert!(!s.contains("..") && !s.starts_with("/run") && !s.contains("dbus"));
                prop_assert!(s.starts_with("/dev/dri/renderD") || s.starts_with("/dev/nvidia"));
            }
        }
    }

    #[test]
    fn card_placeholder_is_an_empty_file() {
        let d = std::env::temp_dir().join(format!("omabox-cp-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let db = shell::bytes(&d).to_vec();
        std::fs::write(d.join("card-placeholder"), b"x").unwrap();
        let c = CardPlaceholder::make(&db, b"card1").unwrap();
        assert_eq!(c.dest(), b"/dev/dri/card1");
        assert_eq!(std::fs::metadata(shell::path(c.src())).unwrap().len(), 0);
        assert!(CardPlaceholder::make(&db, b"renderD128").is_err());
        assert!(CardPlaceholder::make(&db, b"card1/../x").is_err());
        assert!(CardPlaceholder::make(b"/dev/dri", b"card1").is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn device_minor_as_awk() {
        assert_eq!(
            device_minor(b"Model: x\nDevice Minor: \t 1 \nIRQ: 2\n"),
            b"1"
        );
        assert_eq!(device_minor(b"Device Minor:"), b"");
        assert_eq!(device_minor(b"Device Minor: 0\nDevice Minor: 3"), b"0\n3");
        assert_eq!(device_minor(b" Device Minor: 0"), b"");
    }

    #[test]
    fn nvidia_node_and_card_in_a_fake_tree() {
        let t = std::env::temp_dir().join(format!("omabox-nv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let tb = shell::bytes(&t).to_vec();
        std::fs::create_dir_all(t.join("proc/gpus/0000:01:00.0")).unwrap();
        std::fs::create_dir_all(t.join("dev")).unwrap();
        std::fs::write(
            t.join("proc/gpus/0000:01:00.0/information"),
            "Model: X\nDevice Minor: 0\n",
        )
        .unwrap();
        let mut ctx = Ctx::new(b"up", b"/x".to_vec());
        ctx.parsed();
        let (p, d) = (cat!(tb, "/proc"), cat!(tb, "/dev"));
        // (no char devices in a temp dir: both missing, said; nothing created)
        let r = crate::exit::muted(|| nvidia_node(&ctx, b"0000:01:00.0", false, &p, &d));
        assert!(r.is_err());
        assert_eq!(
            nvidia_missing(&d, b"0"),
            vec![b"0".to_vec(), b"255".to_vec()]
        );
        let r = crate::exit::muted(|| nvidia_node(&ctx, b"0000:02:00.0", false, &p, &d));
        assert!(r.is_err());
        std::fs::create_dir_all(t.join("sys/renderD128/device/drm/card1")).unwrap();
        std::fs::create_dir_all(t.join("sys/renderD128/device/drm/renderD128")).unwrap();
        let s = cat!(tb, "/sys");
        assert_eq!(
            nvidia_card_in(b"/dev/dri/renderD128", &s),
            Some(b"card1".to_vec())
        );
        assert_eq!(nvidia_card_in(b"/dev/dri/renderD129", &s), None);
        std::fs::remove_dir_all(&t).unwrap();
    }
}

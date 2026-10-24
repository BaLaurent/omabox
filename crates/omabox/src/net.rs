//! A box's network (#324 wave 5a, #355): pasta's arguments for each mode (finding 89), the isolated
//! mode carrying its allow list (S5), and the protected ports: host ports no connected box reaches.
//!
//! Protected ports (#355, S10): `protect-ports=PORT[,PORT...]` in ~/.config/omabox/config, none by
//! default. A connected box gets each one excluded (`~PORT`) from both of pasta's `-T` rules and both
//! `-U` rules, so its loopback forwards none of them to the host's; and, set up before anything in
//! the box runs by pasta's own shell (root in pasta's namespaces, which the box's user namespace below
//! maps back to the user with no capabilities), an nftables rule in the box's network namespace that
//! rejects a connection to any of the host's addresses on those ports, as a server listening on all
//! of them (0.0.0.0) would otherwise take one through the host's docker bridge or VPN address. An
//! isolated box reaches only its --allow ports, and a protected one is refused there.

#![allow(dead_code)] // (until cmd/up.rs calls it)

use crate::cat;

/// Ports: sorted, each once, 1-65535 (allow_ports: 8082,8081,8081 is 8081,8082, finding 175).
pub use crate::upspec::AllowedPorts as Ports;

/// How a box is networked: pasta's tap with the host's loopback forwarded both ways (connected),
/// a loopback of its own with only the allowed ports spliced to the host's (isolated), or a network
/// namespace with only a loopback and no pasta (none: a nested connected box, or one started from a
/// no_new_privs process).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Net {
    Connected { protect: Ports },
    Isolated { allow: Ports },
    None,
}

/// pasta's arguments for NET (not None), its pid written to PID_FILE; EPH the ephemeral range
/// (`32768-60999`), which `auto` skips and so has rules of its own (finding 156).
pub fn pasta_args(net: &Net, pid_file: &[u8], eph: &[u8]) -> Vec<Vec<u8>> {
    let mut a: Vec<Vec<u8>> = vec![b"-q".to_vec(), b"-P".to_vec(), pid_file.to_vec()];
    let mut add = |w: &[&[u8]]| a.extend(w.iter().map(|x| x.to_vec()));
    match net {
        Net::Isolated { allow } => {
            let t = if allow.is_empty() {
                b"none".to_vec()
            } else {
                allow.csv()
            };
            add(&[
                b"--splice-only",
                b"-t",
                b"none",
                b"-u",
                b"none",
                b"-U",
                b"none",
                b"-T",
                &t,
            ]);
        }
        Net::Connected { protect } => {
            // (the protected ports out of every host-bound rule, #355: `~P,` in front)
            let ex: Vec<u8> = protect
                .ports()
                .iter()
                .flat_map(|p| format!("~{p},").into_bytes())
                .collect();
            let auto = cat!(ex, "auto");
            let eph_auto = cat!(ex, eph, ",auto");
            add(&[
                b"--config-net",
                b"--no-map-gw",
                b"-D",
                b"none",
                b"-S",
                b"none",
                b"--host-lo-to-ns-lo",
                b"-t",
                b"127.0.0.1/auto",
                b"-t",
                &cat!("127.0.0.1/", eph, ",auto"),
                b"-u",
                b"127.0.0.1/auto",
                b"-u",
                &cat!("127.0.0.1/", eph, ",auto"),
                b"-T",
                &auto,
                b"-T",
                &eph_auto,
                b"-U",
                &auto,
                b"-U",
                &eph_auto,
            ]);
        }
        Net::None => {}
    }
    a
}

/// The ephemeral port range (`/proc/sys/net/ipv4/ip_local_port_range`, as `32768-60999`), that
/// default when it cannot be read.
pub fn ephemeral() -> Vec<u8> {
    let s = std::fs::read("/proc/sys/net/ipv4/ip_local_port_range").unwrap_or_default();
    let w: Vec<&[u8]> = s
        .split(|c| c.is_ascii_whitespace())
        .filter(|x| !x.is_empty())
        .collect();
    match w.as_slice() {
        [a, b, ..] if a.iter().all(u8::is_ascii_digit) && b.iter().all(u8::is_ascii_digit) => {
            cat!(a, "-", b)
        }
        _ => b"32768-60999".to_vec(),
    }
}

/// The host's own addresses, from its routing tables (`/32 host LOCAL` in /proc/net/fib_trie) and
/// /proc/net/if_inet6, loopback's left out (a box's loopback is its own), each once, IPv4 then IPv6.
pub fn host_addrs() -> (Vec<String>, Vec<String>) {
    let v4 = std::fs::read("/proc/net/fib_trie").unwrap_or_default();
    let v6 = std::fs::read("/proc/net/if_inet6").unwrap_or_default();
    (local_v4(&v4), local_v6(&v6))
}

fn local_v4(trie: &[u8]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut last: Option<std::net::Ipv4Addr> = None;
    for l in trie.split(|&c| c == b'\n') {
        let t = String::from_utf8_lossy(l);
        let t = t.trim();
        if let Some(a) = t.strip_prefix("|-- ") {
            last = a.parse().ok();
        } else if t == "/32 host LOCAL"
            && let Some(a) = last.take()
            && !a.is_loopback()
            && !out.contains(&a.to_string())
        {
            out.push(a.to_string());
        }
    }
    out
}

fn local_v6(table: &[u8]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in table.split(|&c| c == b'\n') {
        let Some(hex) = l.split(|&c| c == b' ').next() else {
            continue;
        };
        if hex.len() != 32 {
            continue;
        }
        let Some(bytes) = (0..16)
            .map(|i| {
                std::str::from_utf8(&hex[2 * i..2 * i + 2])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            })
            .collect::<Option<Vec<u8>>>()
        else {
            continue;
        };
        let Ok(arr) = <[u8; 16]>::try_from(bytes) else {
            continue;
        };
        let a = std::net::Ipv6Addr::from(arr);
        if !a.is_loopback() && !out.contains(&a.to_string()) {
            out.push(a.to_string());
        }
    }
    out
}

/// The nft program, for pasta's shell, that rejects connections from the box to any of the host's
/// addresses (V4, V6) on a protected port; not to the box's own (oifname lo: its loopback and the
/// address pasta gave it), whose services are its own. None with no port.
pub fn protect_rules(protect: &Ports, v4: &[String], v6: &[String]) -> Option<String> {
    if protect.is_empty() {
        return None;
    }
    let ports = protect
        .ports()
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let mut r = String::from(
        "add table inet omabox; add chain inet omabox out { type filter hook output priority 0; policy accept; }",
    );
    for (fam, addrs) in [("ip", v4), ("ip6", v6)] {
        if addrs.is_empty() {
            continue;
        }
        r.push_str(&format!(
            "; add rule inet omabox out oifname != \"lo\" {fam} daddr {{ {} }} meta l4proto {{ tcp, udp }} th dport {{ {ports} }} reject",
            addrs.join(", ")
        ));
    }
    Some(r)
}

/// conf_get protect-ports: the config's value checked (`PORT[,PORT...]`, the ports once each, in
/// order), else none: an unreadable value is said and ignored, as every setting's.
pub fn conf_protect() -> Ports {
    let v = crate::conf::conf_get(
        "protect-ports",
        |v| Ports::parse(v).ok().map(|p| p.csv()),
        b"",
    );
    Ports::parse(&v).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn ports() {
        assert_eq!(
            Ports::parse(b"8082,8081,8081").map(|p| p.csv()),
            Ok(b"8081,8082".to_vec())
        );
        assert_eq!(Ports::parse(b"080").map(|p| p.csv()), Ok(b"80".to_vec()));
        for bad in ["0", "65536", "1,,2", ",1", "a", "123456", "-1", " 1"] {
            assert!(Ports::parse(bad.as_bytes()).is_err(), "{bad}");
        }
        assert!(Ports::parse(b"").is_ok_and(|p| p.is_empty()));
    }

    #[test]
    fn addresses() {
        let trie = b"Main:\n  +-- 0.0.0.0/0 3 0 5\n     |-- 0.0.0.0\n        /0 universe UNICAST\n     |-- 127.0.0.1\n        /32 host LOCAL\n     |-- 172.17.0.1\n        /32 host LOCAL\n     |-- 172.17.255.255\n        /32 link BROADCAST\nLocal:\n     |-- 172.17.0.1\n        /32 host LOCAL\n";
        assert_eq!(local_v4(trie), vec!["172.17.0.1".to_string()]);
        let t6 = b"00000000000000000000000000000001 01 80 10 80       lo\nfe80000000000000d4e62dfffe8bebda 04 40 20 80 br-1\n20010db8000000000000000000000005 02 40 00 80 enp\n";
        assert_eq!(
            local_v6(t6),
            vec![
                "fe80::d4e6:2dff:fe8b:ebda".to_string(),
                "2001:db8::5".to_string()
            ]
        );
    }

    #[test]
    fn rules() {
        assert!(protect_rules(&Ports::default(), &[], &[]).is_none());
        let p = Ports::parse(b"8080,5432").unwrap_or_default();
        let r = protect_rules(&p, &["172.17.0.1".into()], &[]).unwrap_or_default();
        assert!(r.contains(
            "ip daddr { 172.17.0.1 } meta l4proto { tcp, udp } th dport { 5432, 8080 } reject"
        ));
        assert!(!r.contains("ip6"));
    }

    proptest! {
        // S5: an isolated box's pasta opens exactly its --allow ports (spliced, TCP only) and nothing
        // else; a connected box's every host-bound rule leaves out each protected port.
        #[test]
        fn pasta_opens_what_it_should(ports in prop::collection::btree_set(1u16..=65535, 0..6)) {
            let csv = ports.iter().map(u16::to_string).collect::<Vec<_>>().join(",");
            let set = Ports::parse(csv.as_bytes()).unwrap_or_default();
            let a = pasta_args(&Net::Isolated { allow: set.clone() }, b"/p", b"32768-60999");
            let opt = |o: &[u8]| a.iter().position(|w| w == o).map(|i| a[i + 1].clone());
            prop_assert_eq!(opt(b"-t"), Some(b"none".to_vec()));
            prop_assert_eq!(opt(b"-u"), Some(b"none".to_vec()));
            prop_assert_eq!(opt(b"-U"), Some(b"none".to_vec()));
            let want = if set.is_empty() { b"none".to_vec() } else { set.csv() };
            prop_assert_eq!(opt(b"-T"), Some(want));
            prop_assert!(a.iter().any(|w| w == b"--splice-only"));
            prop_assert_eq!(a.iter().filter(|w| w.starts_with(b"-") && w.len() == 2 && w[1].is_ascii_alphabetic() && w.as_slice() != b"-q" && w.as_slice() != b"-P").count(), 4);
            let c = pasta_args(&Net::Connected { protect: set.clone() }, b"/p", b"32768-60999");
            let mut host_rules = 0;
            for (i, w) in c.iter().enumerate() {
                if w == b"-T" || w == b"-U" {
                    host_rules += 1;
                    let spec = String::from_utf8_lossy(&c[i + 1]).into_owned();
                    for p in set.ports() {
                        let ex = format!("~{p},");
                        prop_assert!(spec.contains(&ex), "{} lacks ~{}", spec, p);
                    }
                }
            }
            prop_assert_eq!(host_rules, 4);
        }
    }
}

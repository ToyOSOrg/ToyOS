//! MEASUREMENT ONLY, never lands: the connection-setup scout's rows, std alone,
//! for the harness's judge and the Linux arm. Read from the job's `setup`,
//! `lan control` and `phase` lines (`https_download.rs`, `netperf_linux.rs`)
//! and netstack's `stamp` lines (`system/netstack/src/stamps.rs`), every stamp
//! nanoseconds on one clock; every row is milliseconds unless it says.
//!
//! A ToyOS fetch's netstack lines are the ones whose `n1` falls inside its own
//! stamps: the lookup's inside J1..J2, the connect's inside J3..J4, the
//! `TCP_NODELAY` option's inside the connect's answer..J4, and the first data
//! exchange's `n3` inside J4..J5. `-` is a stamp that was not there.

use std::collections::BTreeMap;
use std::fmt::Write as _;

const JOB: &str = "https_download: ";
const STAMP: &str = "netstack: stamp ";

type Words = BTreeMap<String, String>;

fn words(text: &str) -> Words {
    text.split_whitespace().filter_map(|w| w.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn num(w: &Words, key: &str) -> u64 {
    w.get(key).and_then(|v| v.parse().ok()).unwrap_or(0)
}

fn said<'a>(log: &'a str, prefix: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    log.lines().filter_map(move |l| l.find(prefix).map(|at| &l[at + prefix.len()..]))
}

/// `to - from` in ms, where both were stamped.
fn ms(from: u64, to: u64) -> Option<f64> {
    (from != 0 && to != 0).then(|| (to as i128 - from as i128) as f64 / 1e6)
}

fn sub(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    Some(a? - b?)
}

fn cell(v: Option<f64>) -> String {
    v.map_or("-".to_string(), |v| format!("{v:.3}"))
}

fn stats(mut v: Vec<f64>) -> String {
    if v.is_empty() {
        return "n=0".to_string();
    }
    v.sort_by(f64::total_cmp);
    let mid = if v.len() % 2 == 1 { v[v.len() / 2] } else { v[v.len() / 2 - 1] / 2.0 + v[v.len() / 2] / 2.0 };
    format!("n={} min={:.3} med={mid:.3} max={:.3}", v.len(), v[0], v[v.len() - 1])
}

/// Every row named, in the order first said, with each fetch's value.
#[derive(Default)]
struct Rows(Vec<(String, Vec<f64>)>);

impl Rows {
    fn put(&mut self, name: &str, v: Option<f64>) {
        let Some(v) = v else { return };
        match self.0.iter_mut().find(|(n, _)| n == name) {
            Some((_, all)) => all.push(v),
            None => self.0.push((name.to_string(), vec![v])),
        }
    }

    fn render(&self, out: &mut String, title: &str) {
        let _ = writeln!(out, "setup: {title}");
        for (name, v) in &self.0 {
            let _ = writeln!(out, "setup:   {name:<36} {}", stats(v.clone()));
        }
    }
}

struct Stamp {
    kind: String,
    w: Words,
}

impl Stamp {
    fn n(&self, k: &str) -> u64 {
        num(&self.w, k)
    }

    /// Whether `n4` is the reply's own interrupt: the pass that received it
    /// took one, the reply was that pass's first frame, and the message landed
    /// after the pass before found the ring empty. Otherwise a frame before it
    /// in the batch, or one an earlier pass consumed, may have raised it.
    fn own_irq(&self) -> bool {
        self.n("irqs") != 0 && self.w.get("idx").is_some_and(|i| i == "0") && self.n("n4") >= self.n("prev")
    }

    /// The reply's wire time, query or SYN to the card until the reply's
    /// interrupt.
    fn wire(&self) -> Option<f64> {
        self.own_irq().then(|| ms(self.n("n3"), self.n("n4"))).flatten()
    }

    fn wake(&self) -> Option<f64> {
        self.own_irq().then(|| ms(self.n("n4"), self.n("n5"))).flatten()
    }
}

/// The scout's rows of every fetch and connect `log` says, then each row's
/// spread.
pub fn table(log: &str) -> String {
    let mut out = String::new();
    let stamps: Vec<Stamp> = said(log, STAMP)
        .filter_map(|l| {
            let (kind, rest) = l.split_once(' ')?;
            Some(Stamp { kind: kind.to_string(), w: words(rest) })
        })
        .collect();
    let find = |kind: &str, key: &str, from: u64, to: u64| stamps.iter().find(|s| s.kind == kind && (from..=to).contains(&s.n(key)));
    let phases: Vec<Words> = said(log, JOB).filter_map(|l| l.strip_prefix("phase ")).map(words).collect();
    let in_phase = |name: &str, at: u64| phases.iter().any(|p| p.get("name").is_some_and(|n| n == name) && (num(p, "from")..=num(p, "to")).contains(&at));

    let mut toyos = Rows::default();
    let mut by_addr = Rows::default();
    let mut linux: BTreeMap<String, Rows> = BTreeMap::new();
    for line in said(log, JOB).filter_map(|l| l.strip_prefix("setup ")) {
        let w = words(line);
        let j: Vec<u64> = (0..6).map(|k| num(&w, &format!("j{k}"))).collect();
        let (j0, j1, j2, j3, j4, j5) = (j[0], j[1], j[2], j[3], j[4], j[5]);
        let addr = w.get("addr").cloned().unwrap_or_default();
        let tcp = ms(j0, j4);
        let tls_phase = ms(j4, j5);
        let ureq = Some(ms(j0, j1).unwrap_or(f64::NAN) + ms(j2, j3).unwrap_or(f64::NAN)).filter(|v| !v.is_nan());
        let lookup = ms(j1, j2);
        if w.get("os").is_some_and(|os| os == "linux") {
            let asked = w.get("asked").cloned().unwrap_or_default();
            let rtt = w.get("tcpi_rtt_us").and_then(|v| v.parse::<f64>().ok()).map(|us| us / 1e3);
            let _ = writeln!(
                out,
                "setup: linux run={} asked={asked} family={} peer={} addr={addr} U1 resolve={} U2 connect={} U3 tcp={} U4 tcpi_rtt={} U5 tls_phase={} ureq={}",
                w.get("run").map_or("?", String::as_str),
                w.get("family").map_or("?", String::as_str),
                w.get("peer_family").map_or("?", String::as_str),
                cell(lookup),
                cell(ms(j3, j4)),
                cell(tcp),
                cell(rtt),
                cell(tls_phase),
                cell(ureq),
            );
            let rows = linux.entry(asked).or_default();
            rows.put("U1 resolve (J2-J1)", lookup);
            rows.put("U2 connect (J4-J3)", ms(j3, j4));
            rows.put("U3 tcp (J4-J0)", tcp);
            rows.put("U4 tcpi_rtt", rtt);
            rows.put("U5 tls_phase (J5-J4)", tls_phase);
            rows.put("ureq (J1-J0 + J3-J2)", ureq);
            continue;
        }
        let l = find("lookup", "n1", j1, j2);
        let c = find("connect", "n1", j3, j4);
        let c6 = c.map_or(j3, |c| c.n("n6"));
        let s = find("setopt", "n1", c6, j4);
        let d = find("data", "n3", j4, j5);
        let n = |st: Option<&Stamp>, k: &str| st.map_or(0, |st| st.n(k));
        let lookup_wire = l.and_then(Stamp::wire);
        let connect = ms(j3, n(c, "n6"));
        let syn_wire = c.and_then(Stamp::wire);
        let nodelay = ms(n(c, "n6"), n(s, "n6"));
        let row = |out: &mut String, name: &str, v: Option<f64>, parts: &[(&str, Option<f64>)]| {
            let parts: Vec<String> = parts.iter().map(|(k, v)| format!("{k} {}", cell(*v))).collect();
            let _ = writeln!(out, "setup:   {name:<14} {:>8}  {}", cell(v), parts.join(", "));
        };
        let _ = writeln!(
            out,
            "setup: toyos run={} addr={addr} family={} tcp={} tls_phase={}",
            w.get("run").map_or("?", String::as_str),
            w.get("family").map_or("?", String::as_str),
            cell(tcp),
            cell(tls_phase)
        );
        row(&mut out, "ureq", ureq, &[("call->resolve", ms(j0, j1)), ("resolved->connector", ms(j2, j3))]);
        row(
            &mut out,
            "lookup",
            lookup,
            &[
                ("wire", lookup_wire),
                ("local", sub(lookup, lookup_wire)),
                ("client->accepted", ms(j1, n(l, "n1"))),
                ("accepted->read", ms(n(l, "n1"), n(l, "n2"))),
                ("read->query", ms(n(l, "n2"), n(l, "n3"))),
                ("wake", l.and_then(Stamp::wake)),
                ("reply->answer", ms(n(l, "n5"), n(l, "n6"))),
                ("answer->client", ms(n(l, "n6"), j2)),
            ],
        );
        row(
            &mut out,
            "connect",
            connect,
            &[
                ("wire", syn_wire),
                ("local", sub(connect, syn_wire)),
                ("client->accepted", ms(j3, n(c, "n1"))),
                ("accepted->read", ms(n(c, "n1"), n(c, "n2"))),
                ("read->syn", ms(n(c, "n2"), n(c, "n3"))),
                ("wake", c.and_then(Stamp::wake)),
                ("reply->answer", ms(n(c, "n5"), n(c, "n6"))),
            ],
        );
        row(
            &mut out,
            "nodelay",
            nodelay,
            &[
                ("answer->accepted", ms(n(c, "n6"), n(s, "n1"))),
                ("accepted->read", ms(n(s, "n1"), n(s, "n2"))),
                ("read->answer", ms(n(s, "n2"), n(s, "n6"))),
            ],
        );
        row(&mut out, "tail", ms(n(s, "n6"), j4), &[]);
        let _ = writeln!(
            out,
            "setup:   {:<14} {:>8}  wake {}, ack_len {}, segs {}, bytes {}",
            "data_rtt",
            cell(d.and_then(Stamp::wire)),
            cell(d.and_then(Stamp::wake)),
            d.map_or("-".to_string(), |d| d.n("ack_len").to_string()),
            d.map_or("-".to_string(), |d| d.n("segs").to_string()),
            d.map_or("-".to_string(), |d| d.n("bytes").to_string()),
        );
        toyos.put("tcp (J4-J0)", tcp);
        toyos.put("ureq", ureq);
        toyos.put("lookup (J2-J1)", lookup);
        toyos.put("lookup_wire", lookup_wire);
        toyos.put("lookup_local", sub(lookup, lookup_wire));
        toyos.put("lookup wake", l.and_then(Stamp::wake));
        toyos.put("connect (N6c-J3)", connect);
        toyos.put("syn_wire", syn_wire);
        toyos.put("connect_local", sub(connect, syn_wire));
        toyos.put("connect wake", c.and_then(Stamp::wake));
        toyos.put("nodelay (N6s-N6c)", nodelay);
        toyos.put("tail (J4-N6s)", ms(n(s, "n6"), j4));
        toyos.put("local sum (lookup+connect+nodelay)", Some(sub(lookup, lookup_wire).unwrap_or(f64::NAN) + sub(connect, syn_wire).unwrap_or(f64::NAN) + nodelay.unwrap_or(f64::NAN)).filter(|v| !v.is_nan()));
        toyos.put("data_rtt", d.and_then(Stamp::wire));
        toyos.put("data wake", d.and_then(Stamp::wake));
        toyos.put("tls_phase (J5-J4)", tls_phase);
        by_addr.put(&format!("syn_wire addr={addr}"), syn_wire);
    }

    let mut lan = Rows::default();
    for line in said(log, JOB).filter_map(|l| l.strip_prefix("lan control ")) {
        let w = words(line);
        let (c0, c1) = (num(&w, "c0"), num(&w, "c1"));
        if c0 == 0 {
            let _ = writeln!(out, "setup: lan control {line}");
            continue;
        }
        let c = find("connect", "n1", c0, c1);
        let _ = writeln!(
            out,
            "setup: lan i={} outcome={} reply={} lan_connect={} lan_syn_wire={} wake={} client->accepted={} accepted->read={} read->syn={} reply->answer={}",
            w.get("i").map_or("?", String::as_str),
            w.get("outcome").map_or("?", String::as_str),
            c.and_then(|c| c.w.get("reply")).map_or("-", String::as_str),
            cell(ms(c0, c1)),
            cell(c.and_then(Stamp::wire)),
            cell(c.and_then(Stamp::wake)),
            cell(ms(c0, c.map_or(0, |c| c.n("n1")))),
            cell(c.and_then(|c| ms(c.n("n1"), c.n("n2")))),
            cell(c.and_then(|c| ms(c.n("n2"), c.n("n3")))),
            cell(c.and_then(|c| ms(c.n("n5"), c.n("n6")))),
        );
        lan.put("lan_connect (c1-c0)", ms(c0, c1));
        lan.put("lan_syn_wire", c.and_then(Stamp::wire));
        lan.put("lan wake", c.and_then(Stamp::wake));
    }

    // Every reply netstack stamped, by the phase it came in.
    let mut phased = Rows::default();
    for s in &stamps {
        let at = s.n(if s.kind == "data" { "n3" } else { "n1" });
        let phase = ["probes", "suite", "lan_idle", "lan_control"].into_iter().find(|p| in_phase(p, at));
        let phase = phase.map(str::to_string).or_else(|| {
            phases.iter().filter_map(|p| p.get("name")).find(|n| n.starts_with("run") && in_phase(n, at)).map(|_| "runs".to_string())
        });
        let Some(phase) = phase else { continue };
        if s.kind == "setopt" {
            phased.put(&format!("{phase} setopt n6-n1"), ms(s.n("n1"), s.n("n6")));
            continue;
        }
        phased.put(&format!("{phase} {} wire", s.kind), s.wire());
        phased.put(&format!("{phase} {} wake", s.kind), s.wake());
    }
    let wakes: Vec<f64> = stamps.iter().filter_map(Stamp::wake).collect();
    let unowned = stamps.iter().filter(|s| s.kind != "setopt" && !s.own_irq()).count();
    let shared = stamps.iter().filter(|s| s.kind != "setopt" && s.own_irq() && s.n("irqs") > 1).count();

    if !toyos.0.is_empty() {
        toyos.render(&mut out, "toyos, over the fetches");
        by_addr.render(&mut out, "toyos syn_wire by whether the fetch's first address was the last fetch's");
    }
    if !lan.0.is_empty() {
        lan.render(&mut out, "toyos LAN control");
    }
    if !phased.0.is_empty() {
        phased.render(&mut out, "toyos, every stamped reply by phase");
        let _ = writeln!(
            out,
            "setup: toyos wakes: {}; {} over 50 us; {unowned} replies whose interrupt is not known their own, left out; {shared} kept with more than one message in their record",
            stats(wakes.clone()),
            wakes.iter().filter(|w| **w > 0.05).count()
        );
    }
    for (asked, rows) in &linux {
        rows.render(&mut out, &format!("linux asked={asked}, over the fetches"));
    }
    out
}

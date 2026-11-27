//! A transfer from the command line, on any of the three systems:
//!
//!   windysync --give <device id> [--only a,b]    ask that device to receive, then send
//!   windysync --accept [--only a,b]              approve the request waiting here, then do this side
//!
//! `--only` takes item ids or their starts (`windysync --scan` lists them); without it the receiver takes everything
//! that has a place on its system.

use std::cell::RefCell;
use std::sync::Arc;

use crate::bus::{self, Answer, Ask, Bus, Device, Request};
use crate::me::{self, Config};
use crate::os::{Env, Os};
use crate::session::{self, Live};

struct Ctx {
    bus: Arc<Bus>,
    cfg: Config,
    secret: [u8; 32],
}

fn connect() -> Result<Ctx, String> {
    let mut cfg = Config::load();
    let token = me::token().ok_or("Not signed in: run `gh auth login`, or set WINDYSYNC_TOKEN.")?;
    if cfg.bus.is_empty() {
        cfg.bus = format!("{}/windysync-bus", bus::github_login(&token)?);
        bus::ensure_repo(&token, &cfg.bus)?;
        cfg.save();
    }
    let b = Arc::new(Bus::new(&cfg.bus, &token));
    let (secret, _) = me::keys();
    let d = me::device(&cfg);
    b.put_json(&format!("devices/{}.json", d.id), &d, None, &format!("device {} ({})", d.name, d.id))?;
    cfg.registered = true;
    cfg.save();
    println!("{} #{} ({}) on {}", cfg.name, cfg.id, d.os, cfg.bus);
    Ok(Ctx { bus: b, cfg, secret })
}

fn device(b: &Bus, id: &str) -> Result<Device, String> {
    let d = b.get_json::<Device>(&format!("devices/{id}.json"))?.map(|d| d.0).ok_or(format!("no device #{id} on the bus"))?;
    if !crate::look::ours(&d) {
        return Err(format!("#{id} runs {}, which this app doesn't move to or from", if d.os.is_empty() { &d.look.os } else { &d.os }));
    }
    Ok(d)
}

/// Prints each step once; while files move, a line per item rather than per few bytes.
fn printer() -> impl Fn(&Live) {
    let last = RefCell::new(String::new());
    move |l: &Live| {
        let key = format!("{}|{}|{}|{}", l.state, l.note, l.items_done, l.current);
        if *last.borrow() != key {
            if l.note.is_empty() {
                println!("{} {:.1} MB  {}/{} items  {}", l.state, l.bytes as f64 / 1e6, l.items_done, l.items, l.current);
            } else {
                println!("{}", l.note);
            }
            *last.borrow_mut() = key;
        }
    }
}

pub fn give(peer: &str, only: Vec<String>) -> Result<(), String> {
    let c = connect()?;
    let other = device(&c.bus, peer)?;
    let r = Request { ask: Ask::Send, from: c.cfg.id.clone(), to: other.id.clone(), at: bus::now(), answer: Answer::Pending, transfer: String::new() };
    c.bus.put_json(&r.path(), &r, None, &format!("Send request {} to {}", r.from, r.to))?;
    let print = printer();
    let r = session::wait(format!("waiting for {} to approve", other.name), &print, || {
        Ok(c.bus.get_json::<Request>(&r.path())?.map(|x| x.0).filter(|x| x.answer != Answer::Pending))
    })?;
    if r.answer != Answer::Accepted {
        return Err(format!("{} said {:?}", other.name, r.answer));
    }
    session::send(&c.bus, &c.secret, &r.transfer, &other, &Env::here(), |mut o| {
        o.items.retain(|i| session::wanted(&i.id, &only));
        Some(o)
    }, print)
}

pub fn accept(only: Vec<String>) -> Result<(), String> {
    let c = connect()?;
    let me = c.cfg.id.clone();
    let mut pending = None;
    for n in c.bus.list("requests")?.into_iter().filter(|n| n.ends_with(".json") && n.contains(&me)) {
        if let Some((r, sha)) = c.bus.get_json::<Request>(&format!("requests/{n}"))?
            && r.to == me && r.answer == Answer::Pending && bus::now().saturating_sub(r.at) < 86400
            && pending.as_ref().is_none_or(|(p, _): &(Request, String)| r.at > p.at)
        {
            pending = Some((r, sha));
        }
    }
    let (r, sha) = pending.ok_or("no request is waiting for this device")?;
    let other = device(&c.bus, if r.from == me { &r.to } else { &r.from })?;
    let names = [(me.clone(), c.cfg.name.clone()), (other.id.clone(), other.name.clone())].into_iter().collect();
    let r = session::approve(&c.bus, r, &sha, &names, true)?;
    println!("approved {:?} from {}: {}", r.ask, other.name, r.transfer);
    let here = Env::here();
    if r.sides().0 == me {
        session::send(&c.bus, &c.secret, &r.transfer, &other, &here, |mut o| {
            o.items.retain(|i| session::wanted(&i.id, &only));
            Some(o)
        }, printer())
    } else {
        let os = Os::current();
        session::receive(&c.bus, &c.secret, &r.transfer, &other, &here, |o| Some(session::picks(o, os, &only)), printer()).map(|_| ())
    }
}

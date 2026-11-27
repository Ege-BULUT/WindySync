//! The window: borderless and see-through, shaped as a gust of wind: a straight top edge that runs on past the
//! corner and hooks down, and a straight bottom edge that trails off to a point on the left and curls up at the end,
//! as the WYSY logo's two lines. This PC is on the left, the other on the right with its card, the panel below;
//! the background lines run straight across like wind. While files move, they run from the sender to the receiver.
//!
//! The bus is read every 5 s on a thread. A transfer runs the command line's own `session::send` or
//! `session::receive` on a thread of its own: it takes the person's ticks from 03 PICK and shows its progress in
//! 04 MOVE. The UI thread only draws and hands things over.

use std::collections::HashSet;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rune::tok::TextStyle;
use rune::{Align, App, AppSpec, Color, Cursor, Cx, El, LineCap, Path, ScrollSpec, Shape, Side, TextEdit, Tx, Waker, WindowOptions, col, el, row, spacer, text};
use rune_kit::{Kit, PaletteSeed, TextInput};

use crate::bus::{self, Answer, Ask, Bus, Device, Request, Session};
use crate::catalog::{CATALOG, Item, LISTS};
use crate::look::{self, H, LID, W, full};
use crate::me::{self, Config, Look};
use crate::os::{Env, Os};
use crate::session::{self, Live, Picks};

// WindySync's site palette, by role: ACCENT marks what is on, DEEP fills it, LIGHT is accent text, ACCENT2 and ACCENT3
// are the logo's other line colours. Rune colours are linear light: these are the sRGB hex values in the comments,
// decoded.
const BG: Color = Color { r: 0.00335, g: 0.00750, b: 0.00857, a: 0.97 }; // #0b1517
const PANEL: Color = Color { r: 0.00478, g: 0.01370, b: 0.01600, a: 0.84 }; // #0f1f22
const PANEL2: Color = Color { r: 0.00750, g: 0.02217, b: 0.02624, a: 1.0 }; // #15292d
const FIELD: Color = Color { r: 0.00212, g: 0.00518, b: 0.00605, a: 1.0 }; // #071012
const EDGE: Color = Color { r: 0.01229, g: 0.04231, b: 0.05127, a: 1.0 }; // #1d3a40
const STEEL: Color = Color { r: 0.10946, g: 0.24228, b: 0.27889, a: 1.0 }; // #5d8790
const ACCENT: Color = Color { r: 0.01600, g: 0.65141, b: 0.85499, a: 1.0 }; // #22d3ee
const DEEP: Color = Color { r: 0.00439, g: 0.17465, b: 0.27889, a: 1.0 }; // #0e7490
const LIGHT: Color = Color { r: 0.37626, g: 0.89627, b: 0.97345, a: 1.0 }; // #a5f3fc
const ACCENT2: Color = Color { r: 0.13563, g: 0.80695, b: 0.94731, a: 1.0 }; // #67e8f9
const ACCENT3: Color = Color { r: 0.83880, g: 0.99110, b: 1.00000, a: 1.0 }; // #ecfeff
const INK: Color = Color { r: 0.79130, g: 0.92158, b: 0.93011, a: 1.0 }; // #e6f6f7
const SOFT: Color = Color { r: 0.32778, g: 0.50289, b: 0.53328, a: 1.0 }; // #9bbcc1
const MUTED: Color = Color { r: 0.11444, g: 0.22697, b: 0.25016, a: 1.0 }; // #5f8389
const DIM: Color = Color { r: 0.02416, g: 0.05781, b: 0.06480, a: 1.0 }; // #2b4448
const ERR: Color = Color { r: 0.96469, g: 0.16513, b: 0.23455, a: 1.0 }; // #fb7185

/// Where everything sits: assets/layout.json, in the same form as MacySync's; each app of the family has its own
/// outline.
#[derive(serde::Deserialize)]
struct Layout {
    outline: Vec<[f32; 2]>,
    /// Straight edges between the outline's points, and lines that bend at 45°, instead of smooth curves.
    #[serde(default)]
    sharp: bool,
    elements: Elements,
    conduit: [[f32; 2]; 4],
    flow: FlowGuide,
}

#[derive(serde::Deserialize, Clone, Copy, Debug)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

#[derive(serde::Deserialize)]
struct Elements {
    title: Rect,
    /// A device's box is a 16-inch laptop's: the lid is LID of the width tall, the hinge sits there.
    laptop_left: Rect,
    laptop_right: Rect,
    left_panel: Rect,
    right_panel: Rect,
    controls: Rect,
    dialog: Rect,
    /// Send and Receive, between the two devices.
    actions: Rect,
}

#[derive(serde::Deserialize, Clone, Copy)]
struct Band {
    y: f32,
    spread: f32,
}

/// The background lines: where they fan out on the left, squeeze through the middle and fan out on the right (a
/// negative spread crosses them over), and the x ranges where they bend from one band to the next.
#[derive(serde::Deserialize, Clone, Copy)]
struct FlowGuide {
    left: Band,
    bridge: Band,
    right: Band,
    into_bridge: [f32; 2],
    into_right: [f32; 2],
}

fn layout() -> &'static Layout {
    static L: OnceLock<Layout> = OnceLock::new();
    L.get_or_init(|| serde_json::from_str(include_str!("../assets/layout.json")).expect("assets/layout.json"))
}

const POLL_S: f32 = 5.0;

pub fn is_tooling_arg(a: &str) -> bool {
    ["--snap", "--script", "--tree", "--list-scenes", "--scene", "--size", "--video", "--desktop", "--native", "--trace"].contains(&a)
}

enum Job {
    Connected(Result<Arc<Bus>, String>),
    Registered(Result<(), String>),
    Polled(Result<Snapshot, String>),
    Asked(Result<Request, String>),
    Answered(Result<Request, String>),
    /// A transfer's thread has the items to tick: this device's own when it sends, the offer when it receives.
    Items(String, Vec<Item>),
    /// A transfer's thread ended.
    Moved(String, Result<(), String>),
}

#[derive(Default)]
struct Snapshot {
    devices: Vec<Device>,
    requests: Vec<(Request, String)>,
    session: Option<Session>,
}

/// 03 PICK and 04 MOVE for the transfer running here.
#[derive(Default)]
struct Pick {
    transfer: String,
    items: Vec<Item>,
    ticks: HashSet<String>,
    open: HashSet<String>,
    /// Hands the ticked ids to the transfer's thread (None stops it); taken when the person presses a button.
    choose: Option<Sender<Option<Vec<String>>>>,
    live: Option<Arc<Mutex<Live>>>,
    /// 04 MOVE came up once the files started moving.
    shown: bool,
}

pub struct S {
    cfg: Config,
    /// The system this device runs.
    here: Os,
    secret: [u8; 32],
    now: f32,
    tab: u8,
    name: TextEdit,
    bus_edit: TextEdit,
    model_open: bool,
    bus: Option<Arc<Bus>>,
    devices: Vec<Device>,
    requests: Vec<(Request, String)>,
    /// Requests already settled when the bus was first read, and transfers the person closed: never picked up.
    old: HashSet<String>,
    /// Requests answered here, so a poll that started before the answer doesn't ask again.
    answered: HashSet<String>,
    peer: String,
    /// The request this device is answering in the dialog.
    incoming: Option<(Request, String)>,
    session: Option<Session>,
    polling: bool,
    polled_at: f32,
    /// Whether the bus has been read once since it connected.
    polled: bool,
    busy: String,
    err: String,
    jobs: (Sender<Job>, Receiver<Job>),
    waker: Option<Waker>,
    demo: bool,
    flow: Flow,
    /// The right side lists the account's devices to pick from.
    finding: bool,
    found_at: f32,
    hovered: String,
    /// 1 while the pointer is on Send, -1 on Receive: the lines show which way it would go.
    hover_dir: f32,
    pick: Pick,
}

/// Where the dashes are along their curves, as in WavySync: they speed up towards `dir` (1 runs left to right)
/// while work runs, and glide to a quarter-cycle stop after.
#[derive(Default)]
struct Flow {
    pos: f32,
    v: f32,
    dir: f32,
    stop: Option<(f32, f32)>,
}

const FLOW_V: f32 = 0.3;
const DASHES: usize = 4;

impl Flow {
    fn env(&self) -> f32 {
        (self.v.abs() / FLOW_V).clamp(0.0, 1.0)
    }

    fn step(&mut self, on: bool, dt: f32) {
        if on {
            self.stop = None;
            let a = FLOW_V / 0.8 * dt;
            self.v += (self.dir * FLOW_V - self.v).clamp(-a, a);
        } else if self.v != 0.0 {
            let sign = self.v.signum();
            let (target, decel) = *self.stop.get_or_insert_with(|| {
                let q = 1.0 / DASHES as f32;
                let t = sign * ((sign * (self.pos + self.v * 0.5)) / q).ceil() * q;
                (t, self.v * self.v / (2.0 * (t - self.pos).abs().max(1e-4)))
            });
            self.v = sign * (self.v.abs() - decel * dt).max(0.0);
            if self.v == 0.0 || sign * (self.pos + self.v * dt - target) >= 0.0 {
                *self = Flow { dir: self.dir, ..Flow::default() };
                return;
            }
        }
        self.pos += self.v * dt;
    }
}

fn fresh(r: &Request) -> bool {
    bus::now().saturating_sub(r.at) < 86400
}

fn init() -> S {
    let cfg = Config::load();
    S {
        here: Os::current(),
        secret: [0; 32],
        now: 0.0,
        tab: if cfg.registered { 2 } else { 1 },
        name: TextEdit::new(cfg.name.clone()),
        bus_edit: TextEdit::new(cfg.bus.clone()),
        model_open: false,
        bus: None,
        devices: Vec::new(),
        requests: Vec::new(),
        old: HashSet::new(),
        answered: HashSet::new(),
        peer: String::new(),
        incoming: None,
        session: None,
        polling: false,
        polled_at: -100.0,
        polled: false,
        busy: String::new(),
        err: String::new(),
        jobs: channel(),
        waker: None,
        demo: false,
        flow: Flow::default(),
        finding: false,
        found_at: 0.0,
        hovered: String::new(),
        hover_dir: 0.0,
        pick: Pick::default(),
        cfg,
    }
}

impl S {
    fn spawn(&mut self, busy: &str, f: impl FnOnce() -> Job + Send + 'static) {
        if self.demo {
            return;
        }
        if !busy.is_empty() {
            self.busy = busy.into();
        }
        let tx = self.jobs.0.clone();
        let w = self.waker.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
            if let Some(w) = w {
                w();
            }
        });
    }

    fn connect(&mut self) {
        let want = self.bus_edit.text.trim().to_string();
        self.spawn("Signing in to GitHub…", move || {
            Job::Connected((|| {
                let token = me::token().ok_or("Not signed in: run `gh auth login`, or set WINDYSYNC_TOKEN.")?;
                let repo = if want.is_empty() { format!("{}/windysync-bus", bus::github_login(&token)?) } else { want };
                bus::ensure_repo(&token, &repo)?;
                Ok(Arc::new(Bus::new(&repo, &token)))
            })())
        });
    }

    fn register(&mut self) {
        let (Some(b), cfg) = (self.bus.clone(), self.cfg.clone()) else { return };
        self.spawn("Saving this device to the bus…", move || {
            let d = me::device(&cfg);
            Job::Registered(b.put_json(&format!("devices/{}.json", d.id), &d, None, &format!("device {} ({})", d.name, d.id)).map(|_| ()))
        });
    }

    fn set_look(&mut self, look: Look) {
        self.cfg.look = look::fix(&look);
        self.cfg.save();
        if self.cfg.registered {
            self.register();
        }
    }

    fn poll_bus(&mut self) {
        let Some(b) = self.bus.clone() else { return };
        self.polling = true;
        let me = self.cfg.id.clone();
        let transfer = self.transfer().map(|r| r.transfer.clone());
        self.spawn("", move || {
            Job::Polled((|| {
                let mut snap = Snapshot::default();
                for n in b.list("devices")?.iter().filter(|n| n.ends_with(".json")) {
                    if let Some((d, _)) = b.get_json::<Device>(&format!("devices/{n}"))? {
                        snap.devices.push(d);
                    }
                }
                for n in b.list("requests")?.iter().filter(|n| n.ends_with(".json") && n.contains(&me)) {
                    if let Some(r) = b.get_json::<Request>(&format!("requests/{n}"))? {
                        snap.requests.push(r);
                    }
                }
                if let Some(t) = transfer {
                    snap.session = b.get_json::<Session>(&format!("{t}/session.json"))?.map(|s| s.0);
                }
                Ok(snap)
            })())
        });
    }

    /// The newest request between this device and another that waits for an answer, or was approved since the
    /// bus was first read.
    fn active_request(&self) -> Option<&Request> {
        self.requests.iter().map(|(r, _)| r)
            .filter(|r| !self.old.contains(&r.path()) && match r.answer {
                Answer::Pending => fresh(r),
                Answer::Accepted => true,
                _ => false,
            })
            .max_by_key(|r| r.at)
    }

    /// The approved request whose transfer folder exists.
    fn transfer(&self) -> Option<&Request> {
        self.active_request().filter(|r| r.answer == Answer::Accepted && !r.transfer.is_empty())
    }

    /// Whether this device sends in the current transfer; None before one is asked for. Each transfer picks its
    /// own direction, so any device can send to or take from any other.
    fn sending(&self) -> Option<bool> {
        self.active_request().map(|r| r.sides().0 == self.cfg.id)
    }

    fn peer_device(&self) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == self.peer)
    }

    /// The system the items land on: the other device's when this one sends, else this one's.
    fn landing_os(&self) -> Os {
        match (self.sending(), self.peer_device()) {
            (Some(true), Some(d)) => Os::parse(&d.os).or(Os::parse(&d.look.os)).unwrap_or(self.here),
            _ => self.here,
        }
    }

    fn live(&self) -> Option<Live> {
        self.pick.live.as_ref().map(|l| l.lock().unwrap().clone())
    }

    /// Lists the account's devices on the right, signing in first if needed.
    fn find(&mut self) {
        self.finding = true;
        self.found_at = self.now;
        self.peer.clear();
        if self.bus.is_none() {
            self.connect();
        } else {
            self.polled_at = -100.0;
        }
    }

    /// Asks the other device for a transfer in which this one sends (`Ask::Send`) or receives.
    fn ask(&mut self, ask: Ask) {
        let (Some(b), peer) = (self.bus.clone(), self.peer.clone()) else { return };
        if peer.is_empty() {
            return;
        }
        self.hover_dir = 0.0;
        let r = Request { ask, from: self.cfg.id.clone(), to: peer, at: bus::now(), answer: Answer::Pending, transfer: String::new() };
        self.spawn("Sending the request…", move || {
            Job::Asked(b.put_json(&r.path(), &r, None, &format!("{:?} request {} to {}", r.ask, r.from, r.to)).map(|_| r))
        });
    }

    fn answer(&mut self, yes: bool) {
        let (Some(b), Some((r, sha))) = (self.bus.clone(), self.incoming.take()) else { return };
        self.answered.insert(r.path());
        let mut names: std::collections::HashMap<String, String> = self.devices.iter().map(|d| (d.id.clone(), d.name.clone())).collect();
        names.insert(self.cfg.id.clone(), self.cfg.name.clone());
        self.spawn("Answering…", move || Job::Answered(session::approve(&b, r, &sha, &names, yes)));
    }

    /// Runs this side of the approved transfer `r` on a thread: the same send or receive as the command line,
    /// taking the ticks from 03 PICK and telling 04 MOVE how it goes.
    fn start(&mut self, r: Request) {
        let sender = r.sides().0 == self.cfg.id;
        let other = if r.from == self.cfg.id { &r.to } else { &r.from };
        let (Some(b), Some(other)) = (self.bus.clone(), self.devices.iter().find(|d| d.id == *other).cloned()) else { return };
        let live = Arc::new(Mutex::new(Live { state: "waiting".into(), note: "starting…".into(), ..Live::default() }));
        let (tx, rx) = channel();
        self.pick = Pick { transfer: r.transfer.clone(), choose: Some(tx), live: Some(live.clone()), ..Pick::default() };
        self.tab = 3;
        let (secret, t) = (self.secret, r.transfer);
        let ask = {
            let (jobs, w, t) = (self.jobs.0.clone(), self.waker.clone(), t.clone());
            move |items: Vec<Item>| -> Option<Vec<String>> {
                let _ = jobs.send(Job::Items(t, items));
                if let Some(w) = &w {
                    w();
                }
                rx.recv().ok().flatten()
            }
        };
        let on = {
            let w = self.waker.clone();
            move |l: &Live| {
                *live.lock().unwrap() = l.clone();
                if let Some(w) = &w {
                    w();
                }
            }
        };
        self.spawn("", move || {
            let here = Env::here();
            let r = if sender {
                session::send(&b, &secret, &t, &other, &here, |mut o| {
                    let ids = ask(o.items.clone())?;
                    o.items.retain(|i| ids.contains(&i.id));
                    Some(o)
                }, on)
            } else {
                session::receive(&b, &secret, &t, &other, &here, |o| ask(o.items.clone()).map(|ids| Picks { ids }), on).map(|_| ())
            };
            Job::Moved(t, r)
        });
    }

    /// Hands the ticked items to the transfer's thread, or stops the transfer.
    fn choose(&mut self, go: bool) {
        let ids: Vec<String> = self.pick.items.iter().filter(|i| self.pick.ticks.contains(&i.id)).map(|i| i.id.clone()).collect();
        if let Some(tx) = self.pick.choose.take() {
            let _ = tx.send(go.then_some(ids));
        }
        if !go {
            self.close();
        }
    }

    /// Leaves the transfer: its request counts as settled here, and the next one can be asked for.
    fn close(&mut self) {
        if let Some(r) = self.active_request() {
            self.old.insert(r.path());
        }
        self.pick = Pick::default();
        self.session = None;
        self.tab = 2;
    }

    fn land(&mut self, job: Job) {
        if !matches!(job, Job::Polled(_) | Job::Items(..)) {
            self.busy.clear();
        }
        match job {
            Job::Connected(Ok(b)) => {
                self.err.clear();
                self.cfg.bus = b.repo.clone();
                self.bus_edit = TextEdit::new(b.repo.clone());
                self.bus = Some(b);
                self.cfg.save();
                self.register();
            }
            Job::Registered(Ok(())) => {
                self.err.clear();
                if !self.cfg.registered {
                    self.cfg.registered = true;
                    self.cfg.save();
                    self.tab = 2;
                }
                self.polled_at = -100.0;
            }
            Job::Polled(r) => {
                self.polling = false;
                match r {
                    Ok(snap) => {
                        if !self.polled {
                            self.polled = true;
                            self.old = snap.requests.iter().filter(|(r, _)| !(r.answer == Answer::Pending && fresh(r))).map(|(r, _)| r.path()).collect();
                        }
                        self.devices = snap.devices.into_iter().filter(|d| d.id != self.cfg.id && look::ours(d)).collect();
                        self.devices.sort_by(|a, b| b.seen.cmp(&a.seen));
                        self.requests = snap.requests;
                        self.session = snap.session;
                        if self.incoming.is_none() {
                            self.incoming = self.requests.iter()
                                .find(|(r, _)| r.to == self.cfg.id && r.answer == Answer::Pending && fresh(r) && !self.answered.contains(&r.path()))
                                .cloned();
                        }
                        if let Some(r) = self.active_request().cloned() {
                            self.peer = if r.from == self.cfg.id { r.to } else { r.from };
                            self.finding = false;
                        }
                    }
                    Err(e) => self.err = e,
                }
            }
            Job::Asked(Ok(r)) | Job::Answered(Ok(r)) => {
                self.err.clear();
                self.requests.retain(|(x, _)| x.path() != r.path());
                self.requests.push((r, String::new()));
                self.polled_at = -100.0;
            }
            Job::Items(t, items) => {
                if t == self.pick.transfer {
                    let os = self.landing_os();
                    self.pick.ticks = items.iter().filter(|i| session::fits(i, os)).map(|i| i.id.clone()).collect();
                    self.pick.items = items;
                    self.tab = 3;
                }
            }
            Job::Moved(t, r) => {
                self.polled_at = -100.0;
                if let (true, Err(e)) = (t == self.pick.transfer, r) {
                    if let Some(l) = &self.pick.live {
                        let mut l = l.lock().unwrap();
                        l.state = "failed".into();
                        l.note = e.clone();
                    }
                    self.err = e;
                }
            }
            Job::Connected(Err(e)) | Job::Registered(Err(e)) | Job::Asked(Err(e)) | Job::Answered(Err(e)) => self.err = e,
        }
    }
}

fn setup(s: &mut S, waker: Waker) {
    s.waker = Some(waker);
    if s.demo {
        return;
    }
    s.secret = me::keys().0;
    if s.cfg.registered || !s.cfg.bus.is_empty() {
        s.connect();
    }
}

fn poll(s: &mut S, now: f64, tx: &mut Tx) -> bool {
    let now = now as f32;
    let dt = (now - s.now).clamp(0.0, 0.1);
    s.now = now;
    while let Ok(job) = s.jobs.1.try_recv() {
        s.land(job);
    }
    if let Some(r) = s.transfer().filter(|r| r.transfer != s.pick.transfer).cloned() {
        s.start(r);
    }
    let live = s.live();
    let moving = live.as_ref().is_some_and(|l| matches!(l.state.as_str(), "sending" | "receiving"));
    if moving && !s.pick.shown {
        s.pick.shown = true;
        s.tab = 4;
    }
    if !s.demo && s.bus.is_some() && !s.polling && now - s.polled_at > POLL_S {
        s.polled_at = now;
        s.poll_bus();
    }
    s.flow.dir = if s.hover_dir != 0.0 { s.hover_dir } else if s.sending() == Some(false) { -1.0 } else { 1.0 };
    s.flow.step(moving || !s.busy.is_empty() || s.hover_dir != 0.0, dt);
    let animating = s.flow.v != 0.0 || (s.finding && now - s.found_at < 1.2);
    tx.wake_after(Duration::from_millis(if animating { 0 } else { 250 }));
    true
}

// Drawing ----------------------------------------------------------------------------------------------

fn label(s: impl Into<String>, size: f32, weight: f32, c: Color) -> El {
    text(s.into()).font(TextStyle::new(size, weight)).color(c)
}
fn mono(s: impl Into<String>, size: f32, c: Color) -> El {
    text(s.into()).font(TextStyle::new(size, 500.0).mono()).color(c)
}
fn caps(s: impl Into<String>, c: Color) -> El {
    text(s.into()).font(TextStyle::new(11.0, 700.0).mono().tracking(1.4)).color(c)
}
fn centred(e: El) -> El {
    row().w_full().child(spacer()).child(e).child(spacer())
}

/// The outline through the layout's points, smoothed (Catmull-Rom), or straight from point to point when the layout
/// is sharp.
fn segments() -> Vec<([f32; 2], [f32; 2], [f32; 2], [f32; 2])> {
    let o = &layout().outline;
    let n = o.len();
    (0..n)
        .map(|i| {
            let (p0, p1, p2, p3) = (o[(i + n - 1) % n], o[i], o[(i + 1) % n], o[(i + 2) % n]);
            // Sharp: the control points on the straight line, a third of the way in from each end.
            let ((a, b), k) = if layout().sharp { ((p1, p2), 1.0 / 3.0) } else { ((p0, p3), 1.0 / 6.0) };
            let c1 = [p1[0] + (p2[0] - a[0]) * k, p1[1] + (p2[1] - a[1]) * k];
            let c2 = [p2[0] - (b[0] - p1[0]) * k, p2[1] - (b[1] - p1[1]) * k];
            (p1, c1, c2, p2)
        })
        .collect()
}

fn body_path() -> Path {
    segments().iter().fold(Path::new().move_to(layout().outline[0]), |p, (_, a, b, c)| p.cubic_to(*a, *b, *c)).close()
}

fn bezier((p0, a, b, c): ([f32; 2], [f32; 2], [f32; 2], [f32; 2]), t: f32) -> [f32; 2] {
    let u = 1.0 - t;
    let k = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    [k[0] * p0[0] + k[1] * a[0] + k[2] * b[0] + k[3] * c[0], k[0] * p0[1] + k[1] * a[1] + k[2] * b[1] + k[3] * c[1]]
}

/// Distance from `p` to the outline, positive inside.
fn depth(poly: &[[f32; 2]], p: [f32; 2]) -> f32 {
    let (mut inside, mut d) = (false, f32::MAX);
    for (i, a) in poly.iter().enumerate() {
        let b = poly[(i + 1) % poly.len()];
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]) {
            inside = !inside;
        }
        let (vx, vy) = (b[0] - a[0], b[1] - a[1]);
        let t = (((p[0] - a[0]) * vx + (p[1] - a[1]) * vy) / (vx * vx + vy * vy).max(1e-6)).clamp(0.0, 1.0);
        d = d.min((a[0] + vx * t - p[0]).hypot(a[1] + vy * t - p[1]));
    }
    if inside { d } else { -d }
}

const LINES: usize = 15;
const SAMPLES: usize = 200;

/// A sampled curve from one lobe to the other, and how deep inside the body each sample is (Rune has no clipping,
/// so the lines are cut and faded by this).
struct FlowLine {
    pts: Vec<[f32; 2]>,
    depth: Vec<f32>,
    inside: Vec<std::ops::Range<usize>>,
    offset: f32,
    len: f32,
    speed: f32,
}

fn hash(n: u32) -> f32 {
    let mut x = n.wrapping_mul(0x9E37_79B9) ^ 0x85EB_CA6B;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    (x & 0xFFFF) as f32 / 65535.0
}

fn flow_lines() -> &'static [FlowLine] {
    static L: OnceLock<Vec<FlowLine>> = OnceLock::new();
    L.get_or_init(|| {
        let poly: Vec<[f32; 2]> = segments().into_iter().flat_map(|s| (1..=16).map(move |i| bezier(s, i as f32 / 16.0))).collect();
        (0..LINES)
            .map(|i| {
                let o = -1.0 + 2.0 * i as f32 / (LINES - 1) as f32;
                // They fan out on each side and squeeze through the middle.
                let f = layout().flow;
                let (yl, yb, yr) = (f.left.y + o * f.left.spread, f.bridge.y + o * f.bridge.spread, f.right.y + o * f.right.spread);
                let wobble = if layout().sharp { 0.0 } else { 4.0 + 6.0 * hash(i as u32 * 7 + 1) };
                let shift = hash(i as u32 * 13 + 5) * std::f32::consts::TAU;
                let pts: Vec<[f32; 2]> = (0..SAMPLES)
                    .map(|j| {
                        let x = -20.0 + 1320.0 * j as f32 / (SAMPLES - 1) as f32;
                        // How far the line has moved by `dy` over the range: smoothly, or at 45° in its middle as a
                        // circuit trace bends.
                        let bend = |[a, b]: [f32; 2], dy: f32| {
                            if layout().sharp {
                                dy * ((x - (a + b) / 2.0) / dy.abs().max(1.0) + 0.5).clamp(0.0, 1.0)
                            } else {
                                let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
                                dy * t * t * (3.0 - 2.0 * t)
                            }
                        };
                        let y = yl + bend(f.into_bridge, yb - yl) + bend(f.into_right, yr - yb) + wobble * (x / 160.0 + shift).sin();
                        [x, y]
                    })
                    .collect();
                let depth: Vec<f32> = pts.iter().map(|p| depth(&poly, *p)).collect();
                let mut inside = Vec::new();
                let mut from = None;
                for (j, d) in depth.iter().chain([&-1.0]).enumerate() {
                    match (from, *d > 6.0) {
                        (None, true) => from = Some(j),
                        (Some(a), false) => {
                            if j - a > 1 {
                                inside.push(a..j);
                            }
                            from = None;
                        }
                        _ => {}
                    }
                }
                let (offset, len) = (hash(i as u32 * 61 + 9), 0.03 + 0.024 * hash(i as u32 * 17 + 3));
                FlowLine { pts, depth, inside, offset, len, speed: if i % 3 == 0 { 2.0 } else { 1.0 } }
            })
            .collect()
    })
}

fn backdrop(s: &S) -> Vec<El> {
    let env = s.flow.env();
    let body = body_path();
    let mut out = vec![full(Shape::new().fill_path(&body, BG)).window_drag().key("body")];
    let (mut faint, mut glow, mut core) = (Shape::new(), Shape::new(), Shape::new());
    for (i, l) in flow_lines().iter().enumerate() {
        // Violet, with a magenta and a teal thread: the logo's crossing lines.
        let tint = match i % 5 {
            1 => ACCENT2,
            3 => ACCENT3,
            _ => ACCENT,
        };
        for r in &l.inside {
            faint = faint.stroke(l.pts[r.clone()].to_vec(), 1.0, Color { a: 0.07 + 0.06 * env, ..tint });
        }
        for j in 0..DASHES {
            let u = (l.offset + j as f32 / DASHES as f32 + l.speed * s.flow.pos).rem_euclid(1.0);
            let last = (SAMPLES - 1) as f32;
            let (a, b) = ((u * last) as usize, (((u + l.len) * last) as usize).min(SAMPLES - 1));
            if b <= a + 1 {
                continue;
            }
            let fade = ((l.depth[a..=b].iter().copied().fold(f32::MAX, f32::min) - 2.0) / 28.0).clamp(0.0, 1.0);
            if fade == 0.0 {
                continue;
            }
            let pts = l.pts[a..=b].to_vec();
            glow = glow.stroke(pts.clone(), 7.0, Color { a: (0.16 + 0.5 * env) * fade, ..tint });
            core = core.stroke(pts, 1.6, Color { a: (0.36 + 0.55 * env) * fade, ..tint });
        }
    }
    out.push(full(faint).key("faint"));
    out.push(full(glow).blur(6.0).key("glow"));
    out.push(full(core).blur(0.6).key("core"));
    out.push(full(Shape::new().stroke_path(&body, 2.0, LineCap::Round, EDGE)).key("edge"));
    // The outline lights up violet while files move. No blurred glow: Rune's blur over the transparent desktop
    // outside the body comes out as a hard, oversaturated band.
    out.push(full(Shape::new().stroke_path(&body, 1.0 + 0.6 * env, LineCap::Round, Color { a: 0.35 + 0.4 * env, ..STEEL.mix(ACCENT, env) })).key("outline-line"));
    out
}

/// The conduit between the devices: two rails, and chevrons that run from the sender to the receiver.
fn conduit(s: &S) -> Vec<El> {
    let env = s.flow.env();
    let [a, c1, c2, b] = layout().conduit;
    let rail = |dy: f32| Path::new().move_to([a[0], a[1] + dy]).cubic_to([c1[0], c1[1] + dy], [c2[0], c2[1] + dy], [b[0], b[1] + dy]);
    let mut shape = Shape::new();
    for dy in [-16.0, 16.0] {
        shape = shape.stroke_path(&rail(dy), 2.0, LineCap::Round, Color { a: 0.85, ..STEEL });
    }
    let mut out = vec![
        full(Shape::new().stroke_path(&rail(0.0), 30.0, LineCap::Round, Color { a: 0.55, ..FIELD })).key("conduit-bed"),
        full(shape).key("conduit-rails"),
    ];
    let n = 7;
    let mut chev = Shape::new();
    for k in 0..n {
        let u = ((k as f32 + s.flow.pos * 3.0 * s.flow.dir.signum() * s.flow.dir) / n as f32).rem_euclid(1.0);
        let seg = (a, c1, c2, b);
        let p = bezier(seg, u);
        let q = bezier(seg, (u + 0.01).min(1.0));
        let d = if s.flow.dir >= 0.0 { 1.0 } else { -1.0 };
        let ang = (q[1] - p[1]).atan2(q[0] - p[0]);
        let (c, sn) = (ang.cos() * d, ang.sin() * d);
        let pt = |x: f32, y: f32| [p[0] + x * c - y * sn, p[1] + x * sn + y * c];
        let alpha = (0.25 + 0.75 * env) * (std::f32::consts::PI * u).sin();
        chev = chev.stroke(vec![pt(-5.0, -8.0), pt(4.0, 0.0), pt(-5.0, 8.0)], 2.4, Color { a: alpha, ..if k % 2 == 0 { ACCENT } else { LIGHT } });
    }
    out.push(full(chev.clone()).blur(4.0).key("chev-glow"));
    out.push(full(chev).key("chev"));
    out
}

/// A device in its big place, with its name and look on its screen.
fn device_on_screen(key: &str, name: &str, id: &str, look: &Look, place: Rect, role: String, this: bool) -> Vec<El> {
    let drawn = look::device(key, look, place.x, place.y, place.w);
    let [x, y, w, h] = drawn.screen;
    let mut out = drawn.layers;
    let l = look::fix(look);
    let mut screen = col().absolute(x, y).size(w, h).padding(14.0).gap(4.0)
        .child(row().w_full().center_y().child(caps(role, if this { ACCENT3 } else { LIGHT })).child(spacer()).child(mono(if this { "THIS DEVICE" } else { "" }, 9.5, MUTED)))
        .child(spacer())
        .child(label(if name.is_empty() { "No device picked" } else { name }, 19.0, 700.0, if name.is_empty() { MUTED } else { INK }))
        .child(mono(look::describe(&l), 10.5, SOFT))
        .child(mono(l.color, 10.0, MUTED));
    if !id.is_empty() {
        screen = screen.child(row().mt(4.0).child(el().fill(PANEL2).radius(5.0).px(6.0).py(1.0).child(mono(format!("#{id}"), 10.0, LIGHT))));
    }
    out.push(screen.key(format!("{key}-screen")));
    out
}

/// A small device with its name on the screen; `lit` puts a halo round it. `alpha` fades it all.
fn mini(key: &str, d: &Device, r: Rect, lit: bool, alpha: f32) -> Vec<El> {
    let drawn = look::device(key, &d.look, r.x, r.y, r.w);
    let [x, y, w, h] = drawn.screen;
    let mut out: Vec<El> = drawn.layers.into_iter().map(|e| e.opacity(alpha)).collect();
    if lit {
        let halo = el().key(format!("{key}-halo")).absolute(x - w * 0.08, y - w * 0.08).size(w * 1.16, h + w * 0.16).radius(w * 0.08)
            .fill(Color { a: 0.32, ..ACCENT }).blur(w * 0.09).opacity(alpha);
        out.insert(1.min(out.len()), halo);
    }
    if !d.name.is_empty() {
        let size = (w / (d.name.chars().count().max(5) as f32 * 0.6)).min(h * 0.3).min(22.0);
        out.push(col().key(format!("{key}-name")).absolute(x, y).size(w, h).center().gap(size * 0.2).opacity(alpha)
            .child(label(d.name.clone(), size, 700.0, if lit { INK } else { SOFT }))
            .child(mono(format!("#{}", d.id), (size * 0.5).max(8.0), LIGHT)));
    }
    out
}

/// Device boxes for `n` devices in the right-hand place, rows of up to three, a short last row centred.
fn grid_cells(n: usize) -> Vec<Rect> {
    let r = layout().elements.laptop_right;
    let cols = match n {
        0 | 1 => 1,
        2 | 4 => 2,
        _ => 3,
    };
    let rows = n.div_ceil(cols).max(1);
    let (ax, ay, aw, ah) = (r.x - 40.0, r.y + 40.0, r.w + 80.0, r.h - 40.0);
    let (cw, ch) = (aw / cols as f32, ah / rows as f32);
    let w = (cw * 0.78).min(ch * 0.85 / 0.935).min(170.0);
    (0..n).map(|i| {
        let (c, row_) = (i % cols, i / cols);
        let in_row = (n - row_ * cols).min(cols);
        let x0 = ax + (aw - in_row as f32 * cw) / 2.0 + c as f32 * cw;
        Rect { x: x0 + (cw - w) / 2.0, y: ay + row_ as f32 * ch + (ch - w * 0.935) / 2.0, w, h: w * 0.935 }
    }).collect()
}

/// The account's devices, each in its own look; the one under the pointer glows, a click picks it.
fn grid(s: &S) -> Vec<El> {
    let r = layout().elements.laptop_right;
    let status = if !s.err.is_empty() {
        (s.err.clone(), ERR)
    } else if !s.polled {
        ("Looking on GitHub…".into(), SOFT)
    } else if s.devices.is_empty() {
        ("No other device yet: open WindySync on it and save it here.".into(), SOFT)
    } else {
        ("Click the device to work with.".into(), MUTED)
    };
    let mut out = vec![col().key("grid-head").absolute(r.x - 40.0, r.y - 16.0).w(r.w + 80.0).gap(4.0)
        .child(centred(caps("DEVICES ON THIS ACCOUNT", LIGHT)))
        .child(centred(label(status.0, 11.5, 500.0, status.1)))];
    for (i, (d, cell)) in s.devices.iter().zip(grid_cells(s.devices.len())).enumerate() {
        let alpha = ((s.now - s.found_at - 0.07 * i as f32) / 0.35).clamp(0.0, 1.0);
        if alpha <= 0.0 {
            continue;
        }
        let key = format!("g-{}", d.id);
        out.extend(mini(&key, d, cell, s.hovered == d.id, alpha));
        let (id, id2) = (d.id.clone(), d.id.clone());
        out.push(el().key(format!("{key}-hit")).absolute(cell.x - cell.w * 0.07, cell.y).size(cell.w * 1.14, cell.h).cursor(Cursor::Pointer)
            .help(format!("{} · {} · {}", look::describe(&d.look), d.user, ago(d.seen)))
            .on_hover_change(move |s: &mut S, inside, _| {
                if inside {
                    s.hovered = id.clone();
                } else if s.hovered == id {
                    s.hovered.clear();
                }
            })
            .on_press(move |s: &mut S| {
                s.peer = id2.clone();
                s.finding = false;
                s.hovered.clear();
            }));
    }
    out
}

/// The right-hand place before a device is picked: a dim one with its screen off, and the find button.
fn find_prompt(s: &S) -> Vec<El> {
    let r = layout().elements.laptop_right;
    let ghost = Device { look: s.cfg.look.clone(), ..Device::default() };
    let mut out = mini("right", &ghost, r, false, 0.3);
    let (cx, cy) = (r.x + r.w / 2.0, r.y + r.w * LID * 0.5);
    let note = if s.cfg.bus.is_empty() { "signs in to GitHub".to_string() } else { s.cfg.bus.clone() };
    out.push(col().key("find").absolute(cx - 150.0, cy - 30.0).w(300.0).gap(8.0)
        .child(centred(button("Find devices on this account", true).key("find-btn").on_press(|s: &mut S| s.find())))
        .child(centred(mono(note, 10.0, MUTED))));
    out
}

/// Send and Receive between the two devices: the direction is chosen per transfer. Pointing at one runs the lines
/// that way first.
fn actions(s: &S) -> Option<El> {
    let d = s.peer_device().filter(|_| !s.finding)?;
    let r = layout().elements.actions;
    if let Some(req) = s.active_request() {
        return (req.answer == Answer::Pending && req.from == s.cfg.id).then(|| {
            col().key("actions").absolute(r.x - 30.0, r.y + r.h / 2.0 - 52.0).w(r.w + 60.0).child(centred(mono("waiting for approval…", 10.5, LIGHT)))
        });
    }
    let pill = |key: &str, text: &str, dir: f32, help: String| {
        let on = s.hover_dir == dir;
        el().key(key).w_full().h(34.0).center().radius(17.0).cursor(Cursor::Pointer)
            .fill(if on { DEEP } else { PANEL2 }).border(1.0, if on { ACCENT } else { EDGE })
            .shadow(10.0, 24.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.45 })
            .child(label(text, 13.0, 700.0, INK))
            .help(help)
            .on_hover_change(move |s: &mut S, inside, _| {
                if inside {
                    s.hover_dir = dir;
                } else if s.hover_dir == dir {
                    s.hover_dir = 0.0;
                }
            })
            .on_press(move |s: &mut S| s.ask(if dir > 0.0 { Ask::Send } else { Ask::Receive }))
    };
    Some(col().key("actions").absolute(r.x, r.y).size(r.w, r.h)
        .child(pill("send", "Send  ⟶", 1.0, format!("This device sends to {}", d.name)))
        .child(spacer())
        .child(pill("receive", "⟵  Receive", -1.0, format!("This device takes from {}", d.name))))
}

fn card(h: f32) -> El {
    col().w_full().h(h).fill(PANEL).border(1.0, EDGE).radius(14.0).px(14.0).py(12.0).gap(7.0)
}

fn button(s: impl Into<String>, primary: bool) -> El {
    el().px(12.0).py(7.0).radius(9.0)
        .fill(if primary { DEEP } else { PANEL2 })
        .border(1.0, if primary { ACCENT } else { EDGE })
        .hover(if primary { ACCENT } else { EDGE })
        .cursor(Cursor::Pointer)
        .child(label(s, 12.5, 600.0, INK))
}

/// One choice of a few side by side.
fn seg(key: String, s: &str, on: bool) -> El {
    el().key(key).px(10.0).h(28.0).center().radius(7.0).cursor(Cursor::Pointer)
        .fill(if on { DEEP } else { FIELD }).border(1.0, if on { ACCENT } else { EDGE }).hover(if on { ACCENT } else { EDGE })
        .child(label(s, 12.0, if on { 650.0 } else { 500.0 }, if on { INK } else { SOFT }))
}

fn trigger(key: &str, s: impl Into<String>) -> El {
    row().key(key).w_full().h(32.0).px(10.0).center_y().fill(FIELD).border(1.0, EDGE).radius(8.0).cursor(Cursor::Pointer)
        .child(label(s, 13.0, 500.0, INK))
        .child(spacer())
        .child(label("▾", 12.0, 600.0, MUTED))
}

fn menu(key: &str, w: f32, h: f32, items: Vec<El>) -> El {
    let list = col().w_full().gap(1.0).children(items);
    col().fill(FIELD).border(1.0, EDGE).radius(10.0).padding(4.0).w(w)
        .shadow(18.0, 40.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.5 })
        .child(list.scroll(ScrollSpec { stick_bottom: false }).scroll_edge_fade(10.0).h(h))
        .anchored(key, Side::Bottom, Align::Start, 4.0)
}

fn menu_row(s: impl Into<String>, on: bool) -> El {
    row().w_full().px(9.0).py(6.0).radius(7.0).hover(PANEL2).cursor(Cursor::Pointer).fill(if on { PANEL2 } else { Color { a: 0.0, ..PANEL2 } })
        .child(label(s, 12.5, if on { 700.0 } else { 500.0 }, if on { LIGHT } else { INK }))
}

fn kit() -> Kit {
    Kit::from_palette(PaletteSeed { bg: FIELD, fg: INK, accent: DEEP })
}

fn note(s: &str) -> El {
    col().w_full().gap(6.0).child(label(s, 13.0, 500.0, SOFT).wrap().w_full())
}

/// 01 DEVICE: the name, and the look in any of the three systems' types; it is only how the device is drawn.
fn tab_device(s: &S, k: &Kit) -> El {
    let l = look::fix(&s.cfg.look);
    let mut systems = row().gap(4.0);
    for (key, shown) in look::SYSTEMS {
        let n = look::with_os(&l, key);
        systems = systems.child(seg(format!("os-{key}"), shown, l.os == *key).on_press(move |s: &mut S| s.set_look(n.clone())));
    }
    let mut kinds = row().gap(4.0);
    for kind in look::KINDS {
        let n = Look { kind: kind.into(), ..l.clone() };
        kinds = kinds.child(seg(format!("kind-{kind}"), if kind == "laptop" { "Laptop" } else { "Desktop" }, l.kind == kind).on_press(move |s: &mut S| s.set_look(n.clone())));
    }
    let mut c = col().w_full().gap(8.0)
        .child(row().w_full().gap(10.0).center_y()
            .child(col().gap(4.0).child(caps("NAME", MUTED))
                .child(TextInput::new(&s.name).width(Some(250.0)).on_edit(|s: &mut S| &mut s.name).on_change(|s: &mut S, _| {
                    s.cfg.name = s.name.text.trim().to_string();
                    s.cfg.save();
                }).build(k)))
            .child(col().gap(4.0).child(caps("ID", MUTED)).child(el().h(32.0).center_y().px(10.0).fill(PANEL2).radius(8.0).child(mono(format!("#{}", s.cfg.id), 13.0, LIGHT)))
                .help("Made on this device's first run; it never changes.")))
        .child(row().w_full().center_y().child(caps("LOOK", MUTED)).child(spacer()).child(mono(format!("runs {}", s.here.name()), 10.5, MUTED)))
        .child(row().w_full().center_y().gap(8.0).child(systems).child(spacer()).child(kinds));
    let models = look::models(&l);
    if let Some((_, shown)) = models.iter().find(|m| m.0 == l.model) {
        c = c.child(trigger("model-dd", shown.clone()).on_press(|s: &mut S| s.model_open = !s.model_open));
    }
    let mut sw = row().gap(8.0).center_y();
    for (name, hex) in look::colours(&l) {
        let on = name == l.color;
        let n = Look { color: name.clone(), ..l.clone() };
        sw = sw.child(el().key(format!("sw-{name}")).size(24.0, 24.0).radius(12.0).fill(look::hx(&hex, STEEL)).border(if on { 2.5 } else { 1.0 }, if on { ACCENT } else { EDGE })
            .cursor(Cursor::Pointer).help(name).on_press(move |s: &mut S| s.set_look(n.clone())));
    }
    c = c.child(row().w_full().center_y().gap(10.0).child(sw).child(spacer()).child(mono(l.color.clone(), 11.5, SOFT)));
    let action = if s.bus.is_none() { "Sign in and save" } else if s.cfg.registered { "Save again" } else { "Save to the bus" };
    c.child(row().w_full().center_y().gap(8.0)
        .child(TextInput::new(&s.bus_edit).placeholder("owner/windysync-bus (empty: yours)").width(Some(300.0)).on_edit(|s: &mut S| &mut s.bus_edit).build(k))
        .child(spacer())
        .child(button(action, true).key("save").on_press(|s: &mut S| if s.bus.is_none() { s.connect() } else { s.register() })))
}

fn ago(t: u64) -> String {
    let d = bus::now().saturating_sub(t);
    if d < 90 { "online".into() } else if d < 3600 { format!("{} min ago", d / 60) } else if d < 86400 { format!("{} h ago", d / 3600) } else { format!("{} d ago", d / 86400) }
}

fn tab_connect(s: &S) -> El {
    let req = s.active_request();
    let picked = s.peer_device().is_some() && !s.finding;
    let done = [s.finding || picked, picked, req.is_some(), req.is_some_and(|r| r.answer == Answer::Accepted)];
    let steps = [
        "Find the devices on this account, on the right.",
        "Click the device to work with.",
        "Press Send or Receive between the two. It holds for this transfer only.",
        "The other device approves.",
    ];
    let current = done.iter().position(|d| !d).unwrap_or(steps.len());
    let mut c = col().w_full().gap(10.0);
    for (i, text) in steps.into_iter().enumerate() {
        let now = i == current;
        let (mark, fg, bg) = if done[i] { ("✓".to_string(), INK, DEEP) } else if now { ((i + 1).to_string(), INK, PANEL2) } else { ((i + 1).to_string(), MUTED, FIELD) };
        c = c.child(row().w_full().gap(10.0).center_y()
            .child(el().size(22.0, 22.0).radius(11.0).center().fill(bg).border(1.0, if now { ACCENT } else { EDGE }).child(mono(mark, 11.0, fg)))
            .child(label(text, 13.0, if now { 600.0 } else { 500.0 }, if now { INK } else if done[i] { SOFT } else { MUTED }).wrap().grow(1.0)));
    }
    let declined = s.requests.iter().map(|(r, _)| r).filter(|r| r.from == s.cfg.id && !s.old.contains(&r.path())).max_by_key(|r| r.at)
        .is_some_and(|r| r.answer == Answer::Declined && req.is_none());
    let state = match req {
        Some(r) if r.answer == Answer::Pending && r.from == s.cfg.id => "Waiting for the other device to approve…",
        Some(r) if r.answer == Answer::Accepted => "Approved. Next: 03 PICK.",
        _ if declined => "The other device declined.",
        _ => "",
    };
    c.child(label(state, 12.0, 600.0, LIGHT).wrap().w_full())
}

fn group_title(g: &str) -> String {
    if g == "ai" { "AI".into() } else { g[..1].to_uppercase() + &g[1..] }
}

/// A tick box: full, partly (a minus) or empty.
fn tick(key: String, on: usize, all: usize, enabled: bool) -> El {
    let mark = if all > 0 && on == all { "✓" } else if on > 0 { "−" } else { "" };
    el().key(key).size(16.0, 16.0).radius(4.0).center()
        .fill(if on == 0 { FIELD } else { DEEP }).border(1.0, if !enabled { DIM } else if on == 0 { EDGE } else { ACCENT })
        .child(mono(mark, 10.0, INK))
}

fn tab_pick(s: &S) -> El {
    if s.pick.transfer.is_empty() {
        return note("Approve a transfer in 02 CONNECT first. Then the sender ticks what it offers and the receiver what it takes: only those move.");
    }
    let sender = s.sending() == Some(true);
    let other = s.peer_device().map(|d| d.name.clone()).unwrap_or_default();
    let open = s.pick.choose.is_some();
    if s.pick.items.is_empty() {
        let msg = if sender { "Looking at what this device has…".to_string() } else { format!("Waiting for the offer from {other}…") };
        let mut c = note(&msg);
        if open {
            c = c.child(row().w_full().child(spacer()).child(button("Cancel", false).key("cancel").on_press(|s: &mut S| s.choose(false))));
        }
        return c;
    }
    let os = s.landing_os();
    let mut rows = Vec::new();
    let mut total = (0, 0);
    for (g, items) in session::groups(&s.pick.items) {
        let fit: Vec<String> = items.iter().filter(|i| session::fits(i, os)).map(|i| i.id.clone()).collect();
        let on = fit.iter().filter(|id| s.pick.ticks.contains(*id)).count();
        let nfit = fit.len();
        total = (total.0 + on, total.1 + nfit);
        let shown = s.pick.open.contains(&g);
        let mut t = tick(format!("tick-{g}"), on, fit.len(), open && !fit.is_empty());
        if open && !fit.is_empty() {
            t = t.cursor(Cursor::Pointer).on_press(move |s: &mut S| {
                let all = fit.iter().all(|id| s.pick.ticks.contains(id));
                for id in &fit {
                    if all { s.pick.ticks.remove(id) } else { s.pick.ticks.insert(id.clone()) };
                }
            });
        }
        let g2 = g.clone();
        rows.push(row().key(format!("group-{g}")).w_full().h(24.0).gap(6.0).center_y().px(4.0).radius(5.0).fill(Color { a: 0.0, ..PANEL2 }).hover(PANEL2)
            .child(el().size(14.0, 16.0).center().cursor(Cursor::Pointer).child(label(if shown { "▾" } else { "▸" }, 11.0, 600.0, MUTED))
                .on_press(move |s: &mut S| {
                    if !s.pick.open.remove(&g2) {
                        s.pick.open.insert(g2.clone());
                    }
                }))
            .child(t)
            .child(label(group_title(&g), 12.5, 650.0, if on == 0 { SOFT } else { INK }).grow(1.0))
            .child(mono(if nfit < items.len() { format!("+{} no place", items.len() - nfit) } else { String::new() }, 9.5, DIM))
            .child(mono(format!("{on}/{nfit}"), 11.0, SOFT)));
        if !shown {
            continue;
        }
        for i in items {
            let fits = session::fits(i, os);
            let ticked = s.pick.ticks.contains(&i.id);
            let mut t = tick(format!("tick-{}", i.id), ticked as usize, 1, open && fits);
            if open && fits {
                let id = i.id.clone();
                t = t.cursor(Cursor::Pointer).on_press(move |s: &mut S| {
                    if !s.pick.ticks.remove(&id) {
                        s.pick.ticks.insert(id.clone());
                    }
                });
            }
            let mut r = row().key(format!("item-{}", i.id)).w_full().h(22.0).gap(6.0).center_y().px(4.0).radius(5.0).fill(Color { a: 0.0, ..PANEL2 }).hover(PANEL2)
                .child(el().w(34.0)).child(t)
                .child(label(i.name.clone(), 12.5, 500.0, if !fits { DIM } else if ticked { INK } else { MUTED }).grow(1.0));
            // A person's folder item: which folder it is from.
            if let Some(folder) = i.id.split('/').nth(1).filter(|_| i.id.starts_with("files/")) {
                r = r.child(mono(folder, 10.0, MUTED));
            }
            if i.secret {
                r = r.child(mono("secret", 9.5, ACCENT2));
            }
            if !fits {
                r = r.child(mono(format!("no place on {}", os.name()), 9.5, DIM));
            }
            if !i.note.is_empty() {
                r = r.help(i.note.clone());
            }
            rows.push(r);
        }
    }
    let action = if open {
        let (go, key) = if sender { (format!("Offer {}", total.0), "offer") } else { (format!("Take {}", total.0), "take") };
        row().gap(8.0)
            .child(button("Cancel", false).key("cancel").on_press(|s: &mut S| s.choose(false)))
            .child(button(go, true).key(key).on_press(|s: &mut S| s.choose(true)))
    } else {
        row().child(label(s.live().map(|l| l.note).unwrap_or_default(), 12.0, 600.0, LIGHT))
    };
    col().w_full().gap(6.0)
        .child(row().w_full().center_y()
            .child(caps(if sender { "TICK WHAT YOU OFFER" } else { "TICK WHAT YOU TAKE" }, MUTED))
            .child(spacer())
            .child(mono(format!("lands on {}", os.name()), 10.5, MUTED)))
        .child(col().w_full().gap(1.0).children(rows).scroll(ScrollSpec { stick_bottom: false }).scroll_edge_fade(10.0).h(170.0))
        .child(row().w_full().h(32.0).center_y().gap(8.0)
            .child(mono(format!("{}/{} items", total.0, total.1), 11.5, SOFT))
            .child(spacer())
            .child(action))
}

/// Opens the transfer folder in the system's file manager; only ever on the person's press.
fn open_folder(dir: &std::path::Path) {
    let cmd = match Os::current() {
        Os::Mac => "open",
        Os::Windows => "explorer",
        Os::Linux => "xdg-open",
    };
    let _ = std::process::Command::new(cmd).arg(dir).spawn();
}

fn tab_move(s: &S) -> El {
    let Some(l) = s.live() else {
        return note("Nothing is moving yet. After 03 PICK the files go device to device, encrypted end to end, and land at the receiving system's own paths.");
    };
    let failed = l.state == "failed";
    let state = match l.state.as_str() {
        "sending" => "SENDING",
        "receiving" => "RECEIVING",
        "placing" => "PUTTING THINGS IN PLACE",
        "done" => "DONE",
        "failed" => "STOPPED",
        _ => "WAITING",
    };
    let pct = if l.items > 0 { l.items_done as f32 / l.items as f32 } else if l.state == "done" { 1.0 } else { 0.0 };
    let w = layout().elements.left_panel.w - 28.0;
    let mut c = col().w_full().gap(7.0)
        .child(row().w_full().center_y().child(caps(state, if failed { ERR } else { ACCENT })).child(spacer())
            .child(mono(format!("{}/{} items", l.items_done, l.items), 11.0, SOFT)))
        .child(mono(fmt_bytes(l.bytes), 12.5, INK))
        .child(el().w_full().h(6.0).radius(3.0).fill(FIELD).child(el().h(6.0).w(w * pct).radius(3.0).linear_gradient(1.57, [(0.0, DEEP), (0.6, ACCENT2), (1.0, ACCENT3)])))
        .child(label(l.current.clone(), 12.0, 500.0, MUTED));
    if l.state != "done" && !l.note.is_empty() {
        c = c.child(label(l.note.clone(), 11.5, 600.0, if failed { ERR } else { SOFT }).wrap().w_full());
    }
    let again = button("New transfer", true).key("again").on_press(|s: &mut S| s.close());
    match (l.state.as_str(), l.dir) {
        ("done", Some(dir)) => {
            let goal = dir.join("GOAL.md").display().to_string();
            c.child(label(format!("{} in place · {} to look at · {} missing", l.placed, l.problems, l.missing), 13.0, 650.0, INK))
                .child(label("An agent finishes the setup from the brief:", 12.0, 500.0, SOFT))
                .child(text(goal).font(TextStyle::new(11.0, 500.0).mono()).color(LIGHT).wrap().w_full())
                .child(row().w_full().gap(8.0).center_y().child(spacer())
                    .child(button("Open folder", false).key("open-dir").on_press(move |_: &mut S| open_folder(&dir)))
                    .child(again))
        }
        ("done", None) => {
            let other = s.peer_device().map(|d| d.name.clone()).unwrap_or_default();
            c.child(label(format!("Everything went across. {other} puts it in place and writes GOAL.md for an agent."), 12.5, 500.0, SOFT).wrap().w_full())
                .child(row().w_full().child(spacer()).child(again))
        }
        ("failed", _) => c.child(row().w_full().child(spacer()).child(again)),
        _ => c,
    }
}

fn left_panel(s: &S, k: &Kit) -> El {
    let names = ["01 DEVICE", "02 CONNECT", "03 PICK", "04 MOVE"];
    let mut tabs = row().w_full().gap(4.0);
    for (i, name) in names.iter().enumerate() {
        let on = s.tab == i as u8 + 1;
        tabs = tabs.child(el().key(format!("tab-{}", i + 1)).grow(1.0).h(28.0).center().radius(5.0)
            .fill(if on { DEEP } else { PANEL }).border(1.0, if on { ACCENT } else { EDGE }).cursor(Cursor::Pointer)
            .child(caps(*name, if on { INK } else { MUTED }))
            .on_press(move |s: &mut S| s.tab = i as u8 + 1));
    }
    let content = match s.tab {
        1 => tab_device(s, k),
        2 => tab_connect(s),
        3 => tab_pick(s),
        _ => tab_move(s),
    };
    let r = layout().elements.left_panel;
    col().absolute(r.x, r.y).w(r.w).gap(8.0).child(tabs).child(card(r.h - 36.0).child(content))
}

fn fmt_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 { format!("{:.1} GB", b / 1e9) } else if b >= 1e6 { format!("{:.0} MB", b / 1e6) } else { format!("{:.0} KB", b / 1e3) }
}

fn right_panel(s: &S) -> El {
    let r = layout().elements.right_panel;
    let mut c = card(r.h);
    match &s.session {
        Some(x) => {
            c = c.child(row().w_full().center_y().child(caps("TRANSFER", ACCENT)).child(spacer()).child(mono(x.state.clone(), 10.5, LIGHT)))
                .child(label(format!("{}  ⟶  {}", x.sender.name, x.receiver.name), 13.0, 600.0, INK))
                .child(mono(format!("started {}", x.started_utc), 10.0, MUTED));
        }
        None => {
            let d = s.peer_device().filter(|_| !s.finding);
            let mut head = row().w_full().center_y().child(caps("OTHER DEVICE", LIGHT)).child(spacer());
            if d.is_some() && s.active_request().is_none() {
                head = head.child(button("Change", false).key("change").on_press(|s: &mut S| s.find()));
            }
            let line = match d {
                Some(d) => format!("{} · {} · {}", d.name, d.user, ago(d.seen)),
                None if s.finding => "Click a device above to work with it.".into(),
                None => "Find the other device above.".into(),
            };
            c = c.child(head).child(label(line, 13.0, 500.0, SOFT).wrap().w_full());
        }
    }
    let status = if !s.busy.is_empty() { (s.busy.clone(), SOFT) } else if !s.err.is_empty() { (s.err.clone(), ERR) } else if s.bus.is_some() || s.demo { (format!("bus {}", s.cfg.bus), MUTED) } else { (String::new(), MUTED) };
    c.child(spacer()).child(label(status.0, 11.0, 600.0, status.1).wrap().w_full())
}

fn sheet(card: El) -> El {
    let dim = Shape::new().fill_path(&body_path(), Color { r: 0.0, g: 0.0, b: 0.002, a: 0.74 });
    let r = layout().elements.dialog;
    el().absolute(0.0, 0.0).size(W, H).child(full(dim)).child(col().absolute(r.x, r.y).w(r.w).child(card))
}

fn incoming(s: &S, r: &Request) -> El {
    let d = s.devices.iter().find(|d| d.id == r.from);
    let who = d.map(|d| format!("{} ({})", d.name, if d.os.is_empty() { look::describe(&d.look) } else { d.os.clone() })).unwrap_or_else(|| format!("#{}", r.from));
    let (title, body) = match r.ask {
        Ask::Send => ("Receive a setup?", format!("{who} wants to send its settings, keys and files to this device. You pick what to take before anything moves.")),
        Ask::Receive => ("Send this setup?", format!("{who} wants to take this device's settings, keys and files. You pick what to offer before anything moves.")),
    };
    let card = col().w_full().fill(FIELD).border(1.0, DEEP).radius(16.0).padding(22.0).gap(12.0)
        // A tight shadow: the window is see-through, and a wide one would show past concave edges.
        .shadow(10.0, 28.0, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.6 })
        .child(caps("REQUEST", ACCENT))
        .child(label(title, 19.0, 700.0, INK))
        .child(label(body, 13.5, 500.0, SOFT).wrap().w_full())
        .child(row().w_full().gap(10.0).child(spacer())
            .child(button("Decline", false).key("decline").on_press(|s: &mut S| s.answer(false)))
            .child(button("Approve", true).key("approve").on_press(|s: &mut S| s.answer(true))));
    sheet(card)
}

fn overlays(s: &S) -> Vec<El> {
    let l = look::fix(&s.cfg.look);
    if !(s.model_open && s.tab == 1) {
        return vec![];
    }
    let items: Vec<El> = look::models(&l).into_iter().map(|(id, shown)| {
        let n = Look { model: id.clone(), ..l.clone() };
        menu_row(shown, id == l.model).key(format!("model-{id}")).on_press(move |s: &mut S| {
            s.model_open = false;
            s.set_look(n.clone());
        })
    }).collect();
    vec![menu("model-dd", 484.0, 190.0, items)]
}

fn view(s: &S, _: &mut Cx<S>) -> El {
    let k = kit();
    let e = &layout().elements;
    let mut root = el().key("root").size(W, H).children(backdrop(s)).children(conduit(s));
    let peer = s.peer_device();
    let (role_me, role_peer) = match s.sending() {
        Some(true) => ("SENDER", "RECEIVER"),
        Some(false) => ("RECEIVER", "SENDER"),
        None => ("READY", if peer.is_some_and(|d| bus::now().saturating_sub(d.seen) < 90) { "ONLINE" } else { "AWAY" }),
    };
    root = root.children(device_on_screen("left", &s.cfg.name, &s.cfg.id, &s.cfg.look, e.laptop_left, role_me.into(), true));
    root = match peer {
        _ if s.finding => root.children(grid(s)),
        Some(d) => root.children(device_on_screen("right", &d.name, &d.id, &d.look, e.laptop_right, role_peer.into(), false)),
        None => root.children(find_prompt(s)),
    };
    root = root
        .child(row().absolute(e.title.x, e.title.y).gap(10.0).center_y().window_drag()
            .child(row().child(label("Windy", 26.0, 800.0, INK)).child(label("~", 26.0, 800.0, ACCENT2)).child(label("Sync", 26.0, 800.0, LIGHT)))
            .child(mono(format!("WYSY · v{}", env!("CARGO_PKG_VERSION")), 10.5, MUTED)))
        .child(row().absolute(e.controls.x, e.controls.y).gap(6.0)
            .child(button("▁", false).key("min").on_press_tx(|_: &mut S, tx: &mut Tx| tx.minimize()))
            .child(button("✕", false).key("quit").on_press_tx(|_: &mut S, tx: &mut Tx| tx.quit())))
        .child(left_panel(s, &k))
        .child(col().absolute(e.right_panel.x, e.right_panel.y).w(e.right_panel.w).child(right_panel(s)));
    if let Some(a) = actions(s) {
        root = root.child(a);
    }
    if let Some((r, _)) = &s.incoming {
        root = root.child(incoming(s, r));
    }
    rune_kit::root(root.children(overlays(s)), Vec::<El>::new())
}

// Scenes for screenshots: made-up devices, nothing from this computer, nothing sent.

fn look_of(os: &str, kind: &str, model: &str, color: &str) -> Look {
    Look { os: os.into(), kind: kind.into(), model: model.into(), color: color.into() }
}

fn demo(s: &mut S) {
    s.demo = true;
    s.busy.clear();
    s.err.clear();
    s.here = Os::Windows;
    s.cfg.id = "7f3a2c91".into();
    s.cfg.name = "Desk PC".into();
    s.name = TextEdit::new("Desk PC");
    s.cfg.registered = true;
    s.cfg.bus = "someone/windysync-bus".into();
    s.bus_edit = TextEdit::new("someone/windysync-bus");
    s.cfg.look = look_of("windows", "desktop", "", "Graphite");
    let dev = |id: &str, name: &str, look: Look, ago: u64| Device { id: id.into(), name: name.into(), os: Os::Windows.name().into(), look, user: "someone".into(), seen: bus::now() - ago, ..Device::default() };
    s.devices = vec![
        dev("c04e19b7", "Studio PC", look_of("windows", "desktop", "", "White"), 20),
        dev("5b21d0aa", "Travel Laptop", look_of("windows", "laptop", "", "Silver"), 40),
        dev("e8d4a130", "Home Server", look_of("windows", "desktop", "", "Black"), 7200),
        dev("91fb07c2", "Old Laptop", look_of("windows", "laptop", "", "Graphite"), 90000),
    ];
    s.polled = true;
    s.peer = "5b21d0aa".into();
    s.tab = 2;
}

fn init_demo() -> S {
    let mut s = init();
    demo(&mut s);
    s
}

/// What a PC might offer: catalogue items, four lists and four of the person's files.
fn demo_items() -> Vec<Item> {
    let ids = ["dev/git", "secrets/ssh", "secrets/gnupg", "shell/powershell", "editors/vscode", "editors/cursor", "editors/zed", "terminals/wave", "notes/obsidian", "browsers/chrome", "browsers/edge", "ai/claude-code", "ai/codex"];
    let mut items: Vec<Item> = ids.iter().filter_map(|id| CATALOG.iter().find(|e| e.id == *id)).map(Item::from).collect();
    items.extend(["tools/winget", "tools/scoop", "tools/vscode-extensions", "tools/npm"].iter().filter_map(|id| LISTS.iter().find(|l| l.id == *id)).map(Item::from));
    items.extend([("files/documents", "Projects"), ("files/documents", "Taxes 2026"), ("files/desktop", "notes.txt"), ("files/pictures", "Holiday")].iter().filter_map(|(f, n)| Item::folder_child(f, n)));
    items
}

fn live(l: Live) -> Option<Arc<Mutex<Live>>> {
    Some(Arc::new(Mutex::new(l)))
}

/// An approved transfer with `peer`, this device sending or receiving.
fn accepted(s: &mut S, this_sends: bool, peer: &str, state: &str) {
    demo(s);
    s.peer = peer.into();
    let (a, b) = if this_sends { ("7f3a2c91", peer) } else { (peer, "7f3a2c91") };
    let r = Request { ask: Ask::Send, from: a.into(), to: b.into(), at: bus::now() - 300, answer: Answer::Accepted, transfer: bus::transfer_dir(bus::now() - 280, a, b) };
    let name = |id: &str| if id == "7f3a2c91" { s.cfg.name.clone() } else { s.devices.iter().find(|d| d.id == id).map(|d| d.name.clone()).unwrap_or_default() };
    let party = |id: &str| bus::Party { id: id.into(), name: name(id) };
    s.session = Some(Session { sender: party(a), receiver: party(b), state: state.into(), started_utc: bus::utc(bus::now() - 280), ..Session::default() });
    s.pick = Pick { transfer: r.transfer.clone(), live: live(Live { state: "waiting".into(), ..Live::default() }), shown: true, ..Pick::default() };
    s.requests = vec![(r, String::new())];
}

/// Taking from Studio PC: shell, editors and files open, the tax folder left out.
fn pick_scene(s: &mut S, chosen: bool) {
    accepted(s, false, "c04e19b7", "picking");
    let os = s.here;
    s.pick.items = demo_items();
    s.pick.ticks = s.pick.items.iter().filter(|i| session::fits(i, os) && i.id != "files/documents/taxes-2026").map(|i| i.id.clone()).collect();
    s.pick.open = ["shell", "editors", "files"].map(String::from).into();
    if chosen {
        s.pick.live = live(Live { state: "waiting".into(), note: "Waiting for Studio PC to start sending…".into(), ..Live::default() });
    } else {
        s.pick.choose = Some(channel().0);
    }
    s.tab = 3;
}

fn moving_scene(s: &mut S, this_sends: bool) {
    let peer = if this_sends { "5b21d0aa" } else { "c04e19b7" };
    accepted(s, this_sends, peer, "sending");
    s.pick.items = demo_items();
    s.pick.live = live(Live { state: if this_sends { "sending" } else { "receiving" }.into(), bytes: 2_340_000_000, items_done: 11, items: 19, current: "Projects".into(), ..Live::default() });
    s.flow.dir = if this_sends { 1.0 } else { -1.0 };
    s.flow.v = s.flow.dir * FLOW_V;
    s.flow.pos = 0.37 * s.flow.dir;
    s.tab = 4;
}

fn done_scene(s: &mut S) {
    accepted(s, false, "c04e19b7", "done");
    let dir = std::path::PathBuf::from(r"C:\Users\someone\WindySync\20261126-101240_c04e19b7_7f3a2c91");
    let note = "placed 31 paths, 2 problems, 0 items missing".into();
    s.pick.live = live(Live { state: "done".into(), note, bytes: 6_120_000_000, items_done: 19, items: 19, dir: Some(dir), placed: 31, problems: 2, missing: 0, ..Live::default() });
    s.tab = 4;
}

pub fn run(tooling: bool) {
    if tooling {
        // Headless renders never touch this device's own config, keys or GitHub.
        // SAFETY: still single threaded; nothing has read the environment yet.
        unsafe {
            std::env::set_var("WINDYSYNC_HOME", std::env::temp_dir().join("windysync-demo"));
            std::env::set_var("WINDYSYNC_TOKEN", "");
        }
    }
    let spec = AppSpec {
        setup: Some(setup),
        poll: Some(poll),
        families: ("Inter", "ui-monospace"),
        scenes: &[
            ("connect", |s| demo(s)),
            ("start", |s| {
                demo(s);
                s.peer.clear();
            }),
            ("find", |s| {
                demo(s);
                s.peer.clear();
                s.finding = true;
                s.found_at = -5.0;
                s.hovered = "5b21d0aa".into();
            }),
            ("hover-send", |s| {
                demo(s);
                s.hover_dir = 1.0;
                s.flow.dir = 1.0;
                s.flow.v = FLOW_V;
                s.flow.pos = 0.3;
            }),
            ("device", |s| {
                demo(s);
                s.tab = 1;
            }),
            ("device-laptop", |s| {
                demo(s);
                s.cfg.look = look_of("windows", "laptop", "", "Silver");
                s.tab = 1;
            }),
            ("request", |s| {
                demo(s);
                s.incoming = Some((Request { ask: Ask::Send, from: "c04e19b7".into(), to: "7f3a2c91".into(), at: bus::now(), answer: Answer::Pending, transfer: String::new() }, String::new()));
            }),
            ("pick", |s| pick_scene(s, false)),
            ("picked", |s| pick_scene(s, true)),
            ("sending", |s| moving_scene(s, true)),
            ("receiving", |s| moving_scene(s, false)),
            ("done", done_scene),
        ],
        ..AppSpec::new("WindySync", (W, H), if tooling { init_demo } else { init }, view)
    };
    let options = WindowOptions { transparent: true, borderless: true, resizable: false, ..Default::default() };
    App::new(spec).window_options(options).run();
}

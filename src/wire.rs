//! Files go straight from device to device over TCP, inside a `seal` stream under the transfer's key. One connection
//! carries every item still missing:
//!
//!   receiver -> sender   sealed stream: {"need": [item ids]}
//!   sender -> receiver   sealed stream: per item, [u32 len][header json] then [u32 len][tar bytes]… [u32 0]
//!
//! An item's tar holds each slot's file or folder as `<slot>/<its name>`, never the sender's own paths: the receiver
//! places slot i at its own path for slot i, on whichever system it runs. A cut connection loses only the item in
//! flight; the receiver asks again for what it hasn't finished.

use std::io::{self, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::catalog::Item;
use crate::os::Env;
use crate::seal;

/// Folders rebuilt from source: skipped inside the person's own folders (`files/…`) only, never in an app's.
pub const REBUILDABLE: [&str; 8] = ["node_modules", "target", ".venv", "venv", "__pycache__", ".gradle", "DerivedData", ".next"];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Endpoint {
    pub key: Vec<u8>,
    pub port: u16,
    pub addrs: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Hello {
    need: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Header {
    id: String,
}

/// This device's address on its main network (the one a route to the internet takes); no packet is sent.
pub fn addresses() -> Vec<String> {
    let probe = UdpSocket::bind("0.0.0.0:0").and_then(|s| s.connect("192.0.2.1:9").map(|_| s)).and_then(|s| s.local_addr());
    probe.map(|a| vec![a.ip().to_string()]).unwrap_or_default()
}

pub fn listen() -> io::Result<(TcpListener, u16)> {
    let l = TcpListener::bind("0.0.0.0:0")?;
    let port = l.local_addr()?.port();
    Ok((l, port))
}

fn put_u32(w: &mut impl Write, n: u32) -> io::Result<()> {
    w.write_all(&n.to_be_bytes())
}

fn get_u32(r: &mut impl Read) -> io::Result<Option<u32>> {
    let mut b = [0u8; 4];
    match r.read_exact(&mut b) {
        Ok(()) => Ok(Some(u32::from_be_bytes(b))),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e),
    }
}

/// An item's bytes so far, told to `on` at most five times a second, so a big folder shows progress before it ends.
struct Tick<'a> {
    on: &'a dyn Fn(u64),
    bytes: u64,
    told: Instant,
}

impl<'a> Tick<'a> {
    fn new(on: &'a dyn Fn(u64)) -> Self {
        Tick { on, bytes: 0, told: Instant::now() }
    }

    fn add(&mut self, n: usize) {
        self.bytes += n as u64;
        if self.told.elapsed() >= Duration::from_millis(200) {
            self.told = Instant::now();
            (self.on)(self.bytes);
        }
    }
}

/// Writes everything as `[u32 len][bytes]` frames; the caller ends an item with a 0 frame.
struct Frames<'a, W: Write> {
    out: &'a mut W,
    tick: Tick<'a>,
}

impl<W: Write> Write for Frames<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        put_u32(self.out, buf.len() as u32)?;
        self.out.write_all(buf)?;
        self.tick.add(buf.len());
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Reads one item's frames back as a plain byte stream, up to its 0 frame.
struct Unframe<'a, R: Read> {
    inp: &'a mut R,
    left: usize,
    ended: bool,
    tick: Tick<'a>,
}

impl<R: Read> Read for Unframe<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.left == 0 {
            if self.ended {
                return Ok(0);
            }
            match get_u32(self.inp)? {
                Some(0) => self.ended = true,
                Some(n) => self.left = n as usize,
                None => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the transfer was cut")),
            }
        }
        let max = self.left.min(buf.len());
        let n = self.inp.read(&mut buf[..max])?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the transfer was cut"));
        }
        self.left -= n;
        self.tick.add(n);
        Ok(n)
    }
}

/// Adds `path` to the tar as `name`, folders walked by hand so links stay links and, in the person's own folders,
/// rebuildable ones stay behind.
fn add(tar: &mut tar::Builder<impl Write>, path: &Path, name: &Path, skip_rebuildable: bool) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        tar.append_dir(name, path)?;
        let mut kids: Vec<_> = std::fs::read_dir(path)?.filter_map(Result::ok).collect();
        kids.sort_by_key(|k| k.file_name());
        for k in kids {
            let n = k.file_name();
            if skip_rebuildable && REBUILDABLE.iter().any(|r| n == *r) {
                continue;
            }
            add(tar, &k.path(), &name.join(&n), skip_rebuildable)?;
        }
        Ok(())
    } else {
        // A file, or a link kept as a link (follow_symlinks is off).
        tar.append_path_with_name(path, name)
    }
}

/// An item's tar, written as frames: slot i's path as `i/<its name>`. Returns the bytes written; `tick` hears
/// them as they go.
fn pack(item: &Item, here: &Env, out: &mut impl Write, tick: &dyn Fn(u64)) -> io::Result<u64> {
    let mut frames = Frames { out, tick: Tick::new(tick) };
    {
        let mut tar = tar::Builder::new(&mut frames);
        tar.follow_symlinks(false);
        if !item.run.is_empty() {
            let text = crate::catalog::run_list(&item.run);
            let mut h = tar::Header::new_gnu();
            h.set_size(text.len() as u64);
            h.set_mode(0o644);
            tar.append_data(&mut h, list_name(item), text.as_slice())?;
        }
        for (i, t) in item.templates(here.os).iter().enumerate() {
            let Some(p) = here.resolve(t) else { continue };
            let p = PathBuf::from(p);
            if std::fs::symlink_metadata(&p).is_err() {
                continue;
            }
            let name = Path::new(&i.to_string()).join(p.file_name().unwrap_or_default());
            add(&mut tar, &p, &name, item.id.starts_with("files/"))?;
        }
        tar.finish()?;
    }
    let n = frames.tick.bytes;
    put_u32(frames.out, 0)?;
    Ok(n)
}

/// What one end has done so far on this connection (the receiver) or in all (the sender).
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub bytes: u64,
    pub items_done: usize,
    pub current: String,
    pub finished: bool,
}

/// Sends `items` to whoever connects with the key, until the receiver needs nothing; returns what was sent.
pub fn serve(listener: TcpListener, key: [u8; 32], items: Vec<Item>, here: &Env, on: impl Fn(&Progress)) -> Result<Progress, String> {
    let mut p = Progress::default();
    let mut sent: std::collections::HashSet<String> = Default::default();
    loop {
        let (stream, _) = listener.accept().map_err(|e| e.to_string())?;
        stream.set_nodelay(true).ok();
        let Ok(mut hello_in) = seal::Reader::new(&key, stream.try_clone().map_err(|e| e.to_string())?) else { continue };
        let mut raw = Vec::new();
        // A connection that can't prove it holds the key is dropped, and the sender waits for the next.
        if hello_in.read_to_end(&mut raw).is_err() {
            continue;
        }
        let Ok(hello) = serde_json::from_slice::<Hello>(&raw) else { continue };
        if hello.need.is_empty() {
            p.finished = true;
            on(&p);
            return Ok(p);
        }
        let mut out = seal::Writer::new(&key, io::BufWriter::with_capacity(1 << 20, stream)).map_err(|e| e.to_string())?;
        let result: io::Result<()> = (|| {
            for item in items.iter().filter(|i| hello.need.contains(&i.id)) {
                p.current = item.name.clone();
                on(&p);
                let head = serde_json::to_vec(&Header { id: item.id.clone() }).unwrap();
                put_u32(&mut out, head.len() as u32)?;
                out.write_all(&head)?;
                let before = p.clone();
                p.bytes += pack(item, here, &mut out, &|n| on(&Progress { bytes: before.bytes + n, ..before.clone() }))?;
                if sent.insert(item.id.clone()) {
                    p.items_done += 1;
                }
                on(&p);
            }
            out.finish()?;
            Ok(())
        })();
        // A cut connection: wait for the receiver to come back and ask for what is left.
        if let Err(e) = result {
            p.current = format!("connection lost ({e}); waiting for the other device");
            on(&p);
        }
    }
}

/// An item id as one folder name.
pub fn safe(id: &str) -> String {
    id.replace('/', "__")
}

/// Connects to the sender and unpacks each needed item into `staging/<item>`, marking it done when whole. `on`
/// hears this connection's progress, `current` being the item's id.
pub fn fetch(ep: &Endpoint, need: Vec<String>, staging: &Path, on: impl Fn(&Progress)) -> Result<Vec<String>, String> {
    let key: [u8; 32] = ep.key.clone().try_into().map_err(|_| "bad transfer key")?;
    let mut last_err = String::from("no address to try");
    let stream = ep.addrs.iter().find_map(|a| {
        let addr = (a.as_str(), ep.port).to_socket_addrs().ok()?.next()?;
        TcpStream::connect_timeout(&addr, Duration::from_secs(4)).map_err(|e| last_err = format!("{a}: {e}")).ok()
    });
    let stream = stream.ok_or_else(|| format!("can't reach the other device ({last_err})"))?;
    stream.set_nodelay(true).ok();
    // A relayed connection can die without a reset; a minute without a byte counts as cut, and the caller asks again.
    stream.set_read_timeout(Some(Duration::from_secs(60))).ok();
    let mut hello = seal::Writer::new(&key, stream.try_clone().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    hello.write_all(&serde_json::to_vec(&Hello { need: need.clone() }).unwrap()).map_err(|e| e.to_string())?;
    hello.finish().map_err(|e| e.to_string())?;
    if need.is_empty() {
        return Ok(vec![]);
    }
    let mut inp = seal::Reader::new(&key, BufReader::with_capacity(1 << 20, stream)).map_err(|e| e.to_string())?;
    let mut done = Vec::new();
    let mut p = Progress::default();
    while let Some(len) = get_u32(&mut inp).map_err(|e| e.to_string())? {
        let mut head = vec![0u8; len as usize];
        inp.read_exact(&mut head).map_err(|e| e.to_string())?;
        let h: Header = serde_json::from_slice(&head).map_err(|e| e.to_string())?;
        let dest = staging.join(safe(&h.id));
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        p.current = h.id.clone();
        on(&p);
        let before = p.clone();
        let tick = |n| on(&Progress { bytes: before.bytes + n, ..before.clone() });
        let mut body = Unframe { inp: &mut inp, left: 0, ended: false, tick: Tick::new(&tick) };
        let mut ar = tar::Archive::new(&mut body);
        ar.set_preserve_permissions(true);
        ar.unpack(&dest).map_err(|e| format!("unpacking {}: {e}", h.id))?;
        // tar stops at its end marker; the frames after it up to the 0 frame are padding.
        io::copy(&mut body, &mut io::sink()).map_err(|e| e.to_string())?;
        p.bytes += body.tick.bytes;
        std::fs::write(dest.with_extension("done"), b"").map_err(|e| e.to_string())?;
        p.items_done += 1;
        on(&p);
        done.push(h.id);
    }
    Ok(done)
}

/// A list's file: `0/<last part of its id>.txt`, which lands in `lists/` of the transfer folder.
fn list_name(item: &Item) -> String {
    format!("0/{}.txt", item.id.rsplit('/').next().unwrap_or("list"))
}

/// Moves an unpacked item from staging to this device's own place for each slot; what was there is renamed to
/// `<name>.before-windysync`. Returns (placed paths, problems).
pub fn place(item: &Item, staging: &Path, here: &Env) -> (Vec<String>, Vec<String>) {
    let (mut placed, mut problems) = (vec![], vec![]);
    if !item.run.is_empty() {
        let lists = staging.parent().unwrap_or(staging).join("lists");
        let name = list_name(item);
        let dst = lists.join(&name[2..]);
        let _ = std::fs::create_dir_all(&lists);
        match std::fs::rename(staging.join(safe(&item.id)).join(&name), &dst) {
            Ok(()) => placed.push(dst.display().to_string()),
            Err(e) => problems.push(format!("{}: {e}", dst.display())),
        }
        return (placed, problems);
    }
    for (i, t) in item.templates(here.os).iter().enumerate() {
        let Some(dst) = here.resolve(t) else { continue };
        let slot = staging.join(safe(&item.id)).join(i.to_string());
        // The slot holds the sender's file or folder under the sender's own name.
        let Some(src) = std::fs::read_dir(&slot).ok().and_then(|mut d| d.next()).and_then(Result::ok).map(|e| e.path()) else { continue };
        let dst = PathBuf::from(dst);
        if std::fs::symlink_metadata(&dst).is_ok() {
            let aside = PathBuf::from(format!("{}.before-windysync", dst.display()));
            let _ = std::fs::remove_dir_all(&aside);
            let _ = std::fs::remove_file(&aside);
            if let Err(e) = std::fs::rename(&dst, &aside) {
                problems.push(format!("{}: couldn't move the existing one aside: {e}", dst.display()));
                continue;
            }
        }
        let _ = std::fs::create_dir_all(dst.parent().unwrap_or(Path::new(".")));
        match std::fs::rename(&src, &dst) {
            Ok(()) => placed.push(dst.display().to_string()),
            Err(e) => problems.push(format!("{}: {e}", dst.display())),
        }
    }
    (placed, problems)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::CATALOG;
    use crate::os::{Env, Os};

    /// A Mac-shaped home sends VS Code settings and a projects folder to a Linux-shaped one over localhost: each
    /// lands at Linux's own path, node_modules stays behind in the person's folder, and a wrong key gets nothing.
    #[test]
    fn a_mac_item_crosses_to_linux_paths() {
        let base = std::env::temp_dir().join(format!("windysync-wire-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (a, b) = (base.join("mac"), base.join("linux"));
        let user = a.join("Library/Application Support/Code/User");
        std::fs::create_dir_all(user.join("snippets")).unwrap();
        std::fs::write(user.join("settings.json"), b"{\"editor.fontSize\": 14}").unwrap();
        std::fs::write(user.join("snippets/rust.json"), b"{}").unwrap();
        std::fs::create_dir_all(a.join("Documents/proj/node_modules")).unwrap();
        std::fs::write(a.join("Documents/proj/main.rs"), b"fn main() {}").unwrap();
        std::fs::write(a.join("Documents/proj/node_modules/x.js"), b"junk").unwrap();
        std::fs::create_dir_all(b.join(".config/Code/User")).unwrap();
        std::fs::write(b.join(".config/Code/User/settings.json"), b"{}").unwrap();

        let mac = Env { os: Os::Mac, home: a.display().to_string(), vars: Default::default() };
        let linux = Env { os: Os::Linux, home: b.display().to_string(), vars: Default::default() };
        let vscode = Item::from(CATALOG.iter().find(|e| e.id == "editors/vscode").unwrap());
        let proj = Item::folder_child("files/documents", "proj").unwrap();
        let list = Item { id: "tools/greeting".into(), name: "A list".into(), mac: vec![], win: vec![], linux: vec![], secret: false, note: String::new(), run: vec!["echo".into(), "hello".into()] };
        let items = vec![vscode.clone(), proj.clone(), list.clone()];

        let key = seal::random::<32>();
        let (l, port) = listen().unwrap();
        let m2 = mac.clone();
        let server = std::thread::spawn(move || serve(l, key, items, &m2, |_| {}));
        let ep = Endpoint { key: key.to_vec(), port, addrs: vec!["127.0.0.1".into()] };
        let staging = base.join("staging");
        let wrong = Endpoint { key: seal::random::<32>().to_vec(), ..ep.clone() };
        assert!(fetch(&wrong, vec![vscode.id.clone()], &staging, |_| {}).is_err());

        let heard = std::sync::Mutex::new(Progress::default());
        let done = fetch(&ep, vec![vscode.id.clone(), proj.id.clone(), list.id.clone()], &staging, |p| *heard.lock().unwrap() = p.clone()).unwrap();
        assert_eq!(done.len(), 3);
        fetch(&ep, vec![], &staging, |_| {}).unwrap();
        let sent = server.join().unwrap().unwrap();
        let got = heard.into_inner().unwrap();
        assert_eq!((got.items_done, sent.items_done), (3, 3));
        assert!(got.bytes > 0 && got.bytes == sent.bytes, "both ends count the same bytes: {} and {}", got.bytes, sent.bytes);

        let (placed, problems) = place(&vscode, &staging, &linux);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(placed.len(), 2, "settings and snippets; the sender had no keybindings");
        let lu = b.join(".config/Code/User");
        assert_eq!(std::fs::read(lu.join("settings.json")).unwrap(), b"{\"editor.fontSize\": 14}");
        assert!(lu.join("snippets/rust.json").exists());
        assert_eq!(std::fs::read(lu.join("settings.json.before-windysync")).unwrap(), b"{}", "what was there is kept aside");
        place(&proj, &staging, &linux);
        assert!(b.join("Documents/proj/main.rs").exists());
        assert!(!b.join("Documents/proj/node_modules").exists(), "rebuildable folders stay behind");
        assert_eq!(place(&list, &staging, &linux).1, Vec::<String>::new());
        assert_eq!(std::fs::read_to_string(base.join("lists/greeting.txt")).unwrap().trim(), "hello", "a list made on the sender lands in lists/");
        let _ = std::fs::remove_dir_all(&base);
    }
}

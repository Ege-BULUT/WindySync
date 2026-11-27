//! One transfer between two devices, on the bus under `transfers/<time>_<sender>_<receiver>/`: the offer (sealed to
//! the receiver), the picks (sealed to the sender), the stream key (sealed to the receiver), and each side's state.
//!
//! `send` and `receive` are the two sides, the same for the command line and the window: each takes the person's
//! choice through a closure (`--only`, or the window's ticks) and tells its progress to another.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::bus::{self, Answer, Bus, Device, Party, Request, Session};
use crate::catalog::{CATALOG, FOLDERS, Item};
use crate::os::{Env, Os};
use crate::wire::{self, Endpoint};
use crate::seal;

/// What the sender offers: its system (`macOS`, `Windows`, `Linux`) and home folder (for the brief's path rewrites) and the items it has.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Offer {
    pub os: String,
    pub home: String,
    pub items: Vec<Item>,
}

/// The item ids the receiver takes.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Picks {
    pub ids: Vec<String>,
}

/// Everything this device could send: the catalogue's items found here, and each top-level file or folder of the
/// person's own folders (hidden ones left out).
pub fn offer(env: &Env) -> Offer {
    let exists = |t: &String| env.resolve(t).is_some_and(|p| std::fs::symlink_metadata(p).is_ok());
    let mut items: Vec<Item> = CATALOG.iter().map(Item::from).filter(|i| i.templates(env.os).iter().any(exists)).collect();
    for (id, ..) in FOLDERS {
        let Some(root) = Item::folder_child(id, "x").and_then(|i| env.resolve(&i.templates(env.os)[0])).map(|p| PathBuf::from(p).parent().unwrap().to_path_buf()) else { continue };
        let mut kids: Vec<String> = std::fs::read_dir(&root).map(|d| d.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| !n.starts_with('.')).collect()).unwrap_or_default();
        kids.sort();
        items.extend(kids.iter().filter_map(|k| Item::folder_child(id, k)));
    }
    items.extend(crate::catalog::LISTS.iter().filter(|l| l.on.contains(&env.os) && crate::catalog::on_path(l.run[0])).map(Item::from));
    Offer { os: env.os.name().into(), home: env.home.clone(), items }
}

/// Whether `item` has a place on `os`; a list goes to any system.
pub fn fits(item: &Item, os: Os) -> bool {
    !item.run.is_empty() || !item.templates(os).is_empty()
}

/// Whether `id` is under any of `only` (an id or its start, "editors"); everything is when `only` is empty.
pub fn wanted(id: &str, only: &[String]) -> bool {
    only.is_empty() || only.iter().any(|p| id == p || id.starts_with(&format!("{p}/")))
}

/// The ids a receiver on `os` takes: those under any of `only`, and never one without a place on its system.
pub fn picks(offer: &Offer, os: Os, only: &[String]) -> Picks {
    Picks { ids: offer.items.iter().filter(|i| fits(i, os) && wanted(&i.id, only)).map(|i| i.id.clone()).collect() }
}

/// The order the window lists items in, by the first part of their id.
pub const GROUPS: [&str; 10] = ["dev", "secrets", "shell", "editors", "terminals", "notes", "browsers", "ai", "tools", "files"];

/// Items by the first part of their id, in GROUPS order; a part GROUPS doesn't name comes after them, in the
/// order first seen. Empty groups are left out.
pub fn groups(items: &[Item]) -> Vec<(String, Vec<&Item>)> {
    let mut out: Vec<(String, Vec<&Item>)> = GROUPS.iter().map(|g| (g.to_string(), vec![])).collect();
    for i in items {
        let g = i.id.split('/').next().unwrap_or_default();
        match out.iter_mut().find(|(n, _)| n == g) {
            Some((_, v)) => v.push(i),
            None => out.push((g.to_string(), vec![i])),
        }
    }
    out.retain(|(_, v)| !v.is_empty());
    out
}

/// Sealed files are stored as base64 text: GitHub's contents API reads some binary files as text in a guessed
/// charset and hands back other bytes.
pub fn put_sealed<T: Serialize>(b: &Bus, path: &str, to: &[u8; 32], t: &T, msg: &str) -> Result<(), String> {
    b.put(path, bus::b64(&seal::seal(to, &serde_json::to_vec(t).unwrap())).as_bytes(), None, msg).map(|_| ())
}

pub fn get_sealed<T: DeserializeOwned>(b: &Bus, path: &str, secret: &[u8; 32]) -> Result<Option<T>, String> {
    let Some((text, _)) = b.get(path)? else { return Ok(None) };
    let bytes = bus::unb64(&String::from_utf8_lossy(&text)).ok_or(format!("{path} is not base64"))?;
    let plain = seal::open(secret, &bytes)?;
    serde_json::from_slice(&plain).map(Some).map_err(|e| format!("{path}: {e}"))
}

/// Answers a request. A yes makes the transfer folder with its session.json first.
pub fn approve(b: &Bus, mut r: Request, sha: &str, names: &std::collections::HashMap<String, String>, yes: bool) -> Result<Request, String> {
    if yes {
        let (sender, receiver) = r.sides();
        let at = bus::now();
        let dir = bus::transfer_dir(at, sender, receiver);
        let party = |id: &str| Party { id: id.into(), name: names.get(id).cloned().unwrap_or_default() };
        let s = Session { sender: party(sender), receiver: party(receiver), state: "offering".into(), started: at, started_utc: bus::utc(at), updated: at, ..Session::default() };
        b.put_json(&format!("{dir}/session.json"), &s, None, &format!("transfer {sender} to {receiver}"))?;
        r.transfer = dir;
    }
    r.answer = if yes { Answer::Accepted } else { Answer::Declined };
    b.put_json(&r.path(), &r, Some(sha), &format!("{:?} {}", r.answer, r.path()))?;
    Ok(r)
}

/// Changes session.json (the sender's document).
pub fn update_session(b: &Bus, transfer: &str, f: impl FnOnce(&mut Session)) -> Result<(), String> {
    let path = format!("{transfer}/session.json");
    let (mut s, sha) = b.get_json::<Session>(&path)?.ok_or("the transfer has no session.json")?;
    f(&mut s);
    s.updated = bus::now();
    b.put_json(&path, &s, Some(&sha), &format!("session {}", s.state)).map(|_| ())
}

/// The receiver's folder for one transfer: `~/WindySync/<transfer>`.
pub fn local_dir(env: &Env, transfer: &str) -> PathBuf {
    PathBuf::from(env.resolve("~/WindySync").unwrap_or_default()).join(transfer.trim_start_matches("transfers/"))
}

/// GOAL.md for the agent finishing the setup: what landed where, what didn't, and what to rewrite.
pub fn brief(offer: &Offer, here: &Env, from: &str, placed: &[(Item, Vec<String>, Vec<String>)]) -> String {
    let mut g = format!("# GOAL: make this {} device work like {from} ({})\n\n", here.os.name(), offer.os);
    if offer.home != here.home || offer.os != here.os.name() {
        g += &format!("The old home folder was `{}` on {}, this one is `{}` on {}. Configs that name the old home or its paths need this device's \
            (separators too when the systems differ); keep a copy of each file before changing it.\n\n", offer.home, offer.os, here.home, here.os.name());
    }
    g += "## Items\n\n";
    for (item, to, problems) in placed {
        g += &format!("### {}{}\n\n- id: `{}`\n", item.name, if item.secret { " (secret)" } else { "" }, item.id);
        for t in to {
            g += &format!("- placed: `{t}`\n");
        }
        for p in problems {
            g += &format!("- PROBLEM: {p}\n");
        }
        if !item.note.is_empty() {
            g += &format!("- to finish: {}\n", item.note);
        }
        g += "\n";
    }
    g += "## Done means\n\n- Every item above is checked and works, or is in TODO-FOR-YOU.md with steps.\n- REPORT.md says what was done per item.\n";
    g
}

/// On a Mac receiver: installs what the picked items need before the agent starts: Apple's command line tools,
/// Homebrew (for the Brewfile; the person runs it in Terminal, so the installer can ask for the password) and Node
/// (for npm globals and Claude Code plugin hooks, which run `node`): with brew when it is here, else nodejs.org's
/// LTS into ~/.local/node, no sudo. It only installs what is missing, and exits 1 when something is still missing.
const PREFLIGHT: &str = r#"#!/bin/sh
# Written by WindySync. Installs what the moved items need before the agent starts; safe to run again.
NEED_BREW=__BREW__
NEED_NODE=__NODE__
BREWS=${PREFLIGHT_BREWS-"/opt/homebrew/bin/brew /usr/local/bin/brew"}
ok=1
say() { printf '\033[1m[WindySync]\033[0m %s\n' "$1"; }
find_brew() { for b in $BREWS; do [ -x "$b" ] && { echo "$b"; return; }; done; }
add_line() { grep -qsF "$1" "$HOME/.zprofile" || { printf '\n%s\n' "$1" >> "$HOME/.zprofile"; say "Added to ~/.zprofile: $1"; }; }
# Node's LTS from nodejs.org into ~/.local/node, checked against its SHASUMS256, linked into ~/.local/bin. No sudo.
node_tarball() {
  arch=$(uname -m); [ "$arch" = x86_64 ] && arch=x64
  idx=$(curl -fsSL https://nodejs.org/dist/index.json) || return 1
  v=$(printf '%s' "$idx" | tr '{' '\n' | grep -v '"lts":false' | grep -m1 '"version"' | sed 's/.*"version":"\([^"]*\)".*/\1/')
  [ -n "$v" ] || return 1
  tar="node-$v-darwin-$arch.tar.gz"
  tmp=$(mktemp -d) || return 1
  curl -fsSL -o "$tmp/$tar" "https://nodejs.org/dist/$v/$tar" || return 1
  want=$(curl -fsSL "https://nodejs.org/dist/$v/SHASUMS256.txt" | awk -v f="$tar" '$2 == f { print $1 }')
  got=$(shasum -a 256 "$tmp/$tar" | awk '{ print $1 }')
  if [ -z "$want" ] || [ "$want" != "$got" ]; then say "Node's download did not match its checksum."; return 1; fi
  tar -xzf "$tmp/$tar" -C "$tmp" || return 1
  rm -rf "${HOME:?}/.local/node" && mkdir -p "$HOME/.local/bin" && mv "$tmp/node-$v-darwin-$arch" "$HOME/.local/node" || return 1
  for b in node npm npx; do ln -sf "$HOME/.local/node/bin/$b" "$HOME/.local/bin/$b"; done
  rm -rf "$tmp"
  add_line 'export PATH="$HOME/.local/bin:$PATH"'
  export PATH="$HOME/.local/bin:$PATH"
  say "Node $v is in ~/.local/node."
}
if ! xcode-select -p >/dev/null 2>&1; then
  say "Apple's command line tools are missing. A window opens to install them; run this again when it finishes:"
  say "  sh '$0'"
  xcode-select --install
  exit 1
fi
brew_bin=$(find_brew)
# Homebrew only for a Brewfile: Node alone comes from nodejs.org without a password.
if [ -z "$brew_bin" ] && [ "$NEED_BREW" = 1 ]; then
  say "Installing Homebrew (it asks for this Mac's password)."
  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)" || say "Homebrew did not install."
  brew_bin=$(find_brew)
fi
if [ -n "$brew_bin" ]; then
  eval "$("$brew_bin" shellenv)"
  # Terminal shells read ~/.zprofile; without this line brew and what it installs are not on PATH.
  add_line "eval \"\$($brew_bin shellenv)\""
elif [ "$NEED_BREW" = 1 ]; then
  ok=0
fi
[ -x "$HOME/.local/node/bin/node" ] && export PATH="$HOME/.local/bin:$PATH"
if [ "$NEED_NODE" = 1 ] && ! command -v node >/dev/null 2>&1; then
  say "Installing Node."
  { [ -n "$brew_bin" ] && "$brew_bin" install node; } || node_tarball || { say "Node did not install; get it from nodejs.org."; ok=0; }
fi
if [ "$ok" = 1 ]; then say "Tools ready."; else say "Some tools are missing; the agent puts them in TODO-FOR-YOU.md."; exit 1; fi
"#;

/// preflight.sh for these picked ids on a Mac, or None when they need no tool. `dir` holds `lists/brew.txt`.
fn preflight(ids: &[&str], dir: &Path) -> Option<String> {
    let brew = ids.contains(&"tools/brew");
    let brew_node = std::fs::read_to_string(dir.join("lists/brew.txt")).is_ok_and(|b| b.lines().any(|l| l.trim_start().starts_with("brew \"node")));
    let node = brew_node || ids.iter().any(|i| *i == "tools/npm" || i.starts_with("ai/claude"));
    (brew || node).then(|| PREFLIGHT.replace("__BREW__", if brew { "1" } else { "0" }).replace("__NODE__", if node { "1" } else { "0" }))
}

/// Places every unpacked item and writes the brief in `dir`; returns (placed paths, problems).
pub fn finish(offer: &Offer, picks: &Picks, here: &Env, from: &str, dir: &Path) -> (usize, usize) {
    let staging = dir.join("staging");
    let placed: Vec<(Item, Vec<String>, Vec<String>)> = offer.items.iter().filter(|i| picks.ids.contains(&i.id)).map(|i| {
        let (to, problems) = wire::place(i, &staging, here);
        (i.clone(), to, problems)
    }).collect();
    let mut goal = brief(offer, here, from, &placed);
    let ids: Vec<&str> = placed.iter().map(|p| p.0.id.as_str()).collect();
    if let Some(s) = preflight(&ids, dir).filter(|_| here.os == Os::Mac) {
        let _ = std::fs::write(dir.join("preflight.sh"), s);
        goal = goal.replacen("\n\n", "\n\nFirst run `sh preflight.sh` in this folder (in Terminal): it installs Apple's command line tools, Homebrew and Node where these items need them. What it can't do goes into TODO-FOR-YOU.md.\n\n", 1);
    }
    let _ = std::fs::write(dir.join("GOAL.md"), goal);
    (placed.iter().map(|p| p.1.len()).sum(), placed.iter().map(|p| p.2.len()).sum())
}

// The two sides ------------------------------------------------------------------------------------------

/// What one side of a transfer is doing, told to the caller each time it changes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Live {
    /// waiting, sending, receiving, placing, done
    pub state: String,
    /// The step in words: what it waits for, or how it went.
    pub note: String,
    pub bytes: u64,
    pub items_done: usize,
    pub items: usize,
    /// The item's name moving now.
    pub current: String,
    /// Receiver: the transfer folder (GOAL.md is in it) once things are in place, and how that went.
    pub dir: Option<PathBuf>,
    pub placed: usize,
    pub problems: usize,
    pub missing: usize,
}

const POLL: Duration = if cfg!(test) { Duration::from_millis(40) } else { Duration::from_secs(5) };

fn step(state: &str, note: String) -> Live {
    Live { state: state.into(), note, ..Live::default() }
}

/// Polls `f` every 5 s, for up to an hour, until it gives something; GitHub doesn't count the unchanged answers.
pub fn wait<T>(what: String, on: &impl Fn(&Live), mut f: impl FnMut() -> Result<Option<T>, String>) -> Result<T, String> {
    on(&step("waiting", format!("{what}…")));
    let start = Instant::now();
    loop {
        if let Some(t) = f()? {
            return Ok(t);
        }
        if start.elapsed() > Duration::from_secs(3600) {
            return Err(format!("gave up: {what}"));
        }
        std::thread::sleep(POLL);
    }
}

/// The sender's side of the approved transfer `t`: offers what `choose` keeps of this device's items (None stops
/// here), waits for the picks and sends them straight to the receiver.
pub fn send(b: &Bus, secret: &[u8; 32], t: &str, to: &Device, here: &Env, choose: impl FnOnce(Offer) -> Option<Offer>, on: impl Fn(&Live)) -> Result<(), String> {
    let key = to.key_bytes().ok_or("the other device has no key on the bus")?;
    let offer = choose(offer(here)).ok_or("cancelled")?;
    on(&step("waiting", format!("offering {} items", offer.items.len())));
    put_sealed(b, &format!("{t}/offer.sealed"), &key, &offer, "offer")?;
    update_session(b, t, |s| s.state = "picking".into())?;
    let picks: Picks = wait(format!("waiting for {} to pick", to.name), &on, || get_sealed(b, &format!("{t}/picks.sealed"), secret))?;
    let items: Vec<Item> = offer.items.into_iter().filter(|i| picks.ids.contains(&i.id)).collect();
    let n = items.len();
    on(&step("waiting", format!("{} picked {n} items", to.name)));
    let (listener, port) = wire::listen().map_err(|e| e.to_string())?;
    let stream_key = seal::random::<32>();
    let mut addrs = wire::addresses();
    if cfg!(test) {
        addrs.push("127.0.0.1".into());
    }
    put_sealed(b, &format!("{t}/key.sealed"), &key, &Endpoint { key: stream_key.to_vec(), port, addrs }, "stream key")?;
    update_session(b, t, |s| s.state = "sending".into())?;
    let p = wire::serve(listener, stream_key, items, here, |p| on(&Live { state: "sending".into(), bytes: p.bytes, items_done: p.items_done, items: n, current: p.current.clone(), ..Live::default() }))?;
    update_session(b, t, |s| s.state = "done".into())?;
    on(&Live { state: "done".into(), note: "done".into(), bytes: p.bytes, items_done: p.items_done, items: n, ..Live::default() });
    Ok(())
}

/// The receiver's side of the approved transfer `t`: takes the offer, sends back what `choose` picks of it (None
/// stops here), fetches those from the sender, asking again after a cut, and puts them in place with the brief.
/// Returns the transfer folder, where GOAL.md is.
pub fn receive(b: &Bus, secret: &[u8; 32], t: &str, from: &Device, here: &Env, choose: impl FnOnce(&Offer) -> Option<Picks>, on: impl Fn(&Live)) -> Result<PathBuf, String> {
    let key = from.key_bytes().ok_or("the other device has no key on the bus")?;
    let offer: Offer = wait(format!("waiting for {} to offer", from.name), &on, || get_sealed(b, &format!("{t}/offer.sealed"), secret))?;
    let picks = choose(&offer).ok_or("cancelled")?;
    let n = picks.ids.len();
    on(&step("waiting", format!("taking {n} of {} offered items (from {})", offer.items.len(), offer.os)));
    put_sealed(b, &format!("{t}/picks.sealed"), &key, &picks, "picks")?;
    let ep: Endpoint = wait(format!("waiting for {} to start sending", from.name), &on, || get_sealed(b, &format!("{t}/key.sealed"), secret))?;
    let dir = local_dir(here, t);
    let staging = dir.join("staging");
    let name = |id: &str| offer.items.iter().find(|i| i.id == id).map(|i| i.name.clone()).unwrap_or_else(|| id.into());
    let arrived = |id: &String| staging.join(format!("{}.done", wire::safe(id))).exists();
    let mut bytes = 0;
    // A cut connection: ask again for what hasn't arrived, up to 20 times.
    for _ in 0..20 {
        let need: Vec<String> = picks.ids.iter().filter(|id| !arrived(id)).cloned().collect();
        if need.is_empty() {
            break;
        }
        let (base, had, got) = (bytes, n - need.len(), Cell::new(0));
        let r = wire::fetch(&ep, need, &staging, |p| {
            got.set(p.bytes);
            on(&Live { state: "receiving".into(), bytes: base + p.bytes, items_done: had + p.items_done, items: n, current: name(&p.current), ..Live::default() });
        });
        bytes += got.get();
        if let Err(e) = r {
            on(&Live { state: "receiving".into(), note: format!("{e}; trying again"), bytes, items_done: n - picks.ids.iter().filter(|id| !arrived(id)).count(), items: n, ..Live::default() });
            std::thread::sleep(POLL);
        }
    }
    let missing = picks.ids.iter().filter(|id| !arrived(id)).count();
    // Tell the sender it can stop.
    let _ = wire::fetch(&ep, vec![], &staging, |_| {});
    on(&Live { state: "placing".into(), note: "putting things in place".into(), bytes, items_done: n - missing, items: n, ..Live::default() });
    let (placed, problems) = finish(&offer, &picks, here, &from.name, &dir);
    let note = format!("placed {placed} paths, {problems} problems, {missing} items missing; the brief is {}", dir.join("GOAL.md").display());
    on(&Live { state: "done".into(), note, bytes, items_done: n - missing, items: n, current: String::new(), dir: Some(dir.clone()), placed, problems, missing });
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::os::env;

    #[test]
    fn preflight_only_for_items_that_need_tools() {
        let dir = std::env::temp_dir().join(format!("windysync-preflight-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("lists")).unwrap();
        assert!(preflight(&["dev/git", "files/documents"], &dir).is_none());
        let s = preflight(&["ai/claude-code"], &dir).unwrap();
        assert!(s.contains("NEED_BREW=0") && s.contains("NEED_NODE=1"));
        std::fs::write(dir.join("lists/brew.txt"), "brew \"node\"\n").unwrap();
        let s = preflight(&["tools/brew"], &dir).unwrap();
        assert!(s.contains("NEED_BREW=1") && s.contains("NEED_NODE=1"), "a Brewfile with node needs Node too");
        std::fs::write(dir.join("preflight.sh"), s).unwrap();
        assert!(std::process::Command::new("/bin/sh").arg("-n").arg(dir.join("preflight.sh")).status().unwrap().success(), "valid sh");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_windows_receiver_takes_only_what_it_has_a_place_for() {
        let mac = env(Os::Mac, "/Users/ege");
        let zsh = Item::from(CATALOG.iter().find(|e| e.id == "shell/zsh").unwrap());
        let vscode = Item::from(CATALOG.iter().find(|e| e.id == "editors/vscode").unwrap());
        let doc = Item::folder_child("files/documents", "Notes 2026").unwrap();
        let o = Offer { os: "macOS".into(), home: mac.home.clone(), items: vec![zsh, vscode, doc] };
        assert_eq!(picks(&o, Os::Windows, &[]).ids, ["editors/vscode", "files/documents/notes-2026"]);
        assert_eq!(picks(&o, Os::Linux, &["shell".into()]).ids, ["shell/zsh"]);
        assert_eq!(picks(&o, Os::Linux, &["files/documents".into()]).ids, ["files/documents/notes-2026"]);
        let g = brief(&o, &env(Os::Windows, r"C:\Users\ege"), "Ege's MacBook", &[]);
        assert!(g.contains("`/Users/ege` on macOS") && g.contains(r"`C:\Users\ege` on Windows"));
    }

    #[test]
    fn items_group_by_the_first_part_of_their_id_in_a_fixed_order() {
        let item = |id: &str| Item { id: id.into(), name: id.into(), mac: vec![], win: vec![], linux: vec![], secret: false, note: String::new(), run: vec![] };
        let items: Vec<Item> = ["files/documents/a", "tools/brew", "zeta/x", "dev/git", "files/desktop/b", "ai/codex", "alpha/y"].map(item).into();
        let got: Vec<(String, Vec<String>)> = groups(&items).into_iter().map(|(g, v)| (g, v.iter().map(|i| i.id.clone()).collect())).collect();
        let want: Vec<(String, Vec<String>)> = [("dev", vec!["dev/git"]), ("ai", vec!["ai/codex"]), ("tools", vec!["tools/brew"]), ("files", vec!["files/documents/a", "files/desktop/b"]),
            ("zeta", vec!["zeta/x"]), ("alpha", vec!["alpha/y"])].into_iter().map(|(g, v)| (g.to_string(), v.into_iter().map(String::from).collect())).collect();
        assert_eq!(got, want);
        assert!(groups(&[]).is_empty());
    }

    /// The routines the command line and the window share, end to end: a Mac-shaped home sends to a Linux-shaped
    /// one through a folder in place of GitHub and a stream over this machine, both telling their progress.
    #[test]
    fn send_and_receive_meet_through_the_bus() {
        let base = std::env::temp_dir().join(format!("windysync-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (a, b) = (base.join("mac"), base.join("linux"));
        std::fs::create_dir_all(a.join("Documents/proj")).unwrap();
        std::fs::write(a.join("Documents/proj/main.rs"), b"fn main() {}").unwrap();
        std::fs::write(a.join(".gitconfig"), b"[user]\n\tname = ege\n").unwrap();
        std::fs::write(a.join(".zshrc"), b"export X=1\n").unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let mac = env(Os::Mac, &a.display().to_string());
        let linux = env(Os::Linux, &b.display().to_string());

        let bus = Bus::folder(&base.join("bus"));
        let (sa, sb) = (seal::random::<32>(), seal::random::<32>());
        let dev = |id: &str, name: &str, s: &[u8; 32]| Device { id: id.into(), name: name.into(), key: bus::b64(&seal::public(s)), ..Device::default() };
        let (da, db) = (dev("aaaa", "Old Mac", &sa), dev("bbbb", "New Linux", &sb));
        let r = Request { ask: bus::Ask::Send, from: "aaaa".into(), to: "bbbb".into(), at: bus::now(), answer: Answer::Pending, transfer: String::new() };
        bus.put_json(&r.path(), &r, None, "").unwrap();
        let r = approve(&bus, r, "", &Default::default(), true).unwrap();
        let t = r.transfer.clone();

        let (sent, got) = (std::sync::Mutex::new(Vec::<Live>::new()), std::sync::Mutex::new(Vec::<Live>::new()));
        let dir = std::thread::scope(|sc| {
            let sender = sc.spawn(|| send(&bus, &sa, &t, &db, &mac, |mut o| {
                // Lists run real commands (brew, cargo); this test is about the files.
                o.items.retain(|i| !i.id.starts_with("tools/"));
                Some(o)
            }, |l| sent.lock().unwrap().push(l.clone())));
            let dir = receive(&bus, &sb, &t, &da, &linux, |o| {
                assert_eq!(o.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["dev/git", "shell/zsh", "files/documents/proj"]);
                Some(picks(o, Os::Linux, &["dev".into(), "files".into()]))
            }, |l| got.lock().unwrap().push(l.clone()));
            sender.join().unwrap().unwrap();
            dir.unwrap()
        });
        let (sent, got) = (sent.into_inner().unwrap(), got.into_inner().unwrap());

        assert_eq!(std::fs::read(b.join(".gitconfig")).unwrap(), b"[user]\n\tname = ege\n");
        assert!(b.join("Documents/proj/main.rs").exists());
        assert!(!b.join(".zshrc").exists(), "only the picks travel");
        assert!(std::fs::read_to_string(dir.join("GOAL.md")).unwrap().contains("Git config"));
        let (s, g) = (sent.last().unwrap(), got.last().unwrap());
        assert_eq!((s.state.as_str(), s.items_done, s.items), ("done", 2, 2));
        assert_eq!((g.state.as_str(), g.items_done, g.items, g.missing, g.placed), ("done", 2, 2, 0, 2));
        assert_eq!(g.dir.as_deref(), Some(dir.as_path()));
        assert!(g.bytes > 0 && g.bytes == s.bytes, "{} and {}", g.bytes, s.bytes);
        assert!(sent.iter().any(|l| l.state == "sending") && got.iter().any(|l| l.state == "receiving" && l.current == "Git config"), "progress is told on the way");
        assert!(got.iter().any(|l| l.note.starts_with("waiting for Old Mac to offer")));
        assert_eq!(bus.get_json::<Session>(&format!("{t}/session.json")).unwrap().unwrap().0.state, "done");
        let _ = std::fs::remove_dir_all(&base);
    }
}

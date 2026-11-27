//! Device looks. The look code knows all three systems (it is AnySync's), but SYSTEMS holds only Windows here, so
//! every device is a PC laptop or a desktop monitor in a chassis colour. Every drawing is an original vector shape:
//! no company artwork or logo.

use std::sync::OnceLock;

use rune::tok::TextStyle;
use rune::{Color, El, Shape, canvas, el, text};
use serde::Deserialize;

use crate::me::Look;
use crate::os::Os;

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Model {
    pub id: String,
    pub family: String,
    pub year: u32,
    pub chips: Vec<String>,
    pub screen_in: f32,
    pub size_label: String,
    pub model_identifiers: Vec<String>,
    pub design: Design,
    pub colors: Vec<Colour>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Design {
    pub lid: String,
    pub notch: bool,
    pub touch_bar: bool,
    pub function_row: String,
    pub bezel: String,
    pub dimensions_mm: Dims,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Dims {
    pub width: f32,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Colour {
    pub name: String,
    pub body_hex: String,
    pub body_hex_dark: String,
    pub keyboard_hex: String,
}

#[derive(Deserialize, Default)]
struct Catalog {
    models: Vec<Model>,
}

pub fn catalog() -> &'static [Model] {
    static C: OnceLock<Vec<Model>> = OnceLock::new();
    C.get_or_init(|| {
        let mut m = serde_json::from_str::<Catalog>(include_str!("../assets/catalog/macbooks.json")).map(|c| c.models).unwrap_or_default();
        m.retain(|m| !m.colors.is_empty());
        // Newest first, Pro before Air within a year, then by size.
        m.sort_by(|a, b| b.year.cmp(&a.year).then(b.family.cmp(&a.family)).then(a.screen_in.total_cmp(&b.screen_in)));
        m
    })
}

/// The model a look names, by catalogue id or by the identifier the Mac reports (`MacBookPro17,1`); else the newest.
pub fn mac_model(id: &str) -> &'static Model {
    let c = catalog();
    c.iter().find(|m| m.id == id).or_else(|| c.iter().find(|m| m.model_identifiers.iter().any(|i| i == id))).unwrap_or(&c[0])
}

/// "MacBook Pro 13″ · M1 · 2020"
pub fn short_name(m: &Model) -> String {
    let fam = if m.family == "air" { "MacBook Air" } else { "MacBook Pro" };
    let chip = m.chips.first().map(String::as_str).unwrap_or("");
    format!("{fam} {}″ · {chip} · {}", m.size_label.trim_end_matches("-inch"), m.year)
}

// What the picker offers -----------------------------------------------------------------------------------

/// (look.os, shown as)
pub const SYSTEMS: &[(&str, &str)] = &[("windows", "Windows")];
pub const KINDS: [&str; 2] = ["laptop", "desktop"];
/// Chassis colours of a PC laptop, a monitor or a Mac desktop.
pub const CHASSIS: [(&str, &str); 4] = [("Graphite", "#3b3e45"), ("Black", "#17181b"), ("Silver", "#b9bdc4"), ("White", "#e4e4e8")];
/// os-release ids, shown as they are.
pub const DISTROS: [&str; 10] = ["ubuntu", "fedora", "debian", "arch", "linuxmint", "pop", "opensuse-tumbleweed", "manjaro", "nixos", "elementary"];

/// Whether `d` runs one of the systems in SYSTEMS, by what it reports or else by its look.
pub fn ours(d: &crate::bus::Device) -> bool {
    Os::parse(&d.os).or(Os::parse(&d.look.os)).is_some_and(|o| SYSTEMS.iter().any(|s| s.0 == os_key(o)))
}

/// The look's system key for `os`.
pub fn os_key(os: Os) -> &'static str {
    match os {
        Os::Mac => "mac",
        Os::Windows => "windows",
        Os::Linux => "linux",
    }
}

/// The colours a look can take: a Mac laptop's own, else the chassis ones.
pub fn colours(look: &Look) -> Vec<(String, String)> {
    if look.os == "mac" && look.kind == "laptop" {
        mac_model(&look.model).colors.iter().map(|c| (c.name.clone(), c.body_hex.clone())).collect()
    } else {
        CHASSIS.iter().map(|(n, h)| (n.to_string(), h.to_string())).collect()
    }
}

/// The models a look can take: catalogue ids for a Mac laptop, distro ids for Linux (the current one first when
/// the list doesn't have it), none for the rest.
pub fn models(look: &Look) -> Vec<(String, String)> {
    match (look.os.as_str(), look.kind.as_str()) {
        ("mac", "laptop") => catalog().iter().map(|m| (m.id.clone(), short_name(m))).collect(),
        ("linux", _) => {
            let mut d: Vec<String> = DISTROS.iter().map(|d| d.to_string()).collect();
            if !look.model.is_empty() && !d.contains(&look.model) {
                d.insert(0, look.model.clone());
            }
            d.into_iter().map(|d| (d.clone(), d)).collect()
        }
        _ => vec![],
    }
}

/// `look` made whole, for drawing and for the picker: a known system (this one's when unknown) and kind, a
/// model the system has, and one of its colours. A change in the picker goes through here, so switching
/// systems always lands on a look that can be drawn.
pub fn fix(look: &Look) -> Look {
    let known = |o: &str| SYSTEMS.iter().any(|s| s.0 == o);
    let os = [look.os.as_str(), os_key(Os::current())].into_iter().find(|o| known(o)).unwrap_or(SYSTEMS[0].0).to_string();
    let kind = if KINDS.contains(&look.kind.as_str()) { look.kind.clone() } else { "laptop".into() };
    let model = match (os.as_str(), kind.as_str()) {
        ("mac", "laptop") => mac_model(&look.model).id.clone(),
        ("linux", _) if look.os == "linux" && !look.model.is_empty() => look.model.clone(),
        ("linux", _) => DISTROS[0].into(),
        _ => String::new(),
    };
    let mut l = Look { os, kind, model, color: String::new() };
    let colours = colours(&l);
    let fallback = if l.os == "mac" && l.kind == "desktop" { "Silver".to_string() } else { colours[0].0.clone() };
    l.color = colours.into_iter().map(|c| c.0).find(|c| *c == look.color).unwrap_or(fallback);
    l
}

/// `look` moved to another system: its model only carries over within the same system.
pub fn with_os(look: &Look, os: &str) -> Look {
    fix(&Look { os: os.into(), model: if look.os == os { look.model.clone() } else { String::new() }, ..look.clone() })
}

/// "MacBook Pro 14″ · M3 · 2023", "Windows desktop", "Linux laptop · ubuntu".
pub fn describe(look: &Look) -> String {
    let l = fix(look);
    let sys = SYSTEMS.iter().find(|s| s.0 == l.os).map(|s| s.1).unwrap_or_default();
    match (l.os.as_str(), l.kind.as_str()) {
        ("mac", "laptop") => short_name(mac_model(&l.model)),
        ("mac", _) => "Mac desktop".into(),
        ("linux", k) => format!("Linux {k} · {}", l.model),
        (_, k) => format!("{sys} {k}"),
    }
}

// Drawing -------------------------------------------------------------------------------------------------

pub fn hx(hex: &str, or: Color) -> Color {
    rune::color::hex(hex).unwrap_or(or)
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    Color { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: a.a + (b.a - a.a) * t }
}

/// The lit side of a body colour. A light metal gets MacySync's lift towards white; a dark body only a little of
/// it, or a black laptop would come out grey (the mix is in linear light).
fn lit(body: Color, t: f32) -> Color {
    mix(body, WHITE, t * (body.luminance() / 0.25).clamp(0.06, 1.0))
}

const BLACK: Color = Color { r: 0.004, g: 0.004, b: 0.005, a: 1.0 };
/// The app's tint: a screen's glow from one corner to the other, and a Linux badge's text.
const SCREEN: [Color; 3] = [Color { r: 0.004, g: 0.028, b: 0.036, a: 1.0 }, Color { r: 0.0015, g: 0.008, b: 0.011, a: 1.0 }, Color { r: 0.008, g: 0.048, b: 0.062, a: 1.0 }];
const BADGE_TEXT: Color = Color { r: 0.55, g: 0.68, b: 0.7, a: 1.0 };
const WHITE: Color = Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };

pub const W: f32 = 1280.0;
pub const H: f32 = 780.0;
/// A 16-inch lid is this much of its width tall; the hinge sits there.
pub const LID: f32 = 0.655;

/// A layer over the whole window, in window coordinates.
pub fn full(shape: Shape) -> El {
    canvas(shape.view_box(W, H)).size(W, H).absolute(0.0, 0.0)
}

/// A drawn device, and where its screen and the strip under it (the chin) are, so the caller can put text there.
pub struct Drawn {
    pub layers: Vec<El>,
    pub screen: [f32; 4],
    pub chin: [f32; 4],
}

/// `look` drawn in the box of a 16-inch laptop whose top-left is (`x`, `y`) and whose width is `w`.
pub fn device(key: &str, look: &Look, x: f32, y: f32, w: f32) -> Drawn {
    let l = fix(look);
    let mut d = match (l.os.as_str(), l.kind.as_str()) {
        ("mac", "laptop") => {
            let m = mac_model(&l.model);
            let c = m.colors.iter().find(|c| c.name == l.color).unwrap_or(&m.colors[0]);
            laptop(key, m, c, x + w / 2.0, y + w * LID, w)
        }
        (_, kind) => {
            let hex = CHASSIS.iter().find(|c| c.0 == l.color).map(|c| c.1).unwrap_or(CHASSIS[0].1);
            if kind == "laptop" {
                // A 15.6-inch PC: as wide as the 16-inch box, thick bezels, a full function row, no notch.
                let pc = Model { screen_in: 15.6, design: Design { bezel: "thick".into(), function_row: "full".into(), lid: "flat".into(), ..Design::default() }, ..Model::default() };
                let c = Colour { body_hex: hex.into(), keyboard_hex: "#0d0e10".into(), ..Colour::default() };
                laptop(key, &pc, &c, x + w / 2.0, y + w * LID, w)
            } else {
                monitor(key, hx(hex, Color { r: 0.1, g: 0.1, b: 0.11, a: 1.0 }), x, y, w)
            }
        }
    };
    if l.os == "linux" {
        d.layers.push(badge(key, &l.model, d.chin));
    }
    d
}

/// A Linux device's distro name as a small text badge on the chin under its screen.
fn badge(key: &str, name: &str, chin: [f32; 4]) -> El {
    let size = (chin[3] * 0.6).clamp(4.0, 12.0);
    let w = name.chars().count() as f32 * size * 0.62 + size * 1.4;
    el().key(format!("{key}-badge")).absolute(chin[0] + (chin[2] - w) / 2.0, chin[1] + (chin[3] - size * 1.4) / 2.0).size(w, size * 1.4).radius(size)
        .fill(Color { a: 0.09, ..WHITE }).center()
        .child(text(name.to_string()).font(TextStyle::new(size, 600.0).mono()).color(BADGE_TEXT))
}

/// A desktop monitor on a stand in the box a laptop takes: the panel where the lid would be, the foot near where
/// the deck's front edge would be.
fn monitor(key: &str, body: Color, x: f32, y: f32, w: f32) -> Drawn {
    let dark = mix(body, BLACK, 0.45);
    let light = lit(body, 0.3);
    let (pw, ph) = (w, w * 0.6);
    let (px, py) = (x, y + w * 0.03);
    let cx = x + w / 2.0;
    let foot = y + w * 0.82;
    let mut out = vec![el().key(format!("{key}-shadow")).absolute(cx - w * 0.3, foot + w * 0.02).size(w * 0.6, w * 0.06).radius(w * 0.03).fill(Color { a: 0.55, ..BLACK }).blur(14.0)];
    // The neck from behind the panel down to a flat foot seen a little from above.
    let s = Shape::new()
        .fill(vec![[cx - w * 0.055, py + ph - 6.0], [cx + w * 0.055, py + ph - 6.0], [cx + w * 0.075, foot + 2.0], [cx - w * 0.075, foot + 2.0]], mix(body, dark, 0.5))
        .fill(vec![[cx - w * 0.2, foot], [cx + w * 0.2, foot], [cx + w * 0.24, foot + w * 0.03], [cx - w * 0.24, foot + w * 0.03]], light)
        .fill(vec![[cx - w * 0.24, foot + w * 0.03], [cx + w * 0.24, foot + w * 0.03], [cx + w * 0.238, foot + w * 0.04], [cx - w * 0.238, foot + w * 0.04]], dark);
    out.push(full(s).key(format!("{key}-stand")));
    out.push(el().key(format!("{key}-panel")).absolute(px, py).size(pw, ph).radius(w * 0.018)
        .linear_gradient(std::f32::consts::PI, [(0.0, light), (0.5, body), (1.0, dark)]));
    let rim = w * 0.008;
    out.push(el().key(format!("{key}-glass")).absolute(px + rim, py + rim).size(pw - 2.0 * rim, ph - 2.0 * rim).radius(w * 0.014).fill(BLACK));
    let (bezel, chin) = (w * 0.012, w * 0.05);
    let screen = [px + rim + bezel, py + rim + bezel, pw - 2.0 * (rim + bezel), ph - 2.0 * rim - bezel - chin];
    out.push(display(key, screen, w));
    Drawn { layers: out, screen, chin: [screen[0], screen[1] + screen[3], screen[2], chin] }
}

fn display(key: &str, s: [f32; 4], w: f32) -> El {
    el().key(format!("{key}-display")).absolute(s[0], s[1]).size(s[2], s[3]).radius(w * 0.012)
        .linear_gradient(2.4, [(0.0, SCREEN[0]), (0.55, SCREEN[1]), (1.0, SCREEN[2])])
}

/// An open laptop seen from the front and a little above, its hinge centred on (`cx`, `hinge_y`). `width` is a
/// 16-inch lid's width; smaller models are drawn smaller by their real width.
pub fn laptop(key: &str, m: &Model, c: &Colour, cx: f32, hinge_y: f32, width: f32) -> Drawn {
    let body = hx(&c.body_hex, Color { r: 0.5, g: 0.5, b: 0.52, a: 1.0 });
    let dark = hx(&c.body_hex_dark, mix(body, BLACK, 0.45));
    let keys = hx(&c.keyboard_hex, BLACK);
    let light = lit(body, 0.35);
    let d = &m.design;
    let w = width * (if d.dimensions_mm.width > 0.0 { d.dimensions_mm.width / 355.7 } else { m.screen_in / 16.2 }).clamp(0.7, 1.0);
    let lid_h = w * 0.655;
    let (x0, y0) = (cx - w / 2.0, hinge_y - lid_h);
    let mut out = Vec::new();

    // Shadow under the deck.
    let depth = w * 0.25;
    out.push(el().key(format!("{key}-shadow")).absolute(cx - w * 0.6, hinge_y + depth * 0.55).size(w * 1.2, depth * 0.7).radius(depth).fill(Color { a: 0.55, ..BLACK }).blur(18.0));

    // Lid: metal edge, black glass, the display.
    out.push(el().key(format!("{key}-lid")).absolute(x0, y0).size(w, lid_h).radius(w * 0.032)
        .linear_gradient(std::f32::consts::PI, [(0.0, light), (0.5, body), (1.0, dark)]));
    let rim = w * 0.009;
    out.push(el().key(format!("{key}-glass")).absolute(x0 + rim, y0 + rim).size(w - 2.0 * rim, lid_h - 2.0 * rim).radius(w * 0.026).fill(BLACK));
    let bezel = if d.bezel == "thick" { w * 0.036 } else { w * 0.016 };
    let bottom_chin = if d.notch { bezel } else { w * 0.05 };
    let screen = [x0 + rim + bezel, y0 + rim + bezel, w - 2.0 * (rim + bezel), lid_h - 2.0 * rim - bezel - bottom_chin];
    out.push(display(key, screen, w));
    if d.notch {
        let nw = w * 0.115;
        out.push(el().key(format!("{key}-notch")).absolute(cx - nw / 2.0, screen[1] - 1.0).size(nw, w * 0.027).radius(w * 0.008).fill(BLACK));
    } else {
        out.push(el().key(format!("{key}-cam")).absolute(cx - 2.0, y0 + rim + bezel * 0.35).size(4.0, 4.0).radius(2.0).fill(Color { r: 0.03, g: 0.03, b: 0.05, a: 1.0 }));
    }

    // Deck in perspective: (u, t) with u across (-0.5..0.5) and t from the hinge to the front edge (0..1).
    let top = hinge_y + w * 0.008;
    let p = |u: f32, t: f32| [cx + u * w * (1.0 + 0.13 * t), top + t * depth];
    let quad = |u0: f32, u1: f32, t0: f32, t1: f32| vec![p(u0, t0), p(u1, t0), p(u1, t1), p(u0, t1)];
    let mut s = Shape::new();
    s = s.fill(vec![[x0 + w * 0.03, hinge_y - 1.0], [x0 + w * 0.97, hinge_y - 1.0], [x0 + w * 0.97, top + 1.0], [x0 + w * 0.03, top + 1.0]], mix(dark, BLACK, 0.5));
    let bands = 8;
    for i in 0..bands {
        let (t0, t1) = (i as f32 / bands as f32, (i + 1) as f32 / bands as f32);
        s = s.fill(quad(-0.5, 0.5, t0, t1 + 0.002), mix(light, body, t0 * 0.9));
    }
    // Keyboard: a well a shade darker than the body, black keys in rows.
    let (kt0, kt1, ku) = (0.07, 0.56, 0.41);
    s = s.fill(quad(-ku, ku, kt0, kt1), mix(body, dark, 0.55));
    let rows = 6;
    let fn_row = if d.touch_bar { 0.55 } else if d.function_row == "half-height" { 0.6 } else { 1.0 };
    let unit = (kt1 - kt0) / (rows as f32 - 1.0 + fn_row);
    let gap = 0.12;
    let mut t = kt0;
    for r in 0..rows {
        let h = if r == 0 { unit * fn_row } else { unit };
        let (a, b) = (t + unit * gap * 0.5, t + h - unit * gap * 0.5);
        if r == 0 && d.touch_bar {
            s = s.fill(quad(-ku + 0.012, ku - 0.012, a, b), Color { r: 0.01, g: 0.012, b: 0.02, a: 1.0 });
        } else if r == rows - 1 {
            // Bottom row: modifiers and the long space bar.
            let cuts = [-ku, -0.31, -0.23, -0.15, 0.13, 0.21, 0.29, ku];
            for k in cuts.windows(2) {
                s = s.fill(quad(k[0] + 0.004, k[1] - 0.004, a, b), keys);
            }
        } else {
            let n = 14;
            for k in 0..n {
                let (u0, u1) = (-ku + 2.0 * ku * k as f32 / n as f32, -ku + 2.0 * ku * (k + 1) as f32 / n as f32);
                s = s.fill(quad(u0 + 0.004, u1 - 0.004, a, b), keys);
            }
        }
        t += h;
    }
    // Trackpad, larger on the bigger lids.
    let tp = if m.screen_in > 14.0 { 0.2 } else { 0.165 };
    s = s.fill(quad(-tp, tp, 0.62, 0.95), mix(body, light, 0.25));
    s = s.stroke(quad(-tp, tp, 0.62, 0.95).into_iter().chain([p(-tp, 0.62)]).collect::<Vec<_>>(), 1.0, Color { a: 0.5, ..dark });
    // Front edge, thinner on the wedge, with the scoop for a thumb.
    let edge = if d.lid == "wedge" { w * 0.009 } else { w * 0.017 };
    let (fl, fr) = (p(-0.5, 1.0), p(0.5, 1.0));
    s = s.fill(vec![fl, fr, [fr[0] - 3.0, fr[1] + edge], [fl[0] + 3.0, fl[1] + edge]], dark);
    s = s.fill(vec![[cx - w * 0.07, fl[1]], [cx + w * 0.07, fl[1]], [cx + w * 0.05, fl[1] + edge * 0.6], [cx - w * 0.05, fl[1] + edge * 0.6]], mix(dark, BLACK, 0.4));
    out.push(full(s).key(format!("{key}-deck")));
    Drawn { layers: out, screen, chin: [screen[0], screen[1] + screen[3], screen[2], bottom_chin] }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(os: &str, kind: &str, model: &str, color: &str) -> Look {
        Look { os: os.into(), kind: kind.into(), model: model.into(), color: color.into() }
    }

    #[test]
    fn every_look_is_a_windows_one() {
        // A look from another system, or nonsense, becomes a Windows one in a chassis colour.
        assert_eq!(fix(&look("mac", "laptop", "MacBookPro17,1", "Midnight")), look("windows", "laptop", "", "Graphite"));
        assert_eq!(fix(&look("linux", "desktop", "ubuntu", "Silver")), look("windows", "desktop", "", "Silver"));
        assert_eq!(fix(&look("beos", "tower", "", "")), look("windows", "laptop", "", "Graphite"));
        assert_eq!(fix(&look("windows", "desktop", "", "Black")), look("windows", "desktop", "", "Black"));
        assert!(models(&look("windows", "laptop", "", "")).is_empty());
        assert_eq!(describe(&look("windows", "desktop", "", "")), "Windows desktop");
        // Only Windows devices are listed or accepted.
        let d = |os: &str| crate::bus::Device { os: os.into(), ..Default::default() };
        assert!(ours(&d("Windows")));
        assert!(!ours(&d("macOS")) && !ours(&d("Linux")) && !ours(&d("")));
    }
}

//! What WindySync knows how to move, by logical id, with each part's place on each system. A part is one slot: the
//! sender's path in slot i lands at the receiver's path in slot i. A system with no place for an item has an empty
//! list, and the item isn't offered to it.

use crate::os::{Env, Os};

pub struct Entry {
    pub id: &'static str,
    pub name: &'static str,
    pub mac: &'static [&'static str],
    pub win: &'static [&'static str],
    pub linux: &'static [&'static str],
    pub secret: bool,
    /// For the agent finishing the setup.
    pub note: &'static str,
}

impl Entry {
    pub fn templates(&self, os: Os) -> &'static [&'static str] {
        match os {
            Os::Mac => self.mac,
            Os::Windows => self.win,
            Os::Linux => self.linux,
        }
    }

    /// (sender's path, receiver's path) per slot both systems have.
    pub fn pairs(&self, from: &Env, to: &Env) -> Vec<(String, String)> {
        let (a, b) = (self.templates(from.os), self.templates(to.os));
        a.iter().zip(b).filter_map(|(x, y)| Some((from.resolve(x)?, to.resolve(y)?))).collect()
    }
}

/// An item as offered: owned, with its templates for every system, so the receiver places it by the offer itself
/// even when its own catalogue is older or newer.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub mac: Vec<String>,
    pub win: Vec<String>,
    pub linux: Vec<String>,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub note: String,
    /// A list made on the sending system by this command (see LISTS); then the item has no paths.
    #[serde(default)]
    pub run: Vec<String>,
}

impl Item {
    pub fn templates(&self, os: Os) -> &[String] {
        match os {
            Os::Mac => &self.mac,
            Os::Windows => &self.win,
            Os::Linux => &self.linux,
        }
    }

    /// One top-level file or folder of the person's `folder` (`files/documents`), by name.
    pub fn folder_child(folder: &str, name: &str) -> Option<Item> {
        let (_, mac, win, linux) = FOLDERS.iter().find(|f| f.0 == folder)?;
        let t = |root: &str| vec![format!("{root}/{name}")];
        Some(Item { id: format!("{folder}/{}", slug(name)), name: name.into(), mac: t(mac), win: t(win), linux: t(linux), secret: false, note: String::new(), run: vec![] })
    }
}

impl From<&Entry> for Item {
    fn from(e: &Entry) -> Item {
        let own = |l: &[&str]| l.iter().map(|s| s.to_string()).collect();
        Item { id: e.id.into(), name: e.name.into(), mac: own(e.mac), win: own(e.win), linux: own(e.linux), secret: e.secret, note: e.note.into(), run: vec![] }
    }
}

/// A name as an id part: lowercase letters, digits and dashes.
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

const fn e(id: &'static str, name: &'static str, mac: &'static [&'static str], win: &'static [&'static str], linux: &'static [&'static str], note: &'static str) -> Entry {
    Entry { id, name, mac, win, linux, secret: false, note }
}

const fn s(id: &'static str, name: &'static str, mac: &'static [&'static str], win: &'static [&'static str], linux: &'static [&'static str], note: &'static str) -> Entry {
    Entry { id, name, mac, win, linux, secret: true, note }
}

const VSCODE_NOTE: &str = "Extensions reinstall from the editor's list in `lists/` of the transfer folder; settings that name paths of the old system need the new ones.";

pub const CATALOG: &[Entry] = &[
    // Shell and git
    e("dev/git", "Git config", &["~/.gitconfig", "~/.gitignore_global"], &["~/.gitconfig", "~/.gitignore_global"], &["~/.gitconfig", "~/.gitignore_global"],
        "Check `core.autocrlf`, `credential.helper` and any path in it: they differ between systems."),
    s("secrets/ssh", "SSH keys and config", &["~/.ssh"], &["~/.ssh"], &["~/.ssh"],
        "On macOS and Linux keep ~/.ssh at 700 and private keys at 600. `UseKeychain` in ssh_config is macOS only."),
    s("secrets/gnupg", "GnuPG", &["~/.gnupg"], &["%APPDATA%/gnupg"], &["~/.gnupg"], "On macOS and Linux keep ~/.gnupg at 700."),
    e("shell/zsh", "zsh config", &["~/.zshrc", "~/.zprofile"], &[], &["~/.zshrc", "~/.zprofile"], "Lines for tools of the old system (brew shellenv, paths) need the new system's."),
    e("shell/bash", "bash config", &["~/.bashrc", "~/.bash_profile"], &[], &["~/.bashrc", "~/.bash_profile"], ""),
    e("shell/powershell", "PowerShell profile", &["~/.config/powershell/Microsoft.PowerShell_profile.ps1"],
        &["~/Documents/PowerShell/Microsoft.PowerShell_profile.ps1"], &["$XDG_CONFIG_HOME/powershell/Microsoft.PowerShell_profile.ps1"], ""),
    // Editors
    e("editors/vscode", "VS Code settings", &["~/Library/Application Support/Code/User/settings.json", "~/Library/Application Support/Code/User/keybindings.json", "~/Library/Application Support/Code/User/snippets"],
        &["%APPDATA%/Code/User/settings.json", "%APPDATA%/Code/User/keybindings.json", "%APPDATA%/Code/User/snippets"],
        &["$XDG_CONFIG_HOME/Code/User/settings.json", "$XDG_CONFIG_HOME/Code/User/keybindings.json", "$XDG_CONFIG_HOME/Code/User/snippets"], VSCODE_NOTE),
    e("editors/cursor", "Cursor settings", &["~/Library/Application Support/Cursor/User/settings.json", "~/Library/Application Support/Cursor/User/keybindings.json", "~/Library/Application Support/Cursor/User/snippets"],
        &["%APPDATA%/Cursor/User/settings.json", "%APPDATA%/Cursor/User/keybindings.json", "%APPDATA%/Cursor/User/snippets"],
        &["$XDG_CONFIG_HOME/Cursor/User/settings.json", "$XDG_CONFIG_HOME/Cursor/User/keybindings.json", "$XDG_CONFIG_HOME/Cursor/User/snippets"], VSCODE_NOTE),
    e("editors/zed", "Zed settings", &["~/.config/zed/settings.json", "~/.config/zed/keymap.json"], &["%APPDATA%/Zed/settings.json", "%APPDATA%/Zed/keymap.json"],
        &["$XDG_CONFIG_HOME/zed/settings.json", "$XDG_CONFIG_HOME/zed/keymap.json"], ""),
    // Terminals
    e("terminals/wave", "Wave config", &["~/.config/waveterm"], &["~/.config/waveterm"], &["~/.config/waveterm"],
        "Widget commands and connections that name the old system's shells or paths need the new ones."),
    // Notes
    e("notes/obsidian", "Obsidian vault list", &["~/Library/Application Support/obsidian/obsidian.json"], &["%APPDATA%/obsidian/obsidian.json"],
        &["$XDG_CONFIG_HOME/obsidian/obsidian.json"], "Lists the vaults by absolute path: rewrite them to where the vault folders landed, and check each opens."),
    // Browsers: bookmarks only; saved passwords and cookies are locked to the old system's key store
    e("browsers/chrome", "Chrome bookmarks", &["~/Library/Application Support/Google/Chrome/Default/Bookmarks"],
        &["%LOCALAPPDATA%/Google/Chrome/User Data/Default/Bookmarks"], &["$XDG_CONFIG_HOME/google-chrome/Default/Bookmarks"], BROWSER_NOTE),
    e("browsers/brave", "Brave bookmarks", &["~/Library/Application Support/BraveSoftware/Brave-Browser/Default/Bookmarks"],
        &["%LOCALAPPDATA%/BraveSoftware/Brave-Browser/User Data/Default/Bookmarks"], &["$XDG_CONFIG_HOME/BraveSoftware/Brave-Browser/Default/Bookmarks"], BROWSER_NOTE),
    e("browsers/edge", "Edge bookmarks", &["~/Library/Application Support/Microsoft Edge/Default/Bookmarks"],
        &["%LOCALAPPDATA%/Microsoft/Edge/User Data/Default/Bookmarks"], &["$XDG_CONFIG_HOME/microsoft-edge/Default/Bookmarks"], BROWSER_NOTE),
    e("browsers/chromium", "Chromium bookmarks", &["~/Library/Application Support/Chromium/Default/Bookmarks"],
        &["%LOCALAPPDATA%/Chromium/User Data/Default/Bookmarks"], &["$XDG_CONFIG_HOME/chromium/Default/Bookmarks"], BROWSER_NOTE),
    // AI tools: settings, never the logins
    e("ai/claude-code", "Claude Code settings, skills, agents", &["~/.claude/settings.json", "~/.claude/CLAUDE.md", "~/.claude/skills", "~/.claude/agents", "~/.claude/commands"],
        &["~/.claude/settings.json", "~/.claude/CLAUDE.md", "~/.claude/skills", "~/.claude/agents", "~/.claude/commands"],
        &["~/.claude/settings.json", "~/.claude/CLAUDE.md", "~/.claude/skills", "~/.claude/agents", "~/.claude/commands"],
        "Install Claude Code, run `claude` and sign in. Hooks and statusline commands that call the old system's shell or paths need rewriting."),
    e("ai/codex", "Codex config", &["~/.codex/config.toml", "~/.codex/AGENTS.md"], &["~/.codex/config.toml", "~/.codex/AGENTS.md"], &["~/.codex/config.toml", "~/.codex/AGENTS.md"],
        "Install Codex and run `codex login`."),
];

const BROWSER_NOTE: &str = "Only the bookmarks cross systems: saved passwords and cookies are encrypted with a key the old system's key store holds. Quit the browser before placing; for passwords use the browser's own sync, or its password export and import.";

/// The person's own folders: each top-level item in them can be picked; the folder itself lands in the same
/// named place (macOS's Movies is Videos elsewhere).
pub const FOLDERS: &[(&str, &str, &str, &str)] = &[
    ("files/desktop", "~/Desktop", "~/Desktop", "~/Desktop"),
    ("files/documents", "~/Documents", "~/Documents", "~/Documents"),
    ("files/downloads", "~/Downloads", "~/Downloads", "~/Downloads"),
    ("files/pictures", "~/Pictures", "~/Pictures", "~/Pictures"),
    ("files/music", "~/Music", "~/Music", "~/Music"),
    ("files/videos", "~/Movies", "~/Videos", "~/Videos"),
];

/// What is installed on the sending system, as the system's own tools list it, for the agent to install on the
/// other. The command runs when the item is sent; `{out}` stands for a file it writes instead of printing.
pub struct List {
    pub id: &'static str,
    pub name: &'static str,
    pub on: &'static [Os],
    pub run: &'static [&'static str],
    pub note: &'static str,
}

const ALL: &[Os] = &[Os::Mac, Os::Windows, Os::Linux];
const PACKAGES: &str = "Install the apps and tools named here with this system's own package manager; many names are libraries pulled in by others, so skip those. Say in TODO-FOR-YOU.md which have no match here.";

pub const LISTS: &[List] = &[
    List { id: "tools/vscode-extensions", name: "VS Code extensions", on: ALL, run: &["code", "--list-extensions"], note: "Install each with `code --install-extension <id>`." },
    List { id: "tools/cursor-extensions", name: "Cursor extensions", on: ALL, run: &["cursor", "--list-extensions"], note: "Install each with `cursor --install-extension <id>`." },
    List { id: "tools/brew", name: "Homebrew packages", on: &[Os::Mac, Os::Linux], run: &["brew", "bundle", "dump", "--file=-"], note: "A Brewfile: with Homebrew here, `brew bundle --file <it>` after removing what this system can't run (casks off macOS). Otherwise see the other lists' note." },
    List { id: "tools/winget", name: "winget packages", on: &[Os::Windows], run: &["winget", "export", "-o", "{out}", "--accept-source-agreements"], note: "From `winget export`: on Windows, `winget import -i <it>`. Elsewhere, install the apps named in it with this system's package manager." },
    List { id: "tools/scoop", name: "Scoop packages", on: &[Os::Windows], run: &["scoop", "export"], note: PACKAGES },
    List { id: "tools/apt", name: "apt packages installed by hand", on: &[Os::Linux], run: &["apt-mark", "showmanual"], note: PACKAGES },
    List { id: "tools/dnf", name: "dnf packages installed by hand", on: &[Os::Linux], run: &["dnf", "repoquery", "--userinstalled"], note: PACKAGES },
    List { id: "tools/pacman", name: "pacman packages installed by hand", on: &[Os::Linux], run: &["pacman", "-Qqe"], note: PACKAGES },
    List { id: "tools/flatpak", name: "Flatpak apps", on: &[Os::Linux], run: &["flatpak", "list", "--app", "--columns=application"], note: "On Linux, `flatpak install <id>` for each." },
    List { id: "tools/snap", name: "Snap packages", on: &[Os::Linux], run: &["snap", "list"], note: PACKAGES },
    List { id: "tools/npm", name: "npm global packages", on: ALL, run: &["npm", "ls", "-g", "--depth=0"], note: "Install each with `npm install -g <name>` once Node is here." },
    List { id: "tools/cargo", name: "cargo-installed tools", on: ALL, run: &["cargo", "install", "--list"], note: "Install each with `cargo install <name>` once Rust is here." },
    List { id: "tools/pipx", name: "pipx tools", on: ALL, run: &["pipx", "list", "--short"], note: "Install each with `pipx install <name>` once pipx is here." },
];

impl From<&List> for Item {
    fn from(l: &List) -> Item {
        Item { id: l.id.into(), name: l.name.into(), mac: vec![], win: vec![], linux: vec![], secret: false, note: l.note.into(), run: l.run.iter().map(|s| s.to_string()).collect() }
    }
}

/// Whether `prog` is on this system's PATH.
pub fn on_path(prog: &str) -> bool {
    let exts: &[&str] = if cfg!(windows) { &["", ".exe", ".cmd", ".bat"] } else { &[""] };
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| exts.iter().any(|x| d.join(format!("{prog}{x}")).is_file())))
}

/// A list's text: what the command printed (or wrote to `{out}`), and why it failed when it did.
pub fn run_list(run: &[String]) -> Vec<u8> {
    // One file per call: tests run lists side by side in one process.
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let out = std::env::temp_dir().join(format!("windysync-list-{}-{n}.txt", std::process::id()));
    let args: Vec<String> = run[1..].iter().map(|a| a.replace("{out}", &out.display().to_string())).collect();
    // `code` and `npm` are .cmd scripts on Windows, which only cmd runs.
    let mut cmd = if cfg!(windows) { let mut c = std::process::Command::new("cmd"); c.arg("/C").arg(&run[0]); c } else { std::process::Command::new(&run[0]) };
    let text = match cmd.args(&args).stdin(std::process::Stdio::null()).output() {
        Ok(o) => {
            let mut t = if run.iter().any(|a| a.contains("{out}")) { std::fs::read(&out).unwrap_or_default() } else { o.stdout };
            if !o.status.success() {
                t.extend(format!("\n# {} ended with {}: {}\n", run.join(" "), o.status, String::from_utf8_lossy(&o.stderr).trim()).into_bytes());
            }
            t
        }
        Err(e) => format!("# couldn't run {}: {e}\n", run[0]).into_bytes(),
    };
    let _ = std::fs::remove_file(&out);
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::os::env;
    use std::collections::HashSet;

    #[test]
    fn every_entry_is_whole() {
        let mut ids = HashSet::new();
        for e in CATALOG {
            assert!(ids.insert(e.id), "{} twice", e.id);
            let lens: Vec<usize> = [e.mac, e.win, e.linux].iter().map(|l| l.len()).filter(|n| *n > 0).collect();
            assert!(lens.windows(2).all(|w| w[0] == w[1]), "{}: systems with a place need the same slots", e.id);
            for os in [Os::Mac, Os::Windows, Os::Linux] {
                let here = env(os, "/h");
                for t in e.templates(os) {
                    assert!(here.resolve(t).is_some(), "{}: {t} names a root {} doesn't have", e.id, os.name());
                }
            }
            assert!(!e.note.contains('\u{2014}') && !e.note.contains('\u{2013}'));
        }
        for l in LISTS {
            assert!(ids.insert(l.id), "{} twice", l.id);
            assert!(!l.run.is_empty() && !l.on.is_empty() && !l.note.contains('\u{2014}') && !l.note.contains('\u{2013}'), "{}", l.id);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_list_is_what_its_command_printed_or_wrote() {
        let own = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(run_list(&own(&["echo", "a b"])), b"a b\n");
        assert_eq!(run_list(&own(&["sh", "-c", "echo hi > \"$0\"", "{out}"])), b"hi\n");
        let failed = String::from_utf8(run_list(&own(&["sh", "-c", "echo part; echo why >&2; exit 3"]))).unwrap();
        assert!(failed.starts_with("part\n") && failed.contains("why"), "{failed}");
        assert!(String::from_utf8(run_list(&own(&["no-such-tool-windysync"]))).unwrap().starts_with("# couldn't run"));
    }

    #[test]
    fn a_mac_item_lands_in_windows_own_place() {
        let mac = env(Os::Mac, "/Users/ege");
        let win = env(Os::Windows, r"C:\Users\ege");
        let lin = env(Os::Linux, "/home/ege");
        let vscode = CATALOG.iter().find(|e| e.id == "editors/vscode").unwrap();
        assert_eq!(vscode.pairs(&mac, &win)[0], ("/Users/ege/Library/Application Support/Code/User/settings.json".into(),
            r"C:\Users\ege\AppData\Roaming\Code\User\settings.json".into()));
        assert_eq!(vscode.pairs(&win, &lin)[2].1, "/home/ege/.config/Code/User/snippets");
        // zsh has no place on Windows: nothing to pair.
        let zsh = CATALOG.iter().find(|e| e.id == "shell/zsh").unwrap();
        assert!(zsh.pairs(&mac, &win).is_empty());
        assert_eq!(zsh.pairs(&mac, &lin).len(), 2);
    }
}

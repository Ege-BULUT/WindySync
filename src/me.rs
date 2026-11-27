//! This device: its id, name, look and key pair, kept in WindySync's own folder (`~/.config/windysync`, on Windows
//! `%APPDATA%\windysync`; WINDYSYNC_HOME overrides), and the GitHub token the bus uses.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::os::{Env, Os};
use crate::seal;

pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("WINDYSYNC_HOME") {
        return PathBuf::from(d);
    }
    let env = Env::here();
    let t = if env.os == Os::Windows { "%APPDATA%/windysync" } else { "~/.config/windysync" };
    PathBuf::from(env.resolve(t).unwrap_or_default())
}

/// How the device is drawn: a Mac model, a Windows desktop or laptop, a Linux one with its distro's name.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Look {
    /// "mac", "windows", "linux"
    pub os: String,
    /// "laptop" or "desktop"
    pub kind: String,
    /// The Mac model or the Linux distro's id (`ubuntu`, `fedora`, `arch`…).
    pub model: String,
    pub color: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Config {
    pub id: String,
    pub name: String,
    pub look: Look,
    pub bus: String,
    pub registered: bool,
}

impl Config {
    pub fn load() -> Config {
        let mut c: Config = fs::read(dir().join("config.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if c.id.is_empty() {
            c.id = seal::random::<4>().iter().map(|b| format!("{b:02x}")).collect();
            c.name = computer_name();
            c.look = guess_look();
            c.save();
        }
        c
    }

    pub fn save(&self) {
        let _ = fs::create_dir_all(dir());
        let _ = fs::write(dir().join("config.json"), serde_json::to_vec_pretty(self).unwrap());
    }
}

fn run(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd).args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

pub fn computer_name() -> String {
    let n = match Os::current() {
        Os::Mac => run("scutil", &["--get", "ComputerName"]),
        Os::Windows => std::env::var("COMPUTERNAME").unwrap_or_default(),
        Os::Linux => fs::read_to_string("/etc/hostname").unwrap_or_default().trim().to_string(),
    };
    if n.is_empty() { run("hostname", &[]) } else { n }
}

/// A Linux distro's id from os-release text (`ID=ubuntu`, quotes or not).
pub fn distro(os_release: &str) -> String {
    os_release.lines().find_map(|l| l.strip_prefix("ID=")).map(|v| v.trim_matches('"').to_lowercase()).unwrap_or_default()
}

fn guess_look() -> Look {
    let os = Os::current();
    // A battery means a laptop on all three.
    let laptop = match os {
        Os::Mac => run("pmset", &["-g", "batt"]).contains("InternalBattery"),
        Os::Linux => fs::read_dir("/sys/class/power_supply").map(|d| d.filter_map(Result::ok).any(|e| e.file_name().to_string_lossy().starts_with("BAT"))).unwrap_or(false),
        Os::Windows => run("powershell", &["-NoProfile", "-Command", "(Get-CimInstance Win32_Battery) -ne $null"]).eq_ignore_ascii_case("true"),
    };
    Look {
        os: match os { Os::Mac => "mac", Os::Windows => "windows", Os::Linux => "linux" }.into(),
        kind: if laptop { "laptop" } else { "desktop" }.into(),
        model: match os {
            Os::Mac => run("sysctl", &["-n", "hw.model"]),
            Os::Linux => distro(&fs::read_to_string("/etc/os-release").unwrap_or_default()),
            Os::Windows => String::new(),
        },
        color: String::new(),
    }
}

/// The device key pair, made on first use. The secret stays in a file only this user can read.
pub fn keys() -> ([u8; 32], [u8; 32]) {
    let path = dir().join("device.key");
    let secret: [u8; 32] = match fs::read(&path).ok().and_then(|b| b.try_into().ok()) {
        Some(s) => s,
        None => {
            let s = seal::random::<32>();
            let _ = fs::create_dir_all(dir());
            let _ = fs::write(&path, s);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
            }
            s
        }
    };
    (secret, seal::public(&secret))
}

/// This device's record on the bus: who it is, the system it runs, its look and its public key.
pub fn device(cfg: &Config) -> crate::bus::Device {
    let (_, public) = keys();
    let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default();
    crate::bus::Device { id: cfg.id.clone(), name: cfg.name.clone(), os: Os::current().name().into(), look: cfg.look.clone(), key: crate::bus::b64(&public), user,
        seen: crate::bus::now(), version: env!("CARGO_PKG_VERSION").into(), ..Default::default() }
}

/// WINDYSYNC_TOKEN, else the GitHub CLI's.
pub fn token() -> Option<String> {
    if let Ok(t) = std::env::var("WINDYSYNC_TOKEN") {
        return Some(t).filter(|t| !t.is_empty());
    }
    let home = Env::here().home;
    let local = format!("{home}/.local/bin/gh");
    ["gh", "/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh", &local].into_iter().map(|gh| run(gh, &["auth", "token"])).find(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distro_ids_from_os_release() {
        assert_eq!(distro("NAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=debian\n"), "ubuntu");
        assert_eq!(distro("NAME=Fedora Linux\nID=\"fedora\"\n"), "fedora");
        assert_eq!(distro("NAME=Arch\n"), "");
    }
}

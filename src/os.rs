//! The three systems the *ySync apps know, and where each keeps a person's things. Paths are written as templates
//! (`~/…`, `%APPDATA%\…`, `$XDG_CONFIG_HOME/…`) and resolved for one system at a time, from its home folder and
//! environment, so the mapping can be checked for all three on any of them.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Os {
    Mac,
    Windows,
    Linux,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::Mac
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Os::Mac => "macOS",
            Os::Windows => "Windows",
            Os::Linux => "Linux",
        }
    }

    /// From `name()`, or from a look's `os` ("mac", "windows", "linux").
    pub fn parse(s: &str) -> Option<Os> {
        match s.to_lowercase().as_str() {
            "macos" | "mac" => Some(Os::Mac),
            "windows" => Some(Os::Windows),
            "linux" => Some(Os::Linux),
            _ => None,
        }
    }

    pub fn sep(self) -> char {
        if self == Os::Windows { '\\' } else { '/' }
    }
}

/// One system's home folder and the environment variables its templates may name.
#[derive(Debug, Clone)]
pub struct Env {
    pub os: Os,
    pub home: String,
    pub vars: HashMap<String, String>,
}

impl Env {
    /// This machine's.
    pub fn here() -> Env {
        let os = Os::current();
        let home = std::env::var(if os == Os::Windows { "USERPROFILE" } else { "HOME" }).unwrap_or_default();
        let vars = ["APPDATA", "LOCALAPPDATA", "XDG_CONFIG_HOME", "XDG_DATA_HOME"].iter()
            .filter_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()).map(|v| (k.to_string(), v))).collect();
        Env { os, home, vars }
    }

    /// A template's path on this system, with this system's separators; None when it names a root this system
    /// doesn't have (a `%APPDATA%` path on Linux).
    pub fn resolve(&self, template: &str) -> Option<String> {
        let (root, rest) = match template.split_once(['/', '\\']) {
            Some((r, rest)) => (r, rest),
            None => (template, ""),
        };
        let h = self.home.trim_end_matches(['/', '\\']);
        let sep = self.os.sep();
        let base = match (root, self.os) {
            ("~", _) => h.to_string(),
            ("%APPDATA%", Os::Windows) => self.var("APPDATA").unwrap_or_else(|| format!("{h}{sep}AppData{sep}Roaming")),
            ("%LOCALAPPDATA%", Os::Windows) => self.var("LOCALAPPDATA").unwrap_or_else(|| format!("{h}{sep}AppData{sep}Local")),
            ("$XDG_CONFIG_HOME", Os::Linux) => self.var("XDG_CONFIG_HOME").unwrap_or_else(|| format!("{h}/.config")),
            ("$XDG_DATA_HOME", Os::Linux) => self.var("XDG_DATA_HOME").unwrap_or_else(|| format!("{h}/.local/share")),
            _ => return None,
        };
        let rest: Vec<&str> = rest.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
        Some(if rest.is_empty() { base } else { format!("{base}{sep}{}", rest.join(&sep.to_string())) })
    }

    fn var(&self, k: &str) -> Option<String> {
        self.vars.get(k).map(|v| v.trim_end_matches(['/', '\\']).to_string())
    }
}

#[cfg(test)]
pub fn env(os: Os, home: &str) -> Env {
    Env { os, home: home.into(), vars: HashMap::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_resolve_per_system() {
        let mac = env(Os::Mac, "/Users/ege");
        let win = env(Os::Windows, r"C:\Users\ege");
        let lin = env(Os::Linux, "/home/ege/");
        assert_eq!(mac.resolve("~/Library/Application Support/Code/User").unwrap(), "/Users/ege/Library/Application Support/Code/User");
        assert_eq!(win.resolve(r"%APPDATA%\Code\User").unwrap(), r"C:\Users\ege\AppData\Roaming\Code\User");
        assert_eq!(win.resolve("~/.ssh").unwrap(), r"C:\Users\ege\.ssh");
        assert_eq!(lin.resolve("$XDG_CONFIG_HOME/Code/User").unwrap(), "/home/ege/.config/Code/User");
        assert_eq!(lin.resolve("~").unwrap(), "/home/ege");
        // A root the system doesn't have.
        assert!(lin.resolve(r"%APPDATA%\Code").is_none());
        assert!(mac.resolve("$XDG_CONFIG_HOME/x").is_none());
        // The environment wins over the default.
        let mut w = win.clone();
        w.vars.insert("APPDATA".into(), r"D:\Roaming\".into());
        assert_eq!(w.resolve(r"%APPDATA%\Code").unwrap(), r"D:\Roaming\Code");
    }
}

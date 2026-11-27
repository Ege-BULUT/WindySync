//! The bus: a private GitHub repo both devices read and write through the contents API, in place of a server.
//!
//!   devices/<id>.json                         every device: id, name, system, look, public key, hardware
//!   requests/<time>_<from>_<to>.json           a send or receive request and its answer
//!   transfers/<time>_<sender>_<receiver>/
//!     session.json                             who, when, bytes, average speed, state (the sender writes it)
//!     receiver.json                            what the receiver is doing with it (the receiver writes it)
//!     offer.sealed                             what the sender offers, sealed to the receiver
//!     picks.sealed                             what the receiver takes, sealed to the sender
//!     key.sealed                               the stream key and where to connect, sealed to the receiver
//!
//! Each file has one writer, so two devices never race for the same sha. Polls send the last ETag: GitHub
//! doesn't count a 304 against the hourly quota.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::me::Look;

const API: &str = "https://api.github.com";

pub struct Bus {
    pub repo: String,
    token: String,
    cache: Mutex<HashMap<String, (String, Value)>>,
    /// A local folder in place of the repo, same paths, for tests of a whole transfer without GitHub.
    dir: Option<std::path::PathBuf>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).http_status_as_error(false).build().into()
}

impl Bus {
    pub fn new(repo: &str, token: &str) -> Bus {
        Bus { repo: repo.trim().trim_start_matches("https://github.com/").trim_end_matches('/').into(), token: token.into(), cache: Mutex::default(), dir: None }
    }

    #[cfg(test)]
    pub fn folder(dir: &std::path::Path) -> Bus {
        Bus { repo: String::new(), token: String::new(), cache: Mutex::default(), dir: Some(dir.into()) }
    }

    /// GET with the cached ETag; None for a 404.
    fn get_value(&self, path: &str) -> Result<Option<Value>, String> {
        let url = format!("{API}/repos/{}/contents/{path}", self.repo);
        let etag = self.cache.lock().unwrap().get(path).map(|c| c.0.clone());
        let mut req = agent().get(&url).header("Authorization", &format!("Bearer {}", self.token)).header("User-Agent", "WindySync");
        if let Some(e) = &etag {
            req = req.header("If-None-Match", e);
        }
        let res = req.call().map_err(|e| e.to_string())?;
        match res.status().as_u16() {
            304 => Ok(self.cache.lock().unwrap().get(path).map(|c| c.1.clone())),
            404 => Ok(None),
            s if s >= 300 => Err(format!("GitHub {s} on {path}")),
            _ => {
                let tag = res.headers().get("etag").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
                let v: Value = res.into_body().read_json().map_err(|e| e.to_string())?;
                self.cache.lock().unwrap().insert(path.into(), (tag, v.clone()));
                Ok(Some(v))
            }
        }
    }

    /// A file's bytes and sha.
    pub fn get(&self, path: &str) -> Result<Option<(Vec<u8>, String)>, String> {
        if let Some(d) = &self.dir {
            return Ok(std::fs::read(d.join(path)).ok().map(|b| (b, String::new())));
        }
        let Some(v) = self.get_value(path)? else { return Ok(None) };
        let sha = v["sha"].as_str().unwrap_or_default().to_string();
        // Over 1 MB the contents API leaves `content` empty; the raw media type still gives the bytes.
        if v["encoding"].as_str() == Some("none") && v["size"].as_u64().unwrap_or(0) > 0 {
            let url = format!("{API}/repos/{}/contents/{path}", self.repo);
            let res = agent().get(&url).header("Authorization", &format!("Bearer {}", self.token)).header("User-Agent", "WindySync")
                .header("Accept", "application/vnd.github.raw+json").call().map_err(|e| e.to_string())?;
            if res.status().as_u16() >= 300 {
                return Err(format!("GitHub {} on {path}", res.status().as_u16()));
            }
            let bytes = res.into_body().with_config().limit(200 << 20).read_to_vec().map_err(|e| e.to_string())?;
            return Ok(Some((bytes, sha)));
        }
        let b64: String = v["content"].as_str().unwrap_or_default().chars().filter(|c| !c.is_whitespace()).collect();
        let bytes = B64.decode(b64).map_err(|e| format!("{path}: {e}"))?;
        Ok(Some((bytes, sha)))
    }

    pub fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<Option<(T, String)>, String> {
        match self.get(path)? {
            Some((b, sha)) => serde_json::from_slice(&b).map(|t| Some((t, sha))).map_err(|e| format!("{path}: {e}")),
            None => Ok(None),
        }
    }

    /// The names in a folder; empty when it doesn't exist yet.
    pub fn list(&self, dir: &str) -> Result<Vec<String>, String> {
        if let Some(d) = &self.dir {
            return Ok(std::fs::read_dir(d.join(dir)).map(|r| r.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default());
        }
        Ok(self.get_value(dir)?.and_then(|v| v.as_array().cloned()).unwrap_or_default().iter().filter_map(|e| e["name"].as_str().map(String::from)).collect())
    }

    /// Creates or replaces a file. `sha` is the version being replaced: None for a new file, or to look it up.
    pub fn put(&self, path: &str, bytes: &[u8], sha: Option<&str>, msg: &str) -> Result<String, String> {
        if let Some(d) = &self.dir {
            // Written aside and renamed, so a reader polling the folder never sees half a file.
            let (to, part) = (d.join(path), d.join(format!("{path}.part")));
            std::fs::create_dir_all(to.parent().unwrap()).and_then(|_| std::fs::write(&part, bytes)).and_then(|_| std::fs::rename(&part, &to)).map_err(|e| e.to_string())?;
            return Ok(String::new());
        }
        // Two devices committing to the branch at the same moment get a 409 for the slower one: try again.
        let mut tries = 0;
        loop {
            let sha = match sha {
                Some(s) => Some(s.to_string()),
                None => self.get(path)?.map(|(_, s)| s),
            };
            let mut body = json!({ "message": msg, "content": B64.encode(bytes) });
            if let Some(s) = sha {
                body["sha"] = json!(s);
            }
            let url = format!("{API}/repos/{}/contents/{path}", self.repo);
            let res = agent().put(&url).header("Authorization", &format!("Bearer {}", self.token)).header("User-Agent", "WindySync").send_json(body).map_err(|e| e.to_string())?;
            let status = res.status().as_u16();
            let v: Value = res.into_body().read_json().unwrap_or_default();
            self.cache.lock().unwrap().remove(path);
            if (status == 409 || status >= 500) && tries < 4 {
                tries += 1;
                std::thread::sleep(Duration::from_millis(700 * tries));
                continue;
            }
            if status >= 300 {
                return Err(format!("GitHub {status} writing {path}: {}", v["message"].as_str().unwrap_or("error")));
            }
            return Ok(v["content"]["sha"].as_str().unwrap_or_default().into());
        }
    }

    pub fn put_json<T: Serialize>(&self, path: &str, t: &T, sha: Option<&str>, msg: &str) -> Result<String, String> {
        self.put(path, &serde_json::to_vec_pretty(t).unwrap(), sha, msg)
    }
}

pub fn github_login(token: &str) -> Result<String, String> {
    let res = ureq::get("https://api.github.com/user").header("Authorization", &format!("Bearer {token}")).header("User-Agent", "WindySync").call().map_err(|e| format!("GitHub: {e}"))?;
    let v: serde_json::Value = res.into_body().read_json().map_err(|e| e.to_string())?;
    v["login"].as_str().map(String::from).ok_or("GitHub didn't say who this token belongs to".into())
}

/// The bus repo, made private on first use when the token's account owns the name.
pub fn ensure_repo(token: &str, repo: &str) -> Result<(), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let auth = format!("Bearer {token}");
    let res = agent.get(&format!("https://api.github.com/repos/{repo}")).header("Authorization", &auth).header("User-Agent", "WindySync").call().map_err(|e| e.to_string())?;
    match res.status().as_u16() {
        200 => Ok(()),
        404 => {
            let name = repo.rsplit('/').next().unwrap_or(repo);
            let body = serde_json::json!({ "name": name, "private": true, "auto_init": true, "description": "WindySync's device and transfer bus (written by the app)." });
            let r = agent.post("https://api.github.com/user/repos").header("Authorization", &auth).header("User-Agent", "WindySync").send_json(body).map_err(|e| e.to_string())?;
            if r.status().as_u16() < 300 { Ok(()) } else { Err(format!("Couldn't create {repo}: GitHub {}", r.status().as_u16())) }
        }
        s => Err(format!("GitHub {s} for {repo}")),
    }
}


// Documents ------------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// The system it runs (`Os::name`); the look is only how it is drawn, and the person may change it.
    pub os: String,
    pub look: Look,
    /// The X25519 public key, base64.
    pub key: String,
    pub chip: String,
    pub ram_gb: u64,
    pub disk_gb: u64,
    pub user: String,
    pub seen: u64,
    pub version: String,
}

impl Device {
    pub fn key_bytes(&self) -> Option<[u8; 32]> {
        B64.decode(&self.key).ok()?.try_into().ok()
    }
}

pub fn b64(b: &[u8]) -> String {
    B64.encode(b)
}

pub fn unb64(s: &str) -> Option<Vec<u8>> {
    B64.decode(s.trim()).ok()
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Ask {
    /// "I want to send you my things."
    Send,
    /// "I want to take your things."
    Receive,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Answer {
    #[default]
    Pending,
    Accepted,
    Declined,
    Cancelled,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Request {
    pub ask: Ask,
    pub from: String,
    pub to: String,
    pub at: u64,
    #[serde(default)]
    pub answer: Answer,
    /// The transfer folder once accepted.
    #[serde(default)]
    pub transfer: String,
}

impl Request {
    pub fn path(&self) -> String {
        format!("requests/{}_{}_{}.json", stamp(self.at), self.from, self.to)
    }
    /// (sender, receiver)
    pub fn sides(&self) -> (&str, &str) {
        match self.ask {
            Ask::Send => (&self.from, &self.to),
            Ask::Receive => (&self.to, &self.from),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Party {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Session {
    pub sender: Party,
    pub receiver: Party,
    /// offering, picking, sending, placing, setting-up, done, failed, cancelled
    pub state: String,
    pub started: u64,
    pub started_utc: String,
    pub updated: u64,
    pub bytes: u64,
    pub total: u64,
    pub avg_bytes_per_s: u64,
    pub files: u64,
    pub items_done: u64,
    pub items: u64,
    pub network: String,
    pub note: String,
}

pub fn transfer_dir(at: u64, sender: &str, receiver: &str) -> String {
    format!("transfers/{}_{sender}_{receiver}", stamp(at))
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `20261126-101500` in UTC: sorts by time and is safe in a path.
pub fn stamp(t: u64) -> String {
    let (y, m, d) = civil((t / 86400) as i64);
    let s = t % 86400;
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", s / 3600, s / 60 % 60, s % 60)
}

pub fn utc(t: u64) -> String {
    let s = stamp(t);
    format!("{}-{}-{}T{}:{}:{}Z", &s[0..4], &s[4..6], &s[6..8], &s[9..11], &s[11..13], &s[13..15])
}

/// Days since 1970-01-01 to (year, month, day), Howard Hinnant's algorithm.
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_are_utc_and_sortable() {
        assert_eq!(stamp(0), "19700101-000000");
        assert_eq!(stamp(1_795_688_100), "20261126-101500");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn request_sides_follow_the_ask() {
        let mut r = Request { ask: Ask::Send, from: "aaaa".into(), to: "bbbb".into(), at: 0, answer: Answer::Pending, transfer: String::new() };
        assert_eq!(r.sides(), ("aaaa", "bbbb"));
        r.ask = Ask::Receive;
        assert_eq!(r.sides(), ("bbbb", "aaaa"));
        assert_eq!(r.path(), "requests/19700101-000000_aaaa_bbbb.json");
    }
}

//! Cover art for non-Steam games: look the title up on the Steam store and cache its library art.
//! Only the game title is sent. Uses curl (no TLS stack in the binary).

use crate::library::{art_path, Art};
use crate::model::{squash, Game, RunnerKind};
use std::path::Path;
use std::process::Command;

fn curl_to(url: &str, dst: &Path) -> bool {
    if let Some(d) = dst.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = dst.with_extension("part");
    let ok = Command::new("curl")
        .args(["-fsL", "--max-time", "20", "-A", "hearth-game-launcher", "-o"])
        .arg(&tmp)
        .arg(url)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok && std::fs::metadata(&tmp).map(|m| m.len() > 1024).unwrap_or(false) {
        std::fs::rename(&tmp, dst).is_ok()
    } else {
        let _ = std::fs::remove_file(&tmp);
        false
    }
}

/// Best Steam app id for a title (exact-ish name match only, so we never show the wrong game).
pub fn steam_lookup(title: &str) -> Option<u32> {
    // Store search chokes on some punctuation ("Hollow Knight - Silksong"): retry with words only.
    let words: String = title.chars().map(|c| if c.is_alphanumeric() || c == '\'' { c } else { ' ' }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
    steam_search(title).or_else(|| (words != title).then(|| steam_search(&words)).flatten())
}

fn steam_search(title: &str) -> Option<u32> {
    let q: String = title.bytes().map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    let url = format!("https://store.steampowered.com/api/storesearch/?term={q}&l=english&cc=US");
    let out = Command::new("curl").args(["-fsSL", "--max-time", "15", "-A", "hearth-game-launcher", &url]).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let items = v.get("items")?.as_array()?;
    let want = squash(title);
    let mut best: Option<(i32, u32)> = None;
    for it in items {
        let name = it.get("name")?.as_str()?;
        let id = it.get("id")?.as_u64()? as u32;
        let n = squash(name);
        let score = if n == want {
            3
        } else if n.starts_with(&want) || want.starts_with(&n) {
            2
        } else if n.contains(&want) || want.contains(&n) {
            1
        } else {
            0
        };
        if score > 0 && best.map(|(s, _)| score > s).unwrap_or(true) {
            best = Some((score, id));
        }
    }
    best.map(|(_, id)| id)
}

/// Download missing art for one game. Returns true if anything new arrived.
pub fn fetch_for(g: &Game) -> bool {
    let need: Vec<Art> = [Art::Cover, Art::Hero, Art::Logo].into_iter().filter(|k| !art_path(&g.id, *k).is_file()).collect();
    if need.is_empty() {
        return false;
    }
    let appid = match g.steam_appid {
        Some(a) => Some(a),
        None if g.runner != RunnerKind::Steam => steam_lookup(&g.name),
        None => None,
    };
    let Some(appid) = appid else { return false };
    let base = format!("https://shared.steamstatic.com/store_item_assets/steam/apps/{appid}");
    let mut any = false;
    for kind in need {
        let files: &[&str] = match kind {
            Art::Cover => &["library_600x900_2x.jpg", "library_600x900.jpg"],
            Art::Hero => &["library_hero.jpg"],
            Art::Logo => &["logo.png"],
        };
        for f in files {
            if curl_to(&format!("{base}/{f}"), &art_path(&g.id, kind)) {
                any = true;
                break;
            }
        }
    }
    any
}

/// Fill in art for every game that lacks a cover. Returns ids that changed.
/// Each game is looked up once (marker file), so titles Steam doesn't know aren't retried every scan.
pub fn fetch_missing(games: &[Game]) -> Vec<String> {
    games
        .iter()
        .filter(|g| g.cover.is_none() && (!art_path(&g.id, Art::Cover).is_file() || !art_path(&g.id, Art::Hero).is_file()))
        .filter(|g| {
            let marker = art_path(&g.id, Art::Cover).with_file_name(".looked-up");
            if marker.exists() {
                return false;
            }
            let got = fetch_for(g);
            let _ = std::fs::create_dir_all(marker.parent().unwrap());
            let _ = std::fs::write(&marker, b"");
            got
        })
        .map(|g| g.id.clone())
        .collect()
}

/// Forget earlier lookups (after a rename).
pub fn reset_lookup(id: &str) {
    let _ = std::fs::remove_file(art_path(id, Art::Cover).with_file_name(".looked-up"));
}

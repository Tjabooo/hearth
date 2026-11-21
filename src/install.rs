//! "Install from .exe": run a Windows installer (FitGirl/DODI repacks are Inno Setup) in a fresh
//! prefix, point it at X:\Games\<name>, wait for it to finish, then add whatever it installed.
//!
//! Runs as a detached `hearth install …` job so closing the GUI doesn't kill a 40-minute unpack.
//! Progress goes to `~/.local/state/hearth/installs/<job>.json`.

use crate::config::{self, expand, write_atomic, Config};
use crate::launch::{self, TAG};
use crate::library::{self, Library};
use crate::model::{clean_title, now, squash, Game, LaunchOptions, RunnerKind, Source};
use crate::{runners, scan};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub setup: PathBuf,
    pub prefix: PathBuf,
    /// Windows path the installer was pointed at ("X:\Games\Name").
    pub target_win: String,
    pub target: PathBuf,
    /// "preparing" | "installing" | "finding" | "done" | "failed"
    pub phase: String,
    pub message: String,
    pub game_id: Option<String>,
    pub started: i64,
    pub pid: i32,
}

#[derive(Debug, Clone, Default)]
pub struct Request {
    pub setup: PathBuf,
    pub name: Option<String>,
    pub prefix: Option<PathBuf>,
    pub proton: Option<String>,
}

fn jobs_dir() -> PathBuf {
    config::state_dir().join("installs")
}

fn save(job: &Job) {
    let _ = write_atomic(&jobs_dir().join(format!("{}.json", job.id)), serde_json::to_string_pretty(job).unwrap_or_default().as_bytes());
}

/// All jobs, newest first. Jobs whose process died mid-way are marked failed.
pub fn jobs() -> Vec<Job> {
    let mut out: Vec<Job> = std::fs::read_dir(jobs_dir())
        .map(|rd| rd.flatten().filter_map(|e| serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok()).collect())
        .unwrap_or_default();
    for j in out.iter_mut() {
        if !matches!(j.phase.as_str(), "done" | "failed") && !library::pid_alive(j.pid) {
            j.phase = "failed".into();
            j.message = "The installer job stopped unexpectedly.".into();
            save(j);
        }
    }
    out.sort_by_key(|j| -j.started);
    out
}

pub fn clear_finished() {
    for j in jobs() {
        if matches!(j.phase.as_str(), "done" | "failed") {
            let _ = std::fs::remove_file(jobs_dir().join(format!("{}.json", j.id)));
        }
    }
}

/// Name a setup.exe would install: its folder ("Foo [FitGirl Repack]/setup.exe") or its own name.
pub fn guess_name(setup: &Path) -> String {
    let stem = setup.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let generic = ["setup", "install", "installer", "autorun"].iter().any(|g| stem.to_lowercase().starts_with(g));
    let raw = if generic { setup.parent().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or(stem) } else { stem };
    let t = clean_title(&raw);
    let t = t.trim_end_matches(" Setup").trim_end_matches(" setup").to_string();
    if t.is_empty() { "New game".into() } else { t }
}

/// Start the detached job; returns its id.
pub fn start(req: &Request) -> Result<String, String> {
    if !req.setup.is_file() {
        return Err(format!("{} not found", req.setup.display()));
    }
    let id = format!("{}-{}", now(), std::process::id());
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(me);
    cmd.arg("install").arg(&req.setup).arg("--job").arg(&id);
    if let Some(n) = &req.name {
        cmd.arg("--name").arg(n);
    }
    if let Some(p) = &req.prefix {
        cmd.arg("--prefix").arg(p);
    }
    if let Some(p) = &req.proton {
        cmd.arg("--proton").arg(p);
    }
    launch::detached(&mut cmd).map_err(|e| e.to_string())?;
    Ok(id)
}

fn safe_name(n: &str) -> String {
    n.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { ' ' } else { c }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Windows path for a Linux folder via the install drive (X: -> ~), plus the drive mapping to create.
fn windows_path(cfg: &Config, target: &Path) -> (String, char, PathBuf) {
    let letter = cfg.install_drive.chars().next().unwrap_or('x').to_ascii_lowercase();
    let drive_target = expand(&cfg.install_drive_target);
    let root = match target.strip_prefix(&drive_target) {
        Ok(_) => drive_target.clone(),
        Err(_) => target.parent().unwrap_or(target).to_path_buf(),
    };
    let rel = target.strip_prefix(&root).unwrap_or(target);
    let win = format!("{}:\\{}", letter.to_ascii_uppercase(), rel.to_string_lossy().replace('/', "\\"));
    (win, letter, root)
}

#[derive(PartialEq)]
enum Kind {
    Inno,
    Nsis,
    Other,
}

fn installer_kind(setup: &Path) -> Kind {
    use std::io::Read;
    let mut buf = vec![0u8; 8 << 20];
    let n = std::fs::File::open(setup).and_then(|mut f| f.read(&mut buf)).unwrap_or(0);
    let b = &buf[..n];
    let has = |s: &[u8]| b.windows(s.len()).any(|w| w == s);
    if has(b"Inno Setup") {
        Kind::Inno
    } else if has(b"Nullsoft") || has(b"NullsoftInst") {
        Kind::Nsis
    } else {
        Kind::Other
    }
}

/// The job itself (`hearth install SETUP --job ID …`).
pub fn run(req: Request, job_id: String) -> Result<(), String> {
    let cfg = Config::load();
    let name = req.name.clone().unwrap_or_else(|| guess_name(&req.setup));
    let folder = safe_name(&name);
    let prefix = req.prefix.clone().unwrap_or_else(|| expand(&cfg.prefix_root).join(&folder));
    let install_root = expand(&cfg.install_root);
    let target = install_root.join(&folder);
    let (target_win, letter, drive_root) = windows_path(&cfg, &target);
    let mut job = Job {
        id: job_id,
        name: name.clone(),
        setup: req.setup.clone(),
        prefix: prefix.clone(),
        target_win: target_win.clone(),
        target: target.clone(),
        phase: "preparing".into(),
        message: "Setting up a Windows prefix…".into(),
        started: now(),
        pid: std::process::id() as i32,
        ..Default::default()
    };
    save(&job);
    let res = run_job(&cfg, &req, &mut job, letter, &drive_root, &install_root);
    match &res {
        Ok(()) => {
            job.phase = "done".into();
            launch::notify(&format!("{name} is installed"), "It's in your library now.");
        }
        Err(e) => {
            job.phase = "failed".into();
            job.message = e.clone();
            launch::notify(&format!("Installing {name} failed"), e);
        }
    }
    save(&job);
    res
}

fn run_job(cfg: &Config, req: &Request, job: &mut Job, letter: char, drive_root: &Path, install_root: &Path) -> Result<(), String> {
    let proton = runners::find_proton(req.proton.as_deref().unwrap_or("latest")).ok_or("No Proton build found. Install GE-Proton first.")?;
    let umu = runners::umu();
    std::fs::create_dir_all(&job.prefix).map_err(|e| format!("Can't create {}: {e}", job.prefix.display()))?;
    std::fs::create_dir_all(install_root).ok();
    let tag = format!("install-{}", job.id);

    let mut env: BTreeMap<String, String> = BTreeMap::new();
    env.insert(TAG.into(), tag.clone());
    let base_argv: Vec<String> = match &umu {
        Some(u) => {
            env.insert("WINEPREFIX".into(), job.prefix.to_string_lossy().into());
            env.insert("PROTONPATH".into(), proton.path.to_string_lossy().into());
            env.insert("GAMEID".into(), "umu-default".into());
            env.insert("STORE".into(), "none".into());
            vec![u.to_string_lossy().into()]
        }
        None => {
            env.insert("STEAM_COMPAT_DATA_PATH".into(), job.prefix.to_string_lossy().into());
            let steam = scan::steam_root(cfg).unwrap_or_else(|| config::home().join(".local/share/Steam"));
            env.insert("STEAM_COMPAT_CLIENT_INSTALL_PATH".into(), steam.to_string_lossy().into());
            vec![proton.path.join("proton").to_string_lossy().into(), "waitforexitandrun".into()]
        }
    };
    // Installers spawn unpackers; FitGirl's can take a lot of RAM, nothing to tune here.
    let log = config::log_dir().join(format!("install-{}.log", crate::model::slugify(&job.name)));
    let _ = std::fs::create_dir_all(config::log_dir());

    // 1. Make sure the prefix exists (umu: empty exe = create prefix and exit).
    let wine_dir = if umu.is_some() { job.prefix.clone() } else { job.prefix.join("pfx") };
    if !wine_dir.join("system.reg").is_file() {
        let mut argv = base_argv.clone();
        argv.push(if umu.is_some() { String::new() } else { "wineboot".into() });
        run_wait(&argv, &env, &log, &tag, Duration::from_secs(600))?;
    }
    // 2. A drive letter that reaches the games folder. Wine's mount manager assigns some letters to
    //    mount points itself (often X: -> ~) and resets them on every start, so use one of those
    //    when it covers the target, else add our own free letter.
    let dd = wine_dir.join("dosdevices");
    std::fs::create_dir_all(&dd).ok();
    let (letter, root) = pick_drive(&dd, &job.target, letter, drive_root)?;
    let rel = job.target.strip_prefix(&root).unwrap_or(&job.target);
    job.target_win = format!("{}:\\{}", letter.to_ascii_uppercase(), rel.to_string_lossy().replace('/', "\\"));

    // 3. Snapshot, then run the installer pointed at X:\Games\<name>.
    let before_root = list_dirs(install_root);
    let before_pfx: HashSet<PathBuf> = pfx_candidates(&wine_dir).into_iter().collect();
    let mut argv = base_argv.clone();
    argv.push(job.setup.to_string_lossy().into());
    match installer_kind(&job.setup) {
        Kind::Inno => argv.push(format!("/DIR={}", job.target_win)),
        Kind::Nsis => argv.push(format!("/D={}", job.target_win)),
        Kind::Other => {}
    }
    job.phase = "installing".into();
    job.message = format!("Installer is open. Suggested folder: {}", job.target_win);
    save(job);
    let t0 = Instant::now();
    run_wait(&argv, &env, &log, &tag, Duration::from_secs(60 * 60 * 6))?;

    // 4. Find what it installed.
    job.phase = "finding".into();
    job.message = "Looking for the installed game…".into();
    save(job);
    let after_root = list_dirs(install_root);
    let mut fresh: Vec<PathBuf> = after_root.difference(&before_root).cloned().collect();
    if fresh.is_empty() && job.target.is_dir() {
        fresh.push(job.target.clone()); // reinstall over an existing folder
    }
    let fresh_pfx: Vec<PathBuf> = pfx_candidates(&wine_dir).into_iter().filter(|d| !before_pfx.contains(d)).collect();
    // Prefer the folder named like the game.
    let want = squash(&job.name);
    let score = |d: &PathBuf| -> i64 {
        let n = squash(&clean_title(&d.file_name().unwrap_or_default().to_string_lossy()));
        (n == want) as i64 * 10 + (n.contains(&want) || want.contains(&n)) as i64 * 5
    };
    let mut cands: Vec<PathBuf> = fresh.into_iter().chain(fresh_pfx).collect();
    cands.sort_by_key(|d| -score(d));
    let mut found: Option<(PathBuf, PathBuf)> = None;
    for d in &cands {
        let n = d.file_name().unwrap_or_default().to_string_lossy().to_string();
        if let Some(exe) = scan::pick_exe(d, &n).or_else(|| scan::pick_exe(d, &job.name)) {
            found = Some((d.clone(), exe));
            break;
        }
    }
    let Some((dir, exe)) = found else {
        if t0.elapsed() < Duration::from_secs(20) {
            return Err("The installer closed before installing anything.".into());
        }
        return Err(format!("Couldn't find the installed game. Use Add game to pick its .exe (prefix: {}).", config::tilde(&job.prefix)));
    };

    let mut lib = Library::load();
    let existing = lib.games.iter().find(|g| g.exe.as_deref() == Some(exe.as_path()) || g.install_dir.as_deref() == Some(dir.as_path())).cloned();
    let mut g = existing.unwrap_or_else(|| Game { id: lib.new_id(&job.name), added: now(), ..Default::default() });
    g.name = job.name.clone();
    g.source = Source::Installer;
    g.runner = RunnerKind::Proton;
    g.exe = Some(exe);
    g.install_dir = Some(dir);
    g.prefix = Some(job.prefix.clone());
    g.options = LaunchOptions { proton: req.proton.clone().or(Some(proton.name.clone())), ..g.options };
    lib.upsert(g.clone()).map_err(|e| e.to_string())?;
    job.game_id = Some(g.id.clone());
    job.message = format!("Added {}", g.name);
    if cfg.fetch_art {
        crate::art::fetch_for(&g);
    }
    Ok(())
}

/// (letter, folder it points at) for installing into `target`.
fn pick_drive(dd: &Path, target: &Path, want: char, want_root: &Path) -> Result<(char, PathBuf), String> {
    let mut best: Option<(char, PathBuf)> = None;
    for l in 'a'..='y' {
        if l == 'c' {
            continue;
        }
        let Ok(t) = std::fs::read_link(dd.join(format!("{l}:"))) else { continue };
        let t = if t.is_absolute() { t } else { dd.join(t) };
        let t = std::fs::canonicalize(&t).unwrap_or(t);
        if target.starts_with(&t) && best.as_ref().map(|(_, b)| t.components().count() > b.components().count()).unwrap_or(true) {
            best = Some((l, t));
        }
    }
    if let Some(b) = best {
        return Ok(b);
    }
    let taken = |l: char| std::fs::symlink_metadata(dd.join(format!("{l}:"))).is_ok();
    let letter = if !taken(want) { want } else { ('g'..='y').find(|l| !taken(*l)).ok_or("No free drive letter in the prefix")? };
    let root = if target.starts_with(want_root) { want_root.to_path_buf() } else { target.parent().unwrap_or(target).to_path_buf() };
    std::os::unix::fs::symlink(&root, dd.join(format!("{letter}:"))).map_err(|e| format!("Can't map {letter}: → {}: {e}", root.display()))?;
    Ok((letter, root))
}

fn list_dirs(d: &Path) -> HashSet<PathBuf> {
    std::fs::read_dir(d).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default()
}

/// Folders an installer might create inside the prefix.
fn pfx_candidates(wine_dir: &Path) -> Vec<PathBuf> {
    let c = wine_dir.join("drive_c");
    let mut out = vec![];
    for sub in ["", "Games", "Program Files", "Program Files (x86)"] {
        out.extend(list_dirs(&c.join(sub)));
    }
    if let Ok(rd) = std::fs::read_dir(c.join("users")) {
        for u in rd.flatten() {
            out.extend(list_dirs(&u.path()));
        }
    }
    out
}

/// Run and wait until the process AND everything it spawned (tagged) has exited.
fn run_wait(argv: &[String], env: &BTreeMap<String, String>, log: &Path, tag: &str, limit: Duration) -> Result<(), String> {
    let f = std::fs::OpenOptions::new().create(true).append(true).open(log).map_err(|e| e.to_string())?;
    let f2 = f.try_clone().map_err(|e| e.to_string())?;
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .envs(env)
        .stdin(Stdio::null())
        .stdout(f)
        .stderr(f2)
        .spawn()
        .map_err(|e| format!("Couldn't start {}: {e}", argv[0]))?;
    let t0 = Instant::now();
    let mut done = false;
    let mut quiet: Option<Instant> = None;
    loop {
        if !done && matches!(child.try_wait(), Ok(Some(_))) {
            done = true;
        }
        let busy = !done || !launch::game_pids(tag).is_empty();
        if busy {
            quiet = None;
        } else if quiet.get_or_insert_with(Instant::now).elapsed() > Duration::from_secs(5) {
            return Ok(());
        }
        if t0.elapsed() > limit {
            return Err("The installer ran for too long and was left alone.".into());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names() {
        assert_eq!(guess_name(Path::new("/d/The Genesis Order [FitGirl Repack]/setup.exe")), "The Genesis Order");
        assert_eq!(guess_name(Path::new("/d/Cool_Game_Setup.exe")), "Cool Game");
    }
    #[test]
    fn winpath() {
        let mut cfg = Config::default();
        cfg.install_drive_target = "/home/u".into();
        let (w, l, r) = windows_path(&cfg, Path::new("/home/u/Games/Foo Bar"));
        assert_eq!(w, "X:\\Games\\Foo Bar");
        assert_eq!(l, 'x');
        assert_eq!(r, PathBuf::from("/home/u"));
    }
}

//! Building the launch command and supervising a running game.
//!
//! The GUI never runs games itself: it starts `hearth run <id>` detached. That supervisor runs the
//! pre-launch script, starts the game in its own session, tracks every process that carries
//! `HEARTH_GAME=<id>` (so launchers that fork and exit are followed), counts play time, then runs the
//! post-exit script. Closing the GUI never touches a running game.

use crate::config::{self, Config, write_atomic};
use crate::library::{self, load_stats, save_stats};
use crate::model::{Game, LaunchOptions, RunnerKind, RunningInfo, now};
use crate::runners;
use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const TAG: &str = "HEARTH_GAME";

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub workdir: Option<PathBuf>,
    pub opts: LaunchOptions,
    /// wineserver for `kill_prefix_on_exit` / Stop.
    pub wineserver: Option<PathBuf>,
    pub prefix: Option<PathBuf>,
}

impl Plan {
    /// Shell-ish rendering for "Show command" and --dry-run.
    pub fn render(&self) -> String {
        let mut s = String::new();
        for (k, v) in &self.env {
            if k != TAG {
                s.push_str(&format!("{k}={} ", quote(v)));
            }
        }
        s.push_str(
            &self
                .argv
                .iter()
                .map(|a| quote(a))
                .collect::<Vec<_>>()
                .join(" "),
        );
        s
    }
}

fn quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || "-_./=:,+@%".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Split an argument string like a shell would (quotes, no expansion).
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut q: Option<char> = None;
    let mut any = false;
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match (q, c) {
            (None, '"') | (None, '\'') => {
                q = Some(c);
                any = true;
            }
            (Some(x), c) if c == x => q = None,
            (Some('"'), '\\') | (None, '\\') => {
                if let Some(n) = it.next() {
                    cur.push(n);
                }
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (_, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Default prefix for a Windows game without one: `<prefix_root>/<name>`.
pub fn default_prefix(cfg: &Config, g: &Game) -> PathBuf {
    let name: String = g
        .name
        .chars()
        .map(|c| if c == '/' { '-' } else { c })
        .collect();
    config::expand(&cfg.prefix_root).join(name.trim())
}

pub fn plan(g: &Game, cfg: &Config) -> Result<Plan, String> {
    let o = g.options.over(&cfg.defaults);
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    env.insert(TAG.into(), g.id.clone());
    let mut argv: Vec<String>;
    let mut workdir = g.workdir.clone();
    let mut wineserver = None;
    let mut prefix = None;
    let args = split_args(&g.args);

    match g.runner {
        RunnerKind::Steam => {
            let appid = g.steam_appid.ok_or("This Steam game has no app id")?;
            argv = cfg
                .steam_command
                .iter()
                .map(|a| {
                    config::expand(&a.replace("{appid}", &appid.to_string()))
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            if argv.is_empty() {
                return Err("steam_command is empty in config.toml".into());
            }
            // Steam owns these games' settings (launch options, Proton version).
            return Ok(Plan {
                argv,
                env,
                workdir: None,
                opts: o,
                wineserver: None,
                prefix: None,
            });
        }
        RunnerKind::Command => {
            if g.command.is_empty() {
                return Err("No command set".into());
            }
            argv = g.command.clone();
            argv.extend(args);
        }
        RunnerKind::Native => {
            let exe = g.exe.clone().ok_or("No executable set")?;
            if !exe.exists() {
                return Err(format!("{} is missing", exe.display()));
            }
            if workdir.is_none() {
                workdir = exe.parent().map(Path::to_path_buf);
            }
            argv = vec![exe.to_string_lossy().into()];
            argv.extend(args);
        }
        RunnerKind::Proton | RunnerKind::Wine => {
            let exe = g.exe.clone().ok_or("No .exe set")?;
            if !exe.exists() {
                return Err(format!("{} is missing", exe.display()));
            }
            if workdir.is_none() {
                workdir = exe.parent().map(Path::to_path_buf);
            }
            let pfx = g.prefix.clone().unwrap_or_else(|| default_prefix(cfg, g));
            env.insert("WINEPREFIX".into(), pfx.to_string_lossy().into());
            prefix = Some(pfx.clone());
            if g.runner == RunnerKind::Proton {
                let want = o.proton.clone().unwrap_or_else(|| "latest".into());
                let build = runners::find_proton(&want)
                    .ok_or_else(|| format!("Proton build \"{want}\" not found"))?;
                wineserver = Some(build.path.join("files/bin/wineserver"));
                if let Some(umu) = runners::umu() {
                    env.insert("PROTONPATH".into(), build.path.to_string_lossy().into());
                    let gameid = g
                        .steam_appid
                        .map(|a| format!("umu-{a}"))
                        .unwrap_or_else(|| "umu-default".into());
                    env.insert("GAMEID".into(), gameid);
                    env.insert("STORE".into(), "none".into());
                    argv = vec![umu.to_string_lossy().into(), exe.to_string_lossy().into()];
                } else {
                    // Plain Proton: it wants a compat-data folder and uses <it>/pfx as the prefix.
                    env.remove("WINEPREFIX");
                    env.insert(
                        "STEAM_COMPAT_DATA_PATH".into(),
                        pfx.to_string_lossy().into(),
                    );
                    let steam = crate::scan::steam_root(cfg)
                        .unwrap_or_else(|| config::home().join(".local/share/Steam"));
                    env.insert(
                        "STEAM_COMPAT_CLIENT_INSTALL_PATH".into(),
                        steam.to_string_lossy().into(),
                    );
                    argv = vec![
                        build.path.join("proton").to_string_lossy().into(),
                        "waitforexitandrun".into(),
                        exe.to_string_lossy().into(),
                    ];
                }
                if let Some(v) = o.fsr4 {
                    env.insert(
                        "PROTON_FSR4_UPGRADE".into(),
                        if v { "1" } else { "0" }.into(),
                    );
                }
                if o.wine_wayland == Some(true) {
                    env.insert("PROTON_ENABLE_WAYLAND".into(), "1".into());
                }
                if o.wined3d == Some(true) {
                    env.insert("PROTON_USE_WINED3D".into(), "1".into());
                }
                match o.sync.as_deref() {
                    Some("ntsync") => {
                        env.insert("PROTON_USE_NTSYNC".into(), "1".into());
                    }
                    Some("fsync") => {
                        env.insert("PROTON_NO_NTSYNC".into(), "1".into());
                    }
                    Some("esync") => {
                        env.insert("PROTON_NO_NTSYNC".into(), "1".into());
                        env.insert("PROTON_NO_FSYNC".into(), "1".into());
                    }
                    Some("off") => {
                        for k in ["PROTON_NO_NTSYNC", "PROTON_NO_FSYNC", "PROTON_NO_ESYNC"] {
                            env.insert(k.into(), "1".into());
                        }
                    }
                    _ => {}
                }
            } else {
                let want = o.wine.clone().unwrap_or_else(|| "system".into());
                let wine = runners::find_wine(&want)
                    .ok_or_else(|| format!("Wine build \"{want}\" not found"))?;
                wineserver = wine.parent().map(|b| b.join("wineserver"));
                argv = vec![wine.to_string_lossy().into(), exe.to_string_lossy().into()];
                match o.sync.as_deref() {
                    Some("esync") => {
                        env.insert("WINEESYNC".into(), "1".into());
                    }
                    Some("fsync") => {
                        env.insert("WINEFSYNC".into(), "1".into());
                    }
                    _ => {}
                }
            }
            if let Some(h) = o.dxvk_hud.as_ref().filter(|h| !h.is_empty()) {
                env.insert("DXVK_HUD".into(), h.clone());
            }
            argv.extend(args);
        }
    }

    if o.mangohud == Some(true) {
        env.extend(cfg.mangohud_env.clone());
        if let Some(c) = o.mangohud_config.as_ref().filter(|c| !c.trim().is_empty()) {
            env.insert("MANGOHUD_CONFIG".into(), format!("read_cfg,{}", c.trim()));
        }
    }
    // User env last: it wins over everything above.
    env.extend(o.env.clone());

    // Wrappers, innermost first.
    if o.gamescope == Some(true) {
        if runners::which("gamescope").is_none() {
            return Err("gamescope is not installed".into());
        }
        let gs = o.gamescope_opts.clone().unwrap_or_default();
        let mut w = vec!["gamescope".to_string()];
        let mut push = |a: &str, v: u32| {
            if v > 0 {
                w.push(a.into());
                w.push(v.to_string());
            }
        };
        push("-w", gs.width);
        push("-h", gs.height);
        push("-W", gs.out_width);
        push("-H", gs.out_height);
        push("-r", gs.refresh);
        if gs.fullscreen {
            w.push("-f".into());
        }
        if !gs.filter.is_empty() {
            w.push("-F".into());
            w.push(gs.filter.clone());
        }
        if gs.grab_cursor {
            w.push("--force-grab-cursor".into());
        }
        if gs.hdr {
            w.push("--hdr-enabled".into());
        }
        w.extend(split_args(&gs.extra));
        w.push("--".into());
        w.append(&mut argv);
        argv = w;
    }
    if o.gamemode == Some(true) && runners::which("gamemoderun").is_some() {
        argv.insert(0, "gamemoderun".into());
    }
    if let Some(cpus) = o.cpu_set.as_ref().filter(|c| !c.trim().is_empty()) {
        argv.splice(
            0..0,
            ["taskset".into(), "-c".into(), cpus.trim().to_string()],
        );
    }
    if o.use_wrapper == Some(true) && !cfg.wrapper.is_empty() {
        argv.splice(
            0..0,
            cfg.wrapper
                .iter()
                .map(|a| config::expand(a).to_string_lossy().to_string()),
        );
    }
    Ok(Plan {
        argv,
        env,
        workdir,
        opts: o,
        wineserver,
        prefix,
    })
}

// ---- starting from the GUI / CLI ------------------------------------------------------------

/// Start `hearth run <id>` detached. Returns once it's spawned.
pub fn spawn_supervisor(id: &str) -> std::io::Result<()> {
    let me = std::env::current_exe()?;
    detached(Command::new(me).arg("run").arg(id))
}

pub fn detached(cmd: &mut Command) -> std::io::Result<()> {
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    // Reap it in the background so it never lingers as a zombie of the GUI.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn running_path(id: &str) -> PathBuf {
    config::running_dir().join(format!("{id}.json"))
}

fn write_running(id: &str, started: i64, phase: &str) {
    let info = RunningInfo {
        id: id.into(),
        supervisor_pid: std::process::id() as i32,
        started,
        phase: phase.into(),
    };
    let _ = write_atomic(
        &running_path(id),
        serde_json::to_string(&info).unwrap_or_default().as_bytes(),
    );
}

/// The supervisor (`hearth run <id>`). Blocks until the game is gone.
pub fn run(id: &str) -> Result<(), String> {
    let cfg = Config::load();
    let lib = library::Library::load();
    let g = lib
        .get(id)
        .cloned()
        .ok_or_else(|| format!("No game with id {id}"))?;
    if library::running(id).is_some() {
        return Err(format!("{} is already running", g.name));
    }
    let p = plan(&g, &cfg)?;
    let started = now();
    write_running(id, started, "starting");
    let result = supervise(&g, &p, started);
    let _ = std::fs::remove_file(running_path(id));
    if let Err(e) = &result {
        notify(&g.name, e);
    }
    result
}

fn shell(cmd: &str, env: &BTreeMap<String, String>, wait: bool) {
    let mut c = Command::new("sh");
    c.arg("-c")
        .arg(cmd)
        .envs(env.iter().filter(|(k, _)| k.as_str() != TAG));
    if wait {
        let _ = c.status();
    } else {
        let _ = detached(&mut c);
    }
}

fn supervise(g: &Game, p: &Plan, started: i64) -> Result<(), String> {
    let log_on = p.opts.log.unwrap_or(true);
    let (out, err) = if log_on {
        let _ = std::fs::create_dir_all(config::log_dir());
        let path = config::log_dir().join(format!("{}.log", g.id));
        let f = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        use std::io::Write;
        let mut f2 = f.try_clone().map_err(|e| e.to_string())?;
        let _ = writeln!(f2, "# {} — {}\n# {}\n", g.name, started, p.render());
        (Stdio::from(f), Stdio::from(f2))
    } else {
        (Stdio::null(), Stdio::null())
    };

    if let Some(pre) = p.opts.prelaunch.as_ref().filter(|s| !s.trim().is_empty()) {
        shell(pre, &p.env, p.opts.prelaunch_wait.unwrap_or(true));
    }

    let mut cmd = Command::new(&p.argv[0]);
    cmd.args(&p.argv[1..])
        .envs(&p.env)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    if let Some(w) = &p.workdir
        && w.is_dir()
    {
        cmd.current_dir(w);
    }
    let nice = p.opts.nice.unwrap_or(0);
    unsafe {
        cmd.pre_exec(move || {
            libc::setsid();
            if nice != 0 {
                libc::setpriority(libc::PRIO_PROCESS, 0, nice);
            }
            Ok(())
        });
    }
    let t0 = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Couldn't start {}: {e}", p.argv[0]))?;
    write_running(&g.id, started, "running");

    let steam = g.runner == RunnerKind::Steam;
    let mut stats = load_stats(&g.id);
    stats.launches += 1;
    stats.last_played = started;
    let _ = save_stats(&g.id, &stats);

    let base = stats.playtime_secs;
    let mut last_flush = Instant::now();
    let mut child_done = false;
    let mut seen_steam_game = false;
    let mut idle_since: Option<Instant> = None;
    let ended: Instant = loop {
        if !child_done && let Ok(Some(_)) = child.try_wait() {
            child_done = true;
        }
        let alive = if steam {
            // steam://rungameid hands off to the Steam client; follow Steam's game reaper instead.
            let up = steam_game_running(g.steam_appid.unwrap_or(0));
            if up {
                seen_steam_game = true;
            }
            up || (!seen_steam_game && t0.elapsed() < Duration::from_secs(90))
        } else {
            !child_done || !game_pids(&g.id).is_empty()
        };
        if !alive {
            // Launchers sometimes hand over with a gap: give it a few seconds before calling it.
            let since = *idle_since.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_secs(if steam { 1 } else { 4 }) {
                break since;
            }
        } else {
            idle_since = None;
        }
        if !steam && last_flush.elapsed() > Duration::from_secs(60) {
            stats.playtime_secs = base + t0.elapsed().as_secs();
            let _ = save_stats(&g.id, &stats);
            last_flush = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(if child_done { 2000 } else { 500 }));
    };
    let _ = child.try_wait();
    if !steam {
        stats.playtime_secs = base + ended.duration_since(t0).as_secs();
        let _ = save_stats(&g.id, &stats);
    }

    if let Some(post) = p.opts.postexit.as_ref().filter(|s| !s.trim().is_empty()) {
        shell(post, &p.env, true);
    }
    if p.opts.kill_prefix_on_exit == Some(true) {
        kill_prefix(p);
    }
    // A game that dies within seconds probably didn't start: say so.
    if !steam
        && t0.elapsed() < Duration::from_secs(5)
        && let Ok(Some(st)) = child.try_wait()
        && !st.success()
    {
        return Err(format!("Exited right away ({st}). See the launch log."));
    }
    Ok(())
}

fn kill_prefix(p: &Plan) {
    if let (Some(ws), Some(pfx)) = (&p.wineserver, &p.prefix)
        && ws.is_file()
    {
        let _ = Command::new(ws).arg("-k").env("WINEPREFIX", pfx).status();
    }
}

/// Wine/Proton plumbing that outlives games for a few seconds; ignored when deciding "still running".
const INFRA: &[&str] = &[
    "wineserver",
    "services.exe",
    "winedevice.exe",
    "plugplay.exe",
    "explorer.exe",
    "rpcss.exe",
    "svchost.exe",
    "tabtip.exe",
    "conhost.exe",
    "rundll32.exe",
    "start.exe",
    "winedbg",
    "wineboot.exe",
    "mscorsvw.exe",
    "steam.exe",
    "pv-adverb",
    "srt-bwrap",
    "reaper",
    "steam-runtime-l",
    "xdg-dbus-proxy",
];

/// PIDs of processes started for this game (tagged env), plumbing excluded.
pub fn game_pids(id: &str) -> Vec<i32> {
    tagged_pids(id)
        .into_iter()
        .filter(|(_, comm)| !INFRA.iter().any(|i| comm.eq_ignore_ascii_case(i)))
        .map(|(p, _)| p)
        .collect()
}

fn tagged_pids(id: &str) -> Vec<(i32, String)> {
    let needle = format!("{TAG}={id}\0");
    let me = std::process::id() as i32;
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return out;
    };
    for e in rd.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Ok(env) = std::fs::read(e.path().join("environ")) else {
            continue;
        };
        if !contains(&env, needle.as_bytes()) {
            continue;
        }
        // Zombies keep their environ but are already gone.
        let stat = std::fs::read_to_string(e.path().join("stat")).unwrap_or_default();
        if stat
            .rsplit(')')
            .next()
            .map(|r| r.trim_start().starts_with('Z'))
            .unwrap_or(false)
        {
            continue;
        }
        let comm = std::fs::read_to_string(e.path().join("comm"))
            .unwrap_or_default()
            .trim()
            .to_string();
        out.push((pid, comm));
    }
    out
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    // environ starts at a var boundary; match at 0 or right after a NUL.
    hay.windows(needle.len())
        .enumerate()
        .any(|(i, w)| w == needle && (i == 0 || hay[i - 1] == 0))
}

fn steam_game_running(appid: u32) -> bool {
    if appid == 0 {
        return false;
    }
    let needle = format!("AppId={appid}");
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return false;
    };
    rd.flatten().any(|e| {
        e.file_name().to_string_lossy().parse::<i32>().is_ok()
            && std::fs::read(e.path().join("cmdline"))
                .map(|c| {
                    c.split(|b| *b == 0).any(|a| a == needle.as_bytes())
                        && c.windows(10).any(|w| w == b"SteamLaunc")
                })
                .unwrap_or(false)
    })
}

/// Stop a running game: TERM everything tagged, KILL after a grace period, stop its prefix.
pub fn stop(id: &str) -> Result<(), String> {
    let cfg = Config::load();
    let lib = library::Library::load();
    let g = lib.get(id).cloned().ok_or("Unknown game")?;
    if g.runner == RunnerKind::Steam {
        // Steam games: signal the reaper's children.
        let appid = g.steam_appid.unwrap_or(0);
        for pid in steam_reapers(appid) {
            unsafe { libc::kill(-pid, libc::SIGTERM) };
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
        return Ok(());
    }
    let pids: Vec<i32> = tagged_pids(id).into_iter().map(|(p, _)| p).collect();
    for &p in &pids {
        unsafe { libc::kill(p, libc::SIGTERM) };
    }
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(5) && !game_pids(id).is_empty() {
        std::thread::sleep(Duration::from_millis(200));
    }
    for (p, _) in tagged_pids(id) {
        unsafe { libc::kill(p, libc::SIGKILL) };
    }
    if g.runner.is_windows()
        && let Ok(p) = plan(&g, &cfg)
    {
        kill_prefix(&p);
    }
    Ok(())
}

fn steam_reapers(appid: u32) -> Vec<i32> {
    let needle = format!("AppId={appid}");
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return vec![];
    };
    rd.flatten()
        .filter_map(|e| {
            let pid = e.file_name().to_string_lossy().parse::<i32>().ok()?;
            let c = std::fs::read(e.path().join("cmdline")).ok()?;
            let first = c.split(|b| *b == 0).next()?;
            (first.ends_with(b"reaper") && c.split(|b| *b == 0).any(|a| a == needle.as_bytes()))
                .then_some(pid)
        })
        .collect()
}

pub fn notify(title: &str, body: &str) {
    let _ = Command::new("notify-send")
        .args(["-a", "Hearth", "-i", "applications-games", title, body])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Run an arbitrary .exe (patch, trainer, config tool) inside a game's prefix with its Proton/Wine.
pub fn run_in_prefix(g: &Game, cfg: &Config, exe: &Path) -> Result<(), String> {
    let mut tmp = g.clone();
    tmp.exe = Some(exe.to_path_buf());
    tmp.args = String::new();
    tmp.workdir = exe.parent().map(Path::to_path_buf);
    tmp.id = format!("{}--tool", g.id);
    // No gamescope/wrapper/gamemode for tools.
    tmp.options.gamescope = Some(false);
    tmp.options.gamemode = Some(false);
    tmp.options.mangohud = Some(false);
    let p = plan(&tmp, cfg)?;
    let mut cmd = Command::new(&p.argv[0]);
    cmd.args(&p.argv[1..]).envs(&p.env);
    if let Some(w) = &p.workdir {
        cmd.current_dir(w);
    }
    detached(&mut cmd).map_err(|e| e.to_string())
}

/// winecfg / regedit / winetricks / control in a game's prefix.
pub fn prefix_tool(g: &Game, cfg: &Config, tool: &str) -> Result<(), String> {
    if !g.runner.is_windows() {
        return Err("Not a Windows game".into());
    }
    if tool == "winetricks" {
        let p = plan(g, cfg)?;
        let mut cmd = if runners::umu().is_some() && g.runner == RunnerKind::Proton {
            let mut c = Command::new(runners::umu().unwrap());
            c.arg("winetricks").arg("--gui");
            c
        } else {
            let mut c = Command::new("winetricks");
            c.arg("--gui");
            c
        };
        cmd.envs(p.env.iter().filter(|(k, _)| k.as_str() != TAG));
        return detached(&mut cmd).map_err(|e| e.to_string());
    }
    let p = plan(g, cfg)?;
    let pfx = p.prefix.clone().unwrap_or_default();
    let mut env: BTreeMap<String, String> = p.env.into_iter().filter(|(k, _)| k != TAG).collect();
    let argv: Vec<String> = if g.runner == RunnerKind::Proton {
        match runners::umu() {
            Some(u) => vec![u.to_string_lossy().into(), tool.into()],
            None => {
                let wine = p
                    .wineserver
                    .as_ref()
                    .and_then(|w| w.parent())
                    .map(|b| b.join("wine"))
                    .ok_or("No wine")?;
                env.insert(
                    "WINEPREFIX".into(),
                    pfx.join("pfx").to_string_lossy().into(),
                );
                vec![wine.to_string_lossy().into(), tool.into()]
            }
        }
    } else {
        let want = g
            .options
            .over(&cfg.defaults)
            .wine
            .unwrap_or_else(|| "system".into());
        vec![
            runners::find_wine(&want)
                .ok_or("No wine")?
                .to_string_lossy()
                .into(),
            tool.into(),
        ]
    };
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).envs(env);
    detached(&mut cmd).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn args() {
        assert_eq!(
            split_args(r#"-a "b c" 'd e' f\ g"#),
            vec!["-a", "b c", "d e", "f g"]
        );
    }
}

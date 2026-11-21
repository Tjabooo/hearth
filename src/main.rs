//! hearth: a game launcher for Linux. Steam, Lutris, repacks and portable games in one library,
//! with per-game Proton/Wine, MangoHud, gamemode and gamescope settings.

mod art;
mod config;
mod install;
mod launch;
mod library;
mod model;
mod runners;
mod scan;
mod ui;
mod vdf;

use std::path::PathBuf;

const USAGE: &str = "hearth — game launcher

USAGE:
  hearth                      open the launcher
  hearth scan [--art]         find new games (and fetch missing cover art)
  hearth list                 list the library (id, runner, name)
  hearth launch ID [--dry-run]  start a game (dry run prints the command)
  hearth stop ID              stop a running game
  hearth install SETUP.exe [--name NAME] [--prefix DIR] [--proton BUILD]
                              install a Windows game and add it when done
  hearth runners              list Proton/Wine builds and tools
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        None => ui::run(),
        Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            0
        }
        Some("scan") => {
            let cfg = config::Config::load();
            let (lib, rep) = library::scan_and_merge(&cfg);
            for id in &rep.added {
                println!("added   {id}");
            }
            if args.iter().any(|a| a == "--art") {
                for id in art::fetch_missing(&lib.games) {
                    println!("art     {id}");
                }
            }
            println!("{} games, {} new", lib.games.len(), rep.added.len());
            0
        }
        Some("list") => {
            let lib = library::Library::load();
            for g in &lib.games {
                let st = lib.stats(&g.id);
                println!("{:<40} {:<8} {:<10} {}", g.id, g.runner.label(), model::fmt_playtime(st.total_secs()), g.name);
            }
            0
        }
        Some("launch") | Some("run") if args.len() >= 2 => {
            let id = &args[1];
            if args.iter().any(|a| a == "--dry-run") {
                let cfg = config::Config::load();
                let lib = library::Library::load();
                match lib.get(id).ok_or("unknown game".to_string()).and_then(|g| launch::plan(g, &cfg)) {
                    Ok(p) => {
                        println!("{}", p.render());
                        if let Some(w) = &p.workdir {
                            println!("# in {}", w.display());
                        }
                        0
                    }
                    Err(e) => {
                        eprintln!("hearth: {e}");
                        1
                    }
                }
            } else if args[0] == "run" {
                // The supervisor (normally started detached by the GUI).
                match launch::run(id) {
                    Ok(()) => 0,
                    Err(e) => {
                        eprintln!("hearth: {e}");
                        1
                    }
                }
            } else {
                match launch::spawn_supervisor(id) {
                    Ok(()) => 0,
                    Err(e) => {
                        eprintln!("hearth: {e}");
                        1
                    }
                }
            }
        }
        Some("stop") if args.len() >= 2 => match launch::stop(&args[1]) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("hearth: {e}");
                1
            }
        },
        Some("install") if args.len() >= 2 => {
            let mut req = install::Request { setup: PathBuf::from(&args[1]), ..Default::default() };
            let mut job = None;
            let mut it = args[2..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--name" => req.name = it.next().cloned(),
                    "--prefix" => req.prefix = it.next().map(PathBuf::from),
                    "--proton" => req.proton = it.next().cloned(),
                    "--job" => job = it.next().cloned(),
                    _ => {}
                }
            }
            let job = job.unwrap_or_else(|| format!("{}-{}", model::now(), std::process::id()));
            match install::run(req, job) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("hearth: {e}");
                    1
                }
            }
        }
        Some("runners") => {
            println!("Proton:");
            for b in runners::protons() {
                println!("  {:<28} {:<7} {}", b.name, b.origin, b.path.display());
            }
            println!("Wine:");
            for b in runners::wines() {
                println!("  {:<28} {:<7} {}", b.name, b.origin, b.path.display());
            }
            println!("Tools:");
            for (n, p) in runners::tools() {
                println!("  {:<28} {}", n, p.map(|p| p.display().to_string()).unwrap_or_else(|| "missing".into()));
            }
            0
        }
        Some(_) => {
            eprint!("{USAGE}");
            2
        }
    };
    std::process::exit(code);
}

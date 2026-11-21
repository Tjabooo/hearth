//! Small modal windows: install from .exe, add game, confirm, show command, file pickers.

use super::{AppRc, button, label, settings};
use crate::config::{expand, tilde};
use crate::library::{self, Art};
use crate::model::{Game, RunnerKind, Source, clean_title, now};
use crate::{install, launch, runners};
use gtk::{gio, glib, prelude::*};
use gtk4 as gtk;
use std::path::{Path, PathBuf};

/// A modal window in the deck style: header (title, sub, ✕), body, footer. Esc closes.
pub fn modal(
    app: &AppRc,
    title: &str,
    sub: &str,
    w: i32,
    h: i32,
) -> (gtk::Window, gtk::Box, gtk::Box) {
    let win = gtk::Window::builder()
        .transient_for(&app.window)
        .modal(true)
        .title(title)
        .default_width(w)
        .build();
    if h > 0 {
        win.set_default_height(h);
    }
    // Same app id as the main window (window rules, taskbars), not the binary name.
    win.set_application(app.window.application().as_ref());
    win.add_css_class("dialog-window");
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    head.add_css_class("dialog-head");
    let tb = gtk::Box::new(gtk::Orientation::Vertical, 2);
    tb.set_hexpand(true);
    tb.append(&label(title, "dialog-title"));
    if !sub.is_empty() {
        let s = label(sub, "dialog-sub");
        s.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        s.set_max_width_chars(60);
        tb.append(&s);
    }
    head.append(&tb);
    let x = button("󰅖", "close");
    x.set_valign(gtk::Align::Center);
    {
        let w = win.clone();
        x.connect_clicked(move |_| w.close());
    }
    head.append(&x);
    let handle = gtk::WindowHandle::new();
    handle.set_child(Some(&head));
    win.set_titlebar(Some(&handle));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("dialog");
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.add_css_class("dialog-body");
    body.set_vexpand(true);
    let foot = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    foot.add_css_class("dialog-foot");
    foot.set_halign(gtk::Align::Fill);
    let sp = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    sp.set_hexpand(true);
    foot.append(&sp);
    root.append(&body);
    root.append(&foot);
    win.set_child(Some(&root));

    let keys = gtk::EventControllerKey::new();
    let w2 = win.clone();
    keys.connect_key_pressed(move |_, k, _, _| {
        if k == gtk::gdk::Key::Escape {
            w2.close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    win.add_controller(keys);
    (win, body, foot)
}

pub fn file_filter(name: &str, patterns: &[&str]) -> gtk::FileFilter {
    let f = gtk::FileFilter::new();
    f.set_name(Some(name));
    for p in patterns {
        f.add_pattern(p);
        // Case-insensitive for the usual Windows suspects.
        let up = p.to_uppercase();
        if up != *p {
            f.add_pattern(&up);
        }
    }
    f
}

/// Open-file picker. `filters` first entry is the default.
pub fn pick_file(
    app: &AppRc,
    title: &str,
    start: Option<PathBuf>,
    filters: Vec<gtk::FileFilter>,
    done: impl FnOnce(PathBuf) + 'static,
) {
    let d = gtk::FileDialog::builder().title(title).modal(true).build();
    if let Some(s) = start.filter(|p| p.is_dir()) {
        d.set_initial_folder(Some(&gio::File::for_path(s)));
    }
    if !filters.is_empty() {
        let store = gio::ListStore::new::<gtk::FileFilter>();
        for f in &filters {
            store.append(f);
        }
        d.set_filters(Some(&store));
        d.set_default_filter(filters.first());
    }
    d.open(Some(&app.window), gio::Cancellable::NONE, move |res| {
        if let Some(p) = res.ok().and_then(|f| f.path()) {
            done(p);
        }
    });
}

pub fn pick_folder(
    parent: &impl IsA<gtk::Window>,
    title: &str,
    start: Option<PathBuf>,
    done: impl FnOnce(PathBuf) + 'static,
) {
    let d = gtk::FileDialog::builder().title(title).modal(true).build();
    if let Some(s) = start.filter(|p| p.is_dir()) {
        d.set_initial_folder(Some(&gio::File::for_path(s)));
    }
    d.select_folder(Some(parent), gio::Cancellable::NONE, move |res| {
        if let Some(p) = res.ok().and_then(|f| f.path()) {
            done(p);
        }
    });
}

// ── install from .exe ─────────────────────────────────────────────────────────────────────

/// Windows path an installer will be pointed at (mirrors install.rs: X: maps to the drive target).
fn target_win(cfg: &crate::config::Config, target: &Path) -> String {
    let letter = cfg
        .install_drive
        .chars()
        .next()
        .unwrap_or('x')
        .to_ascii_uppercase();
    let drive = expand(&cfg.install_drive_target);
    match target.strip_prefix(&drive) {
        Ok(rel) => format!("{letter}:\\{}", rel.to_string_lossy().replace('/', "\\")),
        Err(_) => format!(
            "{letter}:\\{}",
            target.file_name().unwrap_or_default().to_string_lossy()
        ),
    }
}

fn safe_name(n: &str) -> String {
    n.chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn install_flow(app: &AppRc) {
    let start = expand(&app.cfg.borrow().downloads_dir);
    let a = app.clone();
    pick_file(
        app,
        "Pick the installer (.exe)",
        Some(start),
        vec![file_filter("Windows installers", &["*.exe", "*.msi"])],
        move |p| install_dialog(&a, p),
    );
}

pub fn install_dialog(app: &AppRc, setup: PathBuf) {
    let cfg = app.cfg.borrow().clone();
    let (win, body, foot) = modal(app, "INSTALL FROM .EXE", &tilde(&setup), 640, -1);

    let note = label(
        "The installer opens in a fresh Proton prefix. Keep the folder it suggests and hearth adds the game when the installer closes.",
        "note",
    );
    note.set_wrap(true);
    note.set_max_width_chars(70);
    body.append(&note);

    let grid = gtk::Grid::new();
    grid.set_row_spacing(8);
    grid.set_column_spacing(12);
    grid.set_margin_top(10);
    let name = gtk::Entry::new();
    name.set_text(&install::guess_name(&setup));
    name.set_hexpand(true);
    let builds: Vec<String> = runners::protons().into_iter().map(|b| b.name).collect();
    let mut labels = vec![
        builds
            .first()
            .map(|b| format!("Latest ({b})"))
            .unwrap_or_else(|| "Latest".into()),
    ];
    labels.extend(builds.iter().cloned());
    let proton =
        gtk::DropDown::from_strings(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    let prefix = gtk::Entry::new();
    prefix.set_hexpand(true);
    let pbrowse = button("󰉋", "");
    let pbox = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    pbox.append(&prefix);
    pbox.append(&pbrowse);
    let target = label("", "target");
    target.set_wrap(true);
    let target_sub = label("", "opt-help");
    target_sub.set_wrap(true);
    target_sub.set_max_width_chars(60);
    target.set_max_width_chars(60);

    for (i, (k, w)) in [
        ("NAME", name.clone().upcast::<gtk::Widget>()),
        ("PROTON", proton.clone().upcast()),
        ("PREFIX", pbox.clone().upcast()),
    ]
    .into_iter()
    .enumerate()
    {
        let kl = label(k, "k");
        kl.set_valign(gtk::Align::Center);
        grid.attach(&kl, 0, i as i32, 1, 1);
        grid.attach(&w, 1, i as i32, 1, 1);
    }
    let kl = label("INSTALLS TO", "k");
    kl.set_valign(gtk::Align::Start);
    grid.attach(&kl, 0, 3, 1, 1);
    let tb = gtk::Box::new(gtk::Orientation::Vertical, 2);
    tb.append(&target);
    tb.append(&target_sub);
    grid.attach(&tb, 1, 3, 1, 1);
    body.append(&grid);

    if builds.is_empty() {
        let warn = label(
            "No Proton build found. Install GE-Proton (e.g. with ProtonUp-Qt) first.",
            "note",
        );
        warn.set_wrap(true);
        warn.set_max_width_chars(70);
        body.append(&warn);
    }

    // Prefix follows the name until the user edits it.
    let prefix_root = expand(&cfg.prefix_root);
    let install_root = expand(&cfg.install_root);
    let auto = std::rc::Rc::new(std::cell::Cell::new(true));
    let update = {
        let (name, prefix, target, target_sub, auto, cfg) = (
            name.clone(),
            prefix.clone(),
            target.clone(),
            target_sub.clone(),
            auto.clone(),
            cfg.clone(),
        );
        move || {
            let n = safe_name(&name.text());
            if auto.get() {
                prefix.set_text(&tilde(&prefix_root.join(&n)));
                auto.set(true);
            }
            let t = install_root.join(&n);
            target.set_text(&target_win(&cfg, &t));
            target_sub.set_text(&format!(
                "= {}  ·  pre-filled for Inno Setup (FitGirl, DODI) and NSIS installers",
                tilde(&t)
            ));
        }
    };
    update();
    {
        let u = update.clone();
        name.connect_changed(move |_| u());
    }
    {
        let auto = auto.clone();
        let prefix2 = prefix.clone();
        prefix.connect_changed(move |e| {
            // set_text from update() also fires this; only a differing manual edit turns auto off.
            if e.has_focus() {
                auto.set(false);
            }
            let _ = &prefix2;
        });
    }
    {
        let (prefix, win2) = (prefix.clone(), win.clone());
        let auto = auto.clone();
        pbrowse.connect_clicked(move |_| {
            let p2 = prefix.clone();
            let auto = auto.clone();
            pick_folder(
                &win2,
                "Prefix folder",
                Some(expand(&prefix.text())).and_then(|p| p.parent().map(Path::to_path_buf)),
                move |p| {
                    auto.set(false);
                    p2.set_text(&tilde(&p));
                },
            );
        });
    }

    let cancel = button("CANCEL", "");
    let go = button("󰇚  START INSTALLER", "accent");
    go.set_sensitive(!builds.is_empty());
    foot.append(&cancel);
    foot.append(&go);
    {
        let w = win.clone();
        cancel.connect_clicked(move |_| w.close());
    }
    {
        let (a, w) = (app.clone(), win.clone());
        go.connect_clicked(move |_| {
            let n = name.text().trim().to_string();
            if n.is_empty() {
                name.grab_focus();
                return;
            }
            let sel = proton.selected() as usize;
            let req = install::Request {
                setup: setup.clone(),
                name: Some(n.clone()),
                prefix: Some(expand(prefix.text().trim())),
                proton: (sel > 0).then(|| builds[sel - 1].clone()),
            };
            match install::start(&req) {
                Ok(_) => {
                    super::toast(&a, &format!("Starting the {n} installer…"));
                    w.close();
                }
                Err(e) => super::toast(&a, &format!("Couldn't start: {e}")),
            }
        });
    }
    win.present();
    go.grab_focus();
}

// ── add a game by hand ────────────────────────────────────────────────────────────────────

pub fn add_game(app: &AppRc) {
    let start = app
        .cfg
        .borrow()
        .game_dirs()
        .into_iter()
        .find(|p| p.is_dir());
    let a = app.clone();
    let filters = vec![
        file_filter(
            "Games (.exe, AppImage, scripts)",
            &[
                "*.exe",
                "*.AppImage",
                "*.appimage",
                "*.sh",
                "*.x86_64",
                "*.x86",
                "*",
            ],
        ),
        file_filter("Windows .exe", &["*.exe"]),
    ];
    pick_file(
        app,
        "Pick the game's .exe, AppImage or launcher",
        start,
        filters,
        move |p| {
            let is_exe = p
                .extension()
                .map(|e| {
                    e.eq_ignore_ascii_case("exe")
                        || e.eq_ignore_ascii_case("bat")
                        || e.eq_ignore_ascii_case("msi")
                })
                .unwrap_or(false);
            let dir = p.parent().map(Path::to_path_buf);
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            // Generic exe names ("Game.exe", "launcher") -> use the folder name.
            let generic = ["game", "launcher", "start", "play", "run", "setup", "main"]
                .contains(&stem.to_lowercase().as_str())
                || stem.len() <= 3;
            let raw = if generic {
                dir.as_ref()
                    .and_then(|d| d.file_name())
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or(stem)
            } else {
                stem
            };
            let name = clean_title(&raw);
            let runner = if is_exe {
                RunnerKind::Proton
            } else {
                RunnerKind::Native
            };
            let prefix = if is_exe {
                crate::scan::prefix_of(&p)
            } else {
                None
            };
            let mut g = Game {
                name: if name.is_empty() {
                    "New game".into()
                } else {
                    name
                },
                source: Source::Manual,
                runner,
                exe: Some(p.clone()),
                install_dir: dir,
                prefix,
                added: now(),
                ..Default::default()
            };
            g.id = a.lib.borrow().new_id(&g.name);
            if let Err(e) = a.lib.borrow_mut().upsert(g.clone()) {
                super::toast(&a, &format!("Couldn't save: {e}"));
                return;
            }
            super::select(&a, &g.id);
            settings::game_dialog(&a, &g.id);
            // Try to find art in the background.
            if a.cfg.borrow().fetch_art {
                let (a2, g2) = (a.clone(), g.clone());
                glib::spawn_future_local(async move {
                    if gio::spawn_blocking(move || crate::art::fetch_for(&g2))
                        .await
                        .unwrap_or(false)
                    {
                        super::refresh_grid(&a2);
                    }
                });
            }
        },
    );
}

// ── actions ───────────────────────────────────────────────────────────────────────────────

pub fn confirm_remove(app: &AppRc, id: &str) {
    let Some(g) = app.lib.borrow().get(id).cloned() else {
        return;
    };
    let (win, body, foot) = modal(app, "REMOVE FROM LIBRARY", &g.name, 480, -1);
    let t = label(
        &format!(
            "Remove {} from hearth? Files, saves and the prefix stay on disk, and scans won't add it back.",
            g.name
        ),
        "",
    );
    t.set_wrap(true);
    body.append(&t);
    let cancel = button("CANCEL", "");
    let rm = button("󰆴  REMOVE", "danger");
    foot.append(&cancel);
    foot.append(&rm);
    {
        let w = win.clone();
        cancel.connect_clicked(move |_| w.close());
    }
    {
        let (a, w, id) = (app.clone(), win.clone(), id.to_string());
        rm.connect_clicked(move |_| {
            let _ = a.lib.borrow_mut().remove(&id);
            *a.selected.borrow_mut() = None;
            super::refresh_grid(&a);
            w.close();
        });
    }
    win.present();
    cancel.grab_focus();
}

pub fn show_command(app: &AppRc, id: &str) {
    let Some(g) = app.lib.borrow().get(id).cloned() else {
        return;
    };
    let cfg = app.cfg.borrow().clone();
    let (text, wd) = match launch::plan(&g, &cfg) {
        Ok(p) => (p.render(), p.workdir.map(|w| tilde(&w))),
        Err(e) => (format!("Can't build the command: {e}"), None),
    };
    let (win, body, foot) = modal(app, "LAUNCH COMMAND", &g.name, 760, -1);
    let l = label(&text, "cmd");
    l.set_wrap(true);
    l.set_wrap_mode(gtk::pango::WrapMode::Char);
    l.set_selectable(true);
    body.append(&l);
    if let Some(w) = wd {
        body.append(&label(&format!("runs in {w}"), "opt-help"));
    }
    let copy = button("󰆏  COPY", "");
    let close = button("CLOSE", "accent");
    foot.append(&copy);
    foot.append(&close);
    {
        let w = win.clone();
        copy.connect_clicked(move |_| {
            w.clipboard().set_text(&text);
        });
    }
    {
        let w = win.clone();
        close.connect_clicked(move |_| w.close());
    }
    win.present();
}

pub fn run_exe_in_prefix(app: &AppRc, id: &str) {
    let Some(g) = app.lib.borrow().get(id).cloned() else {
        return;
    };
    let start = g
        .install_dir
        .clone()
        .or_else(|| Some(expand(&app.cfg.borrow().downloads_dir)));
    let a = app.clone();
    pick_file(
        app,
        &format!("Run an .exe in {}'s prefix", g.name),
        start,
        vec![file_filter(
            "Windows programs",
            &["*.exe", "*.msi", "*.bat"],
        )],
        move |p| {
            let cfg = a.cfg.borrow().clone();
            match launch::run_in_prefix(&g, &cfg, &p) {
                Ok(()) => super::toast(
                    &a,
                    &format!(
                        "Running {}",
                        p.file_name().unwrap_or_default().to_string_lossy()
                    ),
                ),
                Err(e) => super::toast(&a, &e),
            }
        },
    );
}

pub fn change_cover(app: &AppRc, id: &str) {
    let id = id.to_string();
    let a = app.clone();
    let start = Some(crate::config::home().join("Pictures"));
    pick_file(
        app,
        "Pick a cover image (portrait, 2:3)",
        start,
        vec![file_filter(
            "Images",
            &["*.jpg", "*.jpeg", "*.png", "*.webp"],
        )],
        move |p| {
            let dst = library::art_path(&id, Art::Cover);
            if let Some(d) = dst.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            // Normalise to the cache file name; GdkPixbuf reads any format regardless of extension.
            match std::fs::copy(&p, &dst) {
                Ok(_) => {
                    // An explicit override would win over the cache file: drop it.
                    let g = a.lib.borrow().get(&id).cloned();
                    if let Some(mut g) = g.filter(|g| g.cover.is_some()) {
                        g.cover = None;
                        let _ = a.lib.borrow_mut().upsert(g);
                    }
                    a.images.forget(&dst);
                    super::refresh_grid(&a);
                }
                Err(e) => super::toast(&a, &format!("Couldn't copy: {e}")),
            }
        },
    );
}

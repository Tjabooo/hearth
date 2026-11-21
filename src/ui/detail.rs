//! The right-hand pane: hero art, PLAY/STOP, stats, effective launch options, actions.

use super::{AppRc, button, dialogs, label, settings};
use crate::config::{self, tilde};
use crate::library::{self, Art};
use crate::model::{Game, RunnerKind, fmt_ago, fmt_playtime};
use crate::{launch, runners};
use gtk::{glib, prelude::*};
use gtk4 as gtk;

const HERO_H: i32 = 250;

pub fn show(app: &AppRc) {
    while let Some(c) = app.detail.first_child() {
        app.detail.remove(&c);
    }
    let id = app.selected.borrow().clone();
    let game = id.as_ref().and_then(|id| app.lib.borrow().get(id).cloned());
    let Some(g) = game else {
        let e = gtk::Box::new(gtk::Orientation::Vertical, 8);
        e.set_vexpand(true);
        e.set_valign(gtk::Align::Center);
        e.set_halign(gtk::Align::Center);
        e.append(&label("NO GAME SELECTED", "empty-big"));
        app.detail.append(&e);
        return;
    };
    app.detail.append(&hero(app, &g));
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroll.set_vexpand(true);
    scroll.set_child(Some(&body(app, &g)));
    app.detail.append(&scroll);
}

fn hero(app: &AppRc, g: &Game) -> gtk::Overlay {
    let o = gtk::Overlay::new();
    o.add_css_class("hero");
    let base = gtk::Box::new(gtk::Orientation::Vertical, 0);
    base.set_size_request(-1, HERO_H);
    base.add_css_class("hero-plain");
    o.set_child(Some(&base));

    let pic = gtk::Picture::new();
    pic.set_content_fit(gtk::ContentFit::Cover);
    pic.set_can_shrink(true);
    pic.set_visible(false);
    o.add_overlay(&pic);
    let hero = library::art(g, Art::Hero);
    let src = hero.clone().or_else(|| library::art(g, Art::Cover));
    if let Some(p) = src {
        if hero.is_none() {
            pic.add_css_class("hero-blur");
        }
        let p2 = pic.clone();
        app.images
            .load(&pic, &p, 960, 420, move |ok| p2.set_visible(ok));
    }
    let shade = gtk::Box::new(gtk::Orientation::Vertical, 0);
    shade.add_css_class("hero-shade");
    shade.set_can_target(false);
    o.add_overlay(&shade);

    // Logo if we have one (not for desktop-entry icons, which are square app icons).
    let title_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    title_box.set_valign(gtk::Align::End);
    title_box.set_halign(gtk::Align::Start);
    let logo = library::art(g, Art::Logo)
        .filter(|_| g.runner != RunnerKind::Command && library::art(g, Art::Hero).is_some());
    let title = label(&g.name, "hero-title");
    title.set_wrap(true);
    title.set_max_width_chars(26);
    if let Some(p) = logo {
        let lp = gtk::Picture::new();
        lp.set_content_fit(gtk::ContentFit::Contain);
        lp.set_can_shrink(true);
        lp.add_css_class("hero-logo");
        lp.set_halign(gtk::Align::Start);
        lp.set_size_request(300, 92);
        title.set_visible(false);
        let t2 = title.clone();
        let l2 = lp.clone();
        app.images.load(&lp, &p, 320, 110, move |ok| {
            t2.set_visible(!ok);
            l2.set_visible(ok);
        });
        title_box.append(&lp);
    }
    title_box.append(&title);
    o.add_overlay(&title_box);

    let tags = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    tags.add_css_class("hero-tag");
    tags.set_halign(gtk::Align::End);
    tags.set_valign(gtk::Align::Start);
    if app.running.borrow().contains(&g.id) {
        tags.append(&label("󰐊 RUNNING", "lamp run"));
    }
    tags.append(&label(super::runner_tag(g), "lamp ok"));
    if g.hidden {
        tags.append(&label("HIDDEN", "lamp idle"));
    }
    o.add_overlay(&tags);
    o
}

fn kv(grid: &gtk::Grid, row: i32, k: &str, v: &str, class: &str) {
    let kl = label(k, "k");
    kl.set_valign(gtk::Align::Start);
    grid.attach(&kl, 0, row, 1, 1);
    let vl = label(v, &format!("v {class}"));
    vl.set_wrap(true);
    vl.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    vl.set_selectable(false);
    vl.set_hexpand(true);
    grid.attach(&vl, 1, row, 1, 1);
}

/// "Proton · GE-Proton11-12 (latest)"
pub fn runner_line(app: &AppRc, g: &Game) -> String {
    let o = g.options.over(&app.cfg.borrow().defaults);
    match g.runner {
        RunnerKind::Steam => "Steam client".into(),
        RunnerKind::Proton => {
            let want = o.proton.unwrap_or_else(|| "latest".into());
            match runners::find_proton(&want) {
                Some(b) if want == "latest" => format!("Proton · {} (latest)", b.name),
                Some(b) => format!("Proton · {}", b.name),
                None => format!("Proton · {want} (missing)"),
            }
        }
        RunnerKind::Wine => format!("Wine · {}", o.wine.unwrap_or_else(|| "system".into())),
        RunnerKind::Native => "Linux native".into(),
        RunnerKind::Command => "Command".into(),
    }
}

fn body(app: &AppRc, g: &Game) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.add_css_class("detail-body");

    // ── play row ──
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let running = app.running.borrow().contains(&g.id);
    let starting = app.starting.borrow().contains_key(&g.id);
    let play = if running {
        button("󰓛  STOP", "play stop")
    } else if starting {
        button("󰔟  STARTING…", "play starting")
    } else {
        button("󰐊  PLAY", "play")
    };
    play.set_hexpand(true);
    {
        let a = app.clone();
        let id = g.id.clone();
        play.connect_clicked(move |_| super::play_or_stop(&a, &id));
    }
    row.append(&play);
    let fav = button(if g.favorite { "󰓎" } else { "󰓒" }, "icon");
    if g.favorite {
        fav.add_css_class("on");
    }
    fav.set_tooltip_text(Some(if g.favorite { "Unfavorite" } else { "Favorite" }));
    {
        let a = app.clone();
        let id = g.id.clone();
        fav.connect_clicked(move |_| edit(&a, &id, |g| g.favorite = !g.favorite));
    }
    row.append(&fav);
    let gear = button("󰒓", "icon");
    gear.set_tooltip_text(Some("Game settings"));
    {
        let a = app.clone();
        let id = g.id.clone();
        gear.connect_clicked(move |_| settings::game_dialog(&a, &id));
    }
    row.append(&gear);
    b.append(&row);

    // ── stats ──
    let rule = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    rule.add_css_class("rule");
    rule.add_css_class("hot");
    b.append(&rule);
    let grid = gtk::Grid::new();
    grid.set_row_spacing(5);
    grid.set_column_spacing(10);
    let st = app.lib.borrow().stats(&g.id);
    let mut r = 0;
    let mut add = |k: &str, v: &str, c: &str| {
        kv(&grid, r, k, v, c);
        r += 1;
    };
    add("PLAYTIME", &fmt_playtime(st.total_secs()), "hot");
    add("LAST PLAYED", &fmt_ago(st.last_played), "");
    if st.launches > 0 {
        add("LAUNCHES", &st.launches.to_string(), "");
    }
    add("RUNNER", &runner_line(app, g), "");
    add("SOURCE", g.source.label(), "");
    if let Some(a) = g.steam_appid.filter(|_| g.runner == RunnerKind::Steam) {
        add("APP ID", &a.to_string(), "mono");
    }
    if let Some(p) = &g.prefix {
        add("PREFIX", &tilde(p), "mono");
    } else if g.runner.is_windows() {
        add(
            "PREFIX",
            &format!(
                "{} (new)",
                tilde(&launch::default_prefix(&app.cfg.borrow(), g))
            ),
            "mono",
        );
    }
    if let Some(d) = &g.install_dir {
        add("FOLDER", &tilde(d), "mono");
    }
    if let Some(e) = &g.exe {
        add(
            "EXE",
            &e.file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default(),
            "mono",
        );
    }
    b.append(&grid);

    // ── effective switches, MAGI-lamp style ──
    if g.runner != RunnerKind::Steam {
        let o = g.options.over(&app.cfg.borrow().defaults);
        let r2 = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        r2.add_css_class("rule");
        b.append(&r2);
        b.append(&label("LAUNCH", "section-label"));
        let lamps = gtk::FlowBox::new();
        lamps.set_selection_mode(gtk::SelectionMode::None);
        lamps.set_max_children_per_line(6);
        lamps.set_row_spacing(5);
        lamps.set_column_spacing(5);
        lamps.set_homogeneous(false);
        let mut items: Vec<(&str, bool)> = vec![
            ("MANGOHUD", o.mangohud == Some(true)),
            ("GAMEMODE", o.gamemode == Some(true)),
            ("GAMESCOPE", o.gamescope == Some(true)),
        ];
        if !app.cfg.borrow().wrapper.is_empty() {
            items.push(("WRAPPER", o.use_wrapper == Some(true)));
        }
        if g.runner == RunnerKind::Proton {
            items.push((
                "FSR4",
                o.fsr4 == Some(true)
                    || (o.fsr4.is_none()
                        && std::env::var("PROTON_FSR4_UPGRADE")
                            .map(|v| v == "1")
                            .unwrap_or(false)),
            ));
            items.push(("WAYLAND", o.wine_wayland == Some(true)));
        }
        if o.prelaunch
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            items.push(("PRE-LAUNCH", true));
        }
        if !o.env.is_empty() {
            items.push(("ENV", true));
        }
        for (n, on) in items {
            let l = label(n, if on { "lamp ok" } else { "lamp idle" });
            lamps.insert(&l, -1);
        }
        b.append(&lamps);
    }

    // ── actions ──
    let r3 = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    r3.add_css_class("rule");
    b.append(&r3);
    b.append(&label("ACTIONS", "section-label"));
    let acts = gtk::FlowBox::new();
    acts.set_selection_mode(gtk::SelectionMode::None);
    acts.set_max_children_per_line(4);
    acts.set_row_spacing(5);
    acts.set_column_spacing(5);
    let act = |text: &str, tip: &str, class: &str, f: Box<dyn Fn()>| {
        let bt = button(text, &format!("chip {class}"));
        bt.set_tooltip_text(Some(tip));
        bt.connect_clicked(move |_| f());
        acts.insert(&bt, -1);
    };
    let a = app.clone();
    let id = g.id.clone();
    if let Some(dir) = g.install_dir.clone().or_else(|| {
        g.exe
            .as_ref()
            .and_then(|e| e.parent().map(|p| p.to_path_buf()))
    }) {
        act(
            "󰉋 FOLDER",
            "Open the game folder",
            "",
            Box::new(move || super::open_path(&dir)),
        );
    }
    let logp = config::log_dir().join(format!("{}.log", g.id));
    if logp.is_file() {
        act(
            "󰦪 LOG",
            "Open the last launch log",
            "",
            Box::new(move || super::open_path(&logp)),
        );
    }
    {
        let (a, id) = (a.clone(), id.clone());
        act(
            "󰆍 COMMAND",
            "Show the exact launch command",
            "",
            Box::new(move || dialogs::show_command(&a, &id)),
        );
    }
    if g.runner.is_windows() {
        {
            let (a, id) = (a.clone(), id.clone());
            act(
                "󰐊 RUN .EXE",
                "Run another .exe (patch, trainer, config tool) in this game's prefix",
                "",
                Box::new(move || dialogs::run_exe_in_prefix(&a, &id)),
            );
        }
        if let Some(p) = g.prefix.clone().filter(|p| p.is_dir()) {
            act(
                "󰉋 PREFIX",
                "Open the Wine prefix folder",
                "",
                Box::new(move || super::open_path(&p)),
            );
        }
        for (t, tool, tip) in [
            ("WINECFG", "winecfg", "Wine configuration for this prefix"),
            ("REGEDIT", "regedit", "Registry editor for this prefix"),
            (
                "WINETRICKS",
                "winetricks",
                "Install runtimes/fonts into this prefix",
            ),
        ] {
            let (a, id) = (a.clone(), id.clone());
            act(t, tip, "", Box::new(move || prefix_tool(&a, &id, tool)));
        }
    }
    {
        let (a, id) = (a.clone(), id.clone());
        act(
            "󰋩 COVER",
            "Pick a new cover image",
            "",
            Box::new(move || dialogs::change_cover(&a, &id)),
        );
    }
    {
        let (a, id) = (a.clone(), id.clone());
        let hidden = g.hidden;
        act(
            if hidden { "󰈈 UNHIDE" } else { "󰈉 HIDE" },
            "Hide from the library (kept under HIDDEN)",
            "",
            Box::new(move || edit(&a, &id, |g| g.hidden = !g.hidden)),
        );
    }
    {
        let (a, id) = (a.clone(), id.clone());
        act(
            "󰆴 REMOVE",
            "Remove from the library (files stay on disk)",
            "danger",
            Box::new(move || dialogs::confirm_remove(&a, &id)),
        );
    }
    b.append(&acts);
    b
}

fn prefix_tool(app: &AppRc, id: &str, tool: &str) {
    let Some(g) = app.lib.borrow().get(id).cloned() else {
        return;
    };
    let cfg = app.cfg.borrow().clone();
    let tool = tool.to_string();
    let a = app.clone();
    glib::spawn_future_local(async move {
        let res = gtk::gio::spawn_blocking(move || launch::prefix_tool(&g, &cfg, &tool)).await;
        if let Ok(Err(e)) = res {
            super::toast(&a, &e);
        }
    });
}

/// Change one game, save, refresh.
pub fn edit(app: &AppRc, id: &str, f: impl FnOnce(&mut Game)) {
    let g = app.lib.borrow().get(id).cloned();
    let Some(mut g) = g else { return };
    f(&mut g);
    if let Err(e) = app.lib.borrow_mut().upsert(g) {
        super::toast(app, &format!("Couldn't save: {e}"));
    }
    super::refresh_grid(app);
}

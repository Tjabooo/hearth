//! GTK4 interface: sidebar filters, cover grid, detail pane, settings, installer flow.
//!
//! Layout
//!   ┌ topbar: HEARTH │ counts ……… search · sort · SCAN · ADD · INSTALL FROM .EXE · ⚙ ┐
//!   │ sidebar (filters, now playing) │ cover grid │ detail pane (hero, PLAY, stats) │
//!   └ install jobs strip + key hints ────────────────────────────────────────────────┘
//! Everything slow (scan, art, stop) runs in gio::spawn_blocking; two cheap 2 s timers poll the
//! running/ and installs/ state folders.

mod detail;
mod dialogs;
mod images;
mod settings;
mod settings_search;

use gtk4 as gtk;
use crate::config::Config;
use crate::install;
use crate::library::{self, Library};
use crate::model::{fmt_playtime, squash, Game, RunnerKind, Source, Stats};
use gtk::{gdk, gio, glib, prelude::*};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

pub const APP_ID: &str = "dev.hearth.Hearth";
const TILE_W: i32 = 168;
const TILE_H: i32 = 252;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
    All,
    Favorites,
    Recent,
    Steam,
    Windows,
    Linux,
    Apps,
    Hidden,
}

impl Filter {
    const ALL: [Filter; 8] = [Filter::All, Filter::Favorites, Filter::Recent, Filter::Steam, Filter::Windows, Filter::Linux, Filter::Apps, Filter::Hidden];
    fn label(self) -> &'static str {
        match self {
            Filter::All => "ALL GAMES",
            Filter::Favorites => "FAVORITES",
            Filter::Recent => "RECENT",
            Filter::Steam => "STEAM",
            Filter::Windows => "WINDOWS",
            Filter::Linux => "LINUX",
            Filter::Apps => "APPS",
            Filter::Hidden => "HIDDEN",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Favorites => "favorites",
            Filter::Recent => "recent",
            Filter::Steam => "steam",
            Filter::Windows => "windows",
            Filter::Linux => "linux",
            Filter::Apps => "apps",
            Filter::Hidden => "hidden",
        }
    }
    fn from_key(k: &str) -> Filter {
        Filter::ALL.into_iter().find(|f| f.key() == k).unwrap_or(Filter::All)
    }
    fn matches(self, g: &Game, st: &Stats) -> bool {
        if self == Filter::Hidden {
            return g.hidden;
        }
        if g.hidden {
            return false;
        }
        match self {
            Filter::All | Filter::Hidden => true,
            Filter::Favorites => g.favorite,
            Filter::Recent => st.last_played > 0 && crate::model::now() - st.last_played < 86_400 * 45,
            Filter::Steam => g.runner == RunnerKind::Steam,
            Filter::Windows => g.runner.is_windows(),
            Filter::Linux => g.runner == RunnerKind::Native,
            Filter::Apps => g.source == Source::Desktop || g.runner == RunnerKind::Command,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Name,
    LastPlayed,
    Playtime,
    Added,
}

impl Sort {
    const ALL: [Sort; 4] = [Sort::Name, Sort::LastPlayed, Sort::Playtime, Sort::Added];
    fn label(self) -> &'static str {
        match self {
            Sort::Name => "NAME",
            Sort::LastPlayed => "LAST PLAYED",
            Sort::Playtime => "PLAYTIME",
            Sort::Added => "RECENTLY ADDED",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Sort::Name => "name",
            Sort::LastPlayed => "last_played",
            Sort::Playtime => "playtime",
            Sort::Added => "added",
        }
    }
}

pub struct App {
    pub cfg: RefCell<Config>,
    pub lib: RefCell<Library>,
    pub filter: Cell<Filter>,
    pub sort: Cell<Sort>,
    pub query: RefCell<String>,
    pub selected: RefCell<Option<String>>,
    pub running: RefCell<Vec<String>>,
    pub starting: RefCell<HashMap<String, Instant>>,
    shown: RefCell<Vec<String>>,
    lamps: RefCell<HashMap<String, gtk::Label>>,
    pub window: gtk::ApplicationWindow,
    grid: gtk::FlowBox,
    grid_title: gtk::Label,
    grid_count: gtk::Label,
    grid_stack: gtk::Stack,
    pub detail: gtk::Box,
    nav: RefCell<Vec<(Filter, gtk::Button, gtk::Label)>>,
    now_box: gtk::Box,
    now_title: gtk::Label,
    jobs_box: gtk::Box,
    sys: gtk::Label,
    search: gtk::SearchEntry,
    pub images: images::Cache,
    scanning: Cell<bool>,
    scan_btn: gtk::Button,
    job_state: RefCell<HashMap<String, String>>,
    toast: gtk::Revealer,
    toast_label: gtk::Label,
    toast_gen: Cell<u32>,
}

pub type AppRc = Rc<App>;

pub fn run() -> i32 {
    let app = gtk::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::NON_UNIQUE).build();
    app.connect_activate(build);
    let code = app.run_with_args::<&str>(&[]);
    if code == glib::ExitCode::SUCCESS {
        0
    } else {
        1
    }
}

fn load_css() {
    let Some(display) = gdk::Display::default() else { return };
    if let Some(s) = gtk::Settings::default() {
        s.set_gtk_application_prefer_dark_theme(true);
    }
    let css = gtk::CssProvider::new();
    css.load_from_string(include_str!("style.css"));
    gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_USER);
}

pub fn label(text: &str, class: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    for c in class.split_whitespace() {
        l.add_css_class(c);
    }
    l.set_xalign(0.0);
    l
}

pub fn button(text: &str, class: &str) -> gtk::Button {
    let b = gtk::Button::with_label(text);
    for c in class.split_whitespace() {
        b.add_css_class(c);
    }
    b
}

fn build(gapp: &gtk::Application) {
    load_css();
    let cfg = Config::load();
    let lib = Library::load();

    let window = gtk::ApplicationWindow::builder().application(gapp).title("Hearth").build();
    window.add_css_class("hearth");
    let (w, h) = (cfg.ui.width, cfg.ui.height);
    window.set_default_size(if w > 400 { w } else { 1500 }, if h > 300 { h } else { 900 });
    if cfg.ui.maximized {
        window.maximize();
    }

    // ── topbar (also the title bar, so GTK doesn't draw its own) ──
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top.add_css_class("topbar");
    let brand = label("HEARTH", "brand");
    let sep = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sep.add_css_class("head-sep");
    let sys = label("", "sys");
    sys.set_valign(gtk::Align::Center);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let search = gtk::SearchEntry::new();
    search.add_css_class("search");
    search.set_placeholder_text(Some("Search  ( / )"));
    search.set_valign(gtk::Align::Center);
    let sort_dd = gtk::DropDown::from_strings(&Sort::ALL.map(|s| s.label()));
    sort_dd.set_valign(gtk::Align::Center);
    sort_dd.set_tooltip_text(Some("Sort"));
    let scan_btn = button("󰑐  SCAN", "");
    scan_btn.set_tooltip_text(Some("Find new games (F5)"));
    let add_btn = button("󰐕  ADD GAME", "");
    add_btn.set_tooltip_text(Some("Add a game by picking its .exe, AppImage or script"));
    let install_btn = button("󰇚  INSTALL FROM .EXE", "accent");
    install_btn.set_tooltip_text(Some("Run a Windows installer (FitGirl, DODI, GOG…) and add the game when it's done"));
    let prefs_btn = button("󰒓", "icon flat");
    prefs_btn.set_tooltip_text(Some("Preferences (Ctrl+,)"));
    let close_btn = button("󰅖", "close");
    for wdg in [&scan_btn, &add_btn, &install_btn, &prefs_btn, &close_btn] {
        wdg.set_valign(gtk::Align::Center);
    }
    top.append(&brand);
    top.append(&sep);
    top.append(&sys);
    top.append(&spacer);
    top.append(&search);
    top.append(&sort_dd);
    top.append(&scan_btn);
    top.append(&add_btn);
    top.append(&install_btn);
    top.append(&prefs_btn);
    top.append(&close_btn);
    let handle = gtk::WindowHandle::new();
    handle.set_child(Some(&top));
    window.set_titlebar(Some(&handle));

    // ── sidebar ──
    let side = gtk::Box::new(gtk::Orientation::Vertical, 0);
    side.add_css_class("sidebar");
    side.set_hexpand(false);
    side.append(&label("LIBRARY", "side-title"));
    let mut navs = vec![];
    for (i, f) in Filter::ALL.into_iter().enumerate() {
        let b = gtk::Button::new();
        b.add_css_class("nav");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.append(&label(&format!("{:02}", i + 1), "nav-idx"));
        let name = label(f.label(), "nav-name");
        name.set_hexpand(true);
        row.append(&name);
        let count = label("", "nav-count");
        row.append(&count);
        b.set_child(Some(&row));
        side.append(&b);
        navs.push((f, b, count));
    }
    let now_title = label("NOW PLAYING", "side-title");
    now_title.set_visible(false);
    side.append(&now_title);
    let now_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    side.append(&now_box);
    let side_spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    side_spacer.set_vexpand(true);
    side.append(&side_spacer);
    let ver = label(&format!("hearth {}", env!("CARGO_PKG_VERSION")), "hint");
    ver.set_margin_start(16);
    side.append(&ver);

    // ── grid ──
    let grid = gtk::FlowBox::new();
    grid.add_css_class("grid");
    grid.set_selection_mode(gtk::SelectionMode::Single);
    grid.set_activate_on_single_click(false);
    grid.set_homogeneous(true);
    grid.set_valign(gtk::Align::Start);
    grid.set_max_children_per_line(40);
    grid.set_min_children_per_line(1);
    grid.set_row_spacing(6);
    grid.set_column_spacing(6);
    let grid_scroll = gtk::ScrolledWindow::new();
    grid_scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    grid_scroll.set_vexpand(true);
    grid_scroll.set_child(Some(&grid));
    let empty = gtk::Box::new(gtk::Orientation::Vertical, 8);
    empty.set_valign(gtk::Align::Center);
    empty.set_halign(gtk::Align::Center);
    empty.append(&label("NOTHING HERE", "empty-big"));
    let empty_hint = label("No games match. Try SCAN, ADD GAME or INSTALL FROM .EXE.", "empty");
    empty.append(&empty_hint);
    let grid_stack = gtk::Stack::new();
    grid_stack.add_named(&grid_scroll, Some("grid"));
    grid_stack.add_named(&empty, Some("empty"));
    let grid_head = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    grid_head.add_css_class("grid-head");
    let grid_title = label("ALL GAMES", "grid-title");
    let grid_count = label("", "grid-count");
    grid_count.set_valign(gtk::Align::Baseline);
    grid_head.append(&grid_title);
    grid_head.append(&grid_count);
    let center = gtk::Box::new(gtk::Orientation::Vertical, 0);
    center.add_css_class("library");
    center.set_hexpand(true);
    center.append(&grid_head);
    center.append(&grid_stack);

    let toast_label = label("", "");
    let toast_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    toast_box.add_css_class("toast");
    toast_box.append(&toast_label);
    let toast = gtk::Revealer::new();
    toast.set_transition_type(gtk::RevealerTransitionType::SlideUp);
    toast.set_transition_duration(160);
    toast.set_child(Some(&toast_box));
    toast.set_halign(gtk::Align::Center);
    toast.set_valign(gtk::Align::End);
    toast.set_can_target(false);
    let center_overlay = gtk::Overlay::new();
    center_overlay.set_child(Some(&center));
    center_overlay.add_overlay(&toast);

    // ── detail ──
    let detail = gtk::Box::new(gtk::Orientation::Vertical, 0);
    detail.add_css_class("detail");
    detail.set_size_request(480, -1);
    detail.set_hexpand(false);

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.set_vexpand(true);
    body.append(&side);
    body.append(&center_overlay);
    body.append(&detail);

    // ── bottom: jobs + hints ──
    let jobs_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let foot = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    foot.add_css_class("footbar");
    for (k, v) in [("ENTER", "play"), ("/", "search"), ("F5", "scan"), ("CTRL ,", "preferences"), ("ESC", "clear")] {
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        b.append(&label(k, "key"));
        b.append(&label(v, "hint"));
        foot.append(&b);
    }

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&body);
    root.append(&jobs_box);
    root.append(&foot);
    window.set_child(Some(&root));

    let filter = Filter::from_key(&cfg.ui.view);
    let sort = Sort::ALL.into_iter().find(|s| s.key() == cfg.ui.sort).unwrap_or(Sort::Name);
    sort_dd.set_selected(Sort::ALL.iter().position(|s| *s == sort).unwrap_or(0) as u32);

    let app: AppRc = Rc::new(App {
        cfg: RefCell::new(cfg),
        lib: RefCell::new(lib),
        filter: Cell::new(filter),
        sort: Cell::new(sort),
        query: RefCell::new(String::new()),
        selected: RefCell::new(None),
        running: RefCell::new(library::running_ids()),
        starting: RefCell::new(HashMap::new()),
        shown: RefCell::new(vec![]),
        lamps: RefCell::new(HashMap::new()),
        window: window.clone(),
        grid: grid.clone(),
        grid_title,
        grid_count,
        grid_stack,
        detail,
        nav: RefCell::new(navs),
        now_box,
        now_title,
        jobs_box,
        sys,
        search: search.clone(),
        images: images::Cache::default(),
        scanning: Cell::new(false),
        scan_btn: scan_btn.clone(),
        job_state: RefCell::new(HashMap::new()),
        toast,
        toast_label,
        toast_gen: Cell::new(0),
    });

    // ── signals ──
    for (f, b, _) in app.nav.borrow().iter() {
        let a = app.clone();
        let f = *f;
        b.connect_clicked(move |_| {
            a.filter.set(f);
            refresh_grid(&a);
        });
    }
    {
        let a = app.clone();
        sort_dd.connect_selected_notify(move |dd| {
            a.sort.set(Sort::ALL[dd.selected() as usize % Sort::ALL.len()]);
            refresh_grid(&a);
        });
    }
    {
        let a = app.clone();
        search.connect_search_changed(move |s| {
            *a.query.borrow_mut() = s.text().to_string();
            refresh_grid(&a);
        });
        let a = app.clone();
        search.connect_activate(move |_| {
            // Enter in search: jump to the first hit.
            if let Some(c) = a.grid.child_at_index(0) {
                a.grid.select_child(&c);
                c.grab_focus();
            }
        });
        let a = app.clone();
        search.connect_stop_search(move |s| {
            s.set_text("");
            a.grid.grab_focus();
        });
        search.set_key_capture_widget(Some(&window));
    }
    {
        let a = app.clone();
        grid.connect_selected_children_changed(move |fb| {
            let sel = fb.selected_children().first().map(|c| c.index());
            let id = sel.and_then(|i| a.shown.borrow().get(i as usize).cloned());
            if id.is_some() && *a.selected.borrow() != id {
                *a.selected.borrow_mut() = id;
                detail::show(&a);
            }
        });
        let a = app.clone();
        grid.connect_child_activated(move |_, c| {
            let id = a.shown.borrow().get(c.index() as usize).cloned();
            if let Some(id) = id {
                play_or_stop(&a, &id);
            }
        });
    }
    {
        let a = app.clone();
        scan_btn.connect_clicked(move |_| scan(&a, false));
        let a = app.clone();
        add_btn.connect_clicked(move |_| dialogs::add_game(&a));
        let a = app.clone();
        install_btn.connect_clicked(move |_| dialogs::install_flow(&a));
        let a = app.clone();
        prefs_btn.connect_clicked(move |_| settings::prefs_dialog(&a));
        let w = window.clone();
        close_btn.connect_clicked(move |_| w.close());
    }
    {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let a = app.clone();
        keys.connect_key_pressed(move |_, key, _, mods| {
            let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
            let in_text = gtk::prelude::GtkWindowExt::focus(&a.window).map(|f| f.is::<gtk::Text>() || f.is::<gtk::TextView>()).unwrap_or(false);
            match key {
                gdk::Key::F5 => {
                    scan(&a, false);
                    glib::Propagation::Stop
                }
                gdk::Key::comma if ctrl => {
                    settings::prefs_dialog(&a);
                    glib::Propagation::Stop
                }
                gdk::Key::slash if !in_text => {
                    a.search.grab_focus();
                    glib::Propagation::Stop
                }
                gdk::Key::Escape if !a.search.text().is_empty() => {
                    a.search.set_text("");
                    a.grid.grab_focus();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        window.add_controller(keys);
    }
    {
        let a = app.clone();
        window.connect_close_request(move |w| {
            let mut cfg = a.cfg.borrow_mut();
            cfg.ui.maximized = w.is_maximized();
            if !w.is_maximized() {
                cfg.ui.width = w.width();
                cfg.ui.height = w.height();
            }
            cfg.ui.view = a.filter.get().key().into();
            cfg.ui.sort = a.sort.get().key().into();
            let _ = cfg.save();
            glib::Propagation::Proceed
        });
    }

    // ── timers: running games + install jobs (each tick reads two tiny folders) ──
    {
        let a = app.clone();
        glib::timeout_add_seconds_local(2, move || {
            poll_running(&a);
            poll_jobs(&a);
            glib::ControlFlow::Continue
        });
    }

    refresh_grid(&app);
    update_now(&app);
    poll_jobs(&app);
    window.present();
    app.grid.grab_focus();
    // Pick up new games in the background on every start.
    scan(&app, true);
}

// ── grid ───────────────────────────────────────────────────────────────────────────────────

fn visible_games(app: &App) -> Vec<Game> {
    let lib = app.lib.borrow();
    let f = app.filter.get();
    let q = squash(&app.query.borrow());
    let qlower = app.query.borrow().to_lowercase();
    let mut v: Vec<Game> = lib
        .games
        .iter()
        .filter(|g| f.matches(g, &lib.stats(&g.id)))
        .filter(|g| q.is_empty() || squash(&g.name).contains(&q) || g.name.to_lowercase().contains(qlower.trim()))
        .cloned()
        .collect();
    let st = |g: &Game| lib.stats(&g.id);
    let sort = if f == Filter::Recent { Sort::LastPlayed } else { app.sort.get() };
    match sort {
        Sort::Name => v.sort_by_key(|g| g.name.to_lowercase()),
        Sort::LastPlayed => v.sort_by_key(|g| (-st(g).last_played, g.name.to_lowercase())),
        Sort::Playtime => v.sort_by_key(|g| (std::cmp::Reverse(st(g).total_secs()), g.name.to_lowercase())),
        Sort::Added => v.sort_by_key(|g| (-g.added, g.name.to_lowercase())),
    }
    // Favorites float to the top of every view.
    v.sort_by_key(|g| !g.favorite);
    v
}

pub fn refresh_grid(app: &AppRc) {
    let games = visible_games(app);
    app.grid.remove_all();
    app.lamps.borrow_mut().clear();
    let running = app.running.borrow().clone();
    {
        let lib = app.lib.borrow();
        for g in &games {
            let tile = tile(app, g, &lib.stats(&g.id), running.contains(&g.id));
            app.grid.insert(&tile, -1);
        }
    }
    *app.shown.borrow_mut() = games.iter().map(|g| g.id.clone()).collect();
    app.grid_title.set_text(app.filter.get().label());
    let q = app.query.borrow().clone();
    app.grid_count.set_text(&if q.is_empty() { format!("{} titles", games.len()) } else { format!("{} matching \u{201c}{}\u{201d}", games.len(), q) });
    app.grid_stack.set_visible_child_name(if games.is_empty() { "empty" } else { "grid" });

    for (f, b, count) in app.nav.borrow().iter() {
        if *f == app.filter.get() {
            b.add_css_class("active");
        } else {
            b.remove_css_class("active");
        }
        let lib = app.lib.borrow();
        let n = lib.games.iter().filter(|g| f.matches(g, &lib.stats(&g.id))).count();
        count.set_text(&n.to_string());
    }
    update_sys(app);

    // Keep the selection if it's still shown, else the first tile.
    let want = app.selected.borrow().clone();
    let idx = want.as_ref().and_then(|id| app.shown.borrow().iter().position(|x| x == id)).or(if games.is_empty() { None } else { Some(0) });
    match idx.and_then(|i| app.grid.child_at_index(i as i32)) {
        Some(c) => {
            app.grid.select_child(&c);
            let id = app.shown.borrow().get(c.index() as usize).cloned();
            if *app.selected.borrow() != id {
                *app.selected.borrow_mut() = id;
            }
            detail::show(app);
        }
        None => {
            if games.is_empty() && want.as_ref().map(|id| app.lib.borrow().get(id).is_none()).unwrap_or(true) {
                *app.selected.borrow_mut() = None;
            }
            detail::show(app);
        }
    }
}

fn initials(name: &str) -> String {
    name.split_whitespace().filter_map(|w| w.chars().find(|c| c.is_alphanumeric())).take(2).flat_map(|c| c.to_uppercase()).collect()
}

fn tile(app: &AppRc, g: &Game, st: &Stats, running: bool) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.set_halign(gtk::Align::Center);
    b.set_tooltip_text(Some(&g.name));

    let frame = gtk::Overlay::new();
    frame.add_css_class("cover-frame");
    let fallback = gtk::Box::new(gtk::Orientation::Vertical, 6);
    fallback.add_css_class("cover-fallback");
    fallback.set_size_request(TILE_W, TILE_H);
    let inner = gtk::Box::new(gtk::Orientation::Vertical, 6);
    inner.set_valign(gtk::Align::Center);
    inner.set_vexpand(true);
    // No cover but an app icon (desktop entries, native games): show the icon big.
    let icon = library::art(g, library::Art::Logo).filter(|_| library::art(g, library::Art::Cover).is_none());
    let mark = label(&initials(&g.name), "fallback-mark");
    mark.set_xalign(0.5);
    if let Some(p) = icon {
        let ip = gtk::Picture::new();
        ip.set_content_fit(gtk::ContentFit::Contain);
        ip.set_can_shrink(true);
        ip.set_size_request(96, 96);
        ip.set_halign(gtk::Align::Center);
        ip.set_valign(gtk::Align::Center);
        ip.set_vexpand(false);
        ip.set_visible(false);
        let (ip2, m2) = (ip.clone(), mark.clone());
        app.images.load(&ip, &p, 96, 96, move |ok| {
            ip2.set_visible(ok);
            m2.set_visible(!ok);
        });
        inner.append(&ip);
    }
    inner.append(&mark);
    let n = label(&g.name, "fallback-name");
    n.set_xalign(0.5);
    n.set_justify(gtk::Justification::Center);
    n.set_wrap(true);
    n.set_max_width_chars(14);
    inner.append(&n);
    fallback.append(&inner);
    frame.set_child(Some(&fallback));

    let pic = gtk::Picture::new();
    pic.set_content_fit(gtk::ContentFit::Cover);
    pic.set_can_shrink(true);
    pic.set_visible(false);
    frame.add_overlay(&pic);
    if let Some(path) = library::art(g, library::Art::Cover) {
        let p2 = pic.clone();
        app.images.load(&pic, &path, TILE_W * 2, TILE_H * 2, move |ok| p2.set_visible(ok));
    }

    let lamp = label("󰐊 RUNNING", "lamp run");
    lamp.set_halign(gtk::Align::Start);
    lamp.set_valign(gtk::Align::Start);
    lamp.set_margin_start(6);
    lamp.set_margin_top(6);
    lamp.set_visible(running);
    frame.add_overlay(&lamp);
    app.lamps.borrow_mut().insert(g.id.clone(), lamp);
    if g.favorite {
        let fav = label("󰓎", "lamp fav");
        fav.set_halign(gtk::Align::End);
        fav.set_valign(gtk::Align::Start);
        fav.set_margin_end(6);
        fav.set_margin_top(6);
        frame.add_overlay(&fav);
    }
    b.append(&frame);

    let name = label(&g.name, "tile-name");
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.set_max_width_chars(1);
    name.set_size_request(TILE_W, -1);
    b.append(&name);
    let secs = st.total_secs();
    let play = if secs == 0 { "NEW".to_string() } else { fmt_playtime(secs) };
    b.append(&label(&format!("{} · {}", runner_tag(g), play), "tile-meta"));
    b
}

pub fn runner_tag(g: &Game) -> &'static str {
    match g.runner {
        RunnerKind::Steam => "STEAM",
        RunnerKind::Proton => "PROTON",
        RunnerKind::Wine => "WINE",
        RunnerKind::Native => "LINUX",
        RunnerKind::Command => "APP",
    }
}

fn update_sys(app: &App) {
    let lib = app.lib.borrow();
    let total = lib.games.iter().filter(|g| !g.hidden).count();
    let secs: u64 = lib.games.iter().map(|g| lib.stats(&g.id).total_secs()).sum();
    let run = app.running.borrow().len();
    let mut s = format!("{total} GAMES · {} PLAYED", fmt_playtime(secs).to_uppercase());
    if run > 0 {
        s.push_str(&format!(" · {run} RUNNING"));
        app.sys.remove_css_class("sys");
        app.sys.add_css_class("sys-hot");
    } else {
        app.sys.remove_css_class("sys-hot");
        app.sys.add_css_class("sys");
    }
    app.sys.set_text(&s);
}

// ── running state ──────────────────────────────────────────────────────────────────────────

fn poll_running(app: &AppRc) {
    let now = library::running_ids();
    let before = app.running.borrow().clone();
    let mut starting_changed = false;
    app.starting.borrow_mut().retain(|id, t| {
        let keep = !now.contains(id) && t.elapsed().as_secs() < 25;
        starting_changed |= !keep;
        keep
    });
    if now == before && !starting_changed {
        return;
    }
    // Games that just stopped: their stats changed on disk.
    for id in before.iter().filter(|id| !now.contains(id)) {
        let st = library::load_stats(id);
        app.lib.borrow_mut().stats.insert(id.clone(), st);
    }
    *app.running.borrow_mut() = now.clone();
    for (id, lamp) in app.lamps.borrow().iter() {
        lamp.set_visible(now.contains(id));
    }
    update_now(app);
    update_sys(app);
    detail::show(app);
}

fn update_now(app: &AppRc) {
    while let Some(c) = app.now_box.first_child() {
        app.now_box.remove(&c);
    }
    let running = app.running.borrow().clone();
    app.now_title.set_visible(!running.is_empty());
    let lib = app.lib.borrow();
    for id in &running {
        let Some(g) = lib.get(id) else { continue };
        let card = gtk::Box::new(gtk::Orientation::Vertical, 2);
        card.add_css_class("now-card");
        let n = label(&g.name, "now-name");
        n.set_ellipsize(gtk::pango::EllipsizeMode::End);
        card.append(&n);
        let started = library::running(id).map(|r| r.started).unwrap_or(0);
        let mins = ((crate::model::now() - started).max(0) / 60) as u64;
        card.append(&label(&format!("󰐊 {} · since {}", runner_tag(g), if mins == 0 { "now".into() } else { format!("{mins} min") }), "now-meta"));
        let btn = gtk::Button::new();
        btn.add_css_class("flat");
        btn.set_child(Some(&card));
        let a = app.clone();
        let id2 = id.clone();
        btn.connect_clicked(move |_| select(&a, &id2));
        app.now_box.append(&btn);
    }
}

/// Select a game (switching to ALL if the current filter hides it).
pub fn select(app: &AppRc, id: &str) {
    *app.selected.borrow_mut() = Some(id.to_string());
    if !app.shown.borrow().iter().any(|x| x == id) {
        app.filter.set(if app.lib.borrow().get(id).map(|g| g.hidden).unwrap_or(false) { Filter::Hidden } else { Filter::All });
        app.search.set_text("");
        *app.query.borrow_mut() = String::new();
    }
    refresh_grid(app);
}

pub fn play_or_stop(app: &AppRc, id: &str) {
    if app.running.borrow().iter().any(|x| x == id) {
        let id2 = id.to_string();
        let a = app.clone();
        glib::spawn_future_local(async move {
            let res = gio::spawn_blocking(move || crate::launch::stop(&id2)).await;
            if let Ok(Err(e)) = res {
                toast(&a, &format!("Couldn't stop: {e}"));
            }
        });
        toast(app, "Stopping…");
        return;
    }
    if app.starting.borrow().contains_key(id) {
        return;
    }
    match crate::launch::spawn_supervisor(id) {
        Ok(()) => {
            app.starting.borrow_mut().insert(id.to_string(), Instant::now());
            detail::show(app);
        }
        Err(e) => toast(app, &format!("Couldn't start: {e}")),
    }
}

// ── scan ───────────────────────────────────────────────────────────────────────────────────

pub fn reload(app: &AppRc) {
    *app.lib.borrow_mut() = Library::load();
    refresh_grid(app);
}

pub fn scan(app: &AppRc, quiet: bool) {
    if app.scanning.replace(true) {
        return;
    }
    app.scan_btn.set_label("󰑐  SCANNING…");
    app.scan_btn.set_sensitive(false);
    let cfg = app.cfg.borrow().clone();
    let a = app.clone();
    glib::spawn_future_local(async move {
        let cfg2 = cfg.clone();
        let res = gio::spawn_blocking(move || {
            let (lib, rep) = library::scan_and_merge(&cfg2);
            (lib.games, rep.added)
        })
        .await;
        let (games, added) = res.unwrap_or_default();
        reload(&a);
        if !added.is_empty() {
            let names: Vec<String> = added.iter().filter_map(|id| a.lib.borrow().get(id).map(|g| g.name.clone())).take(3).collect();
            toast(&a, &format!("{} new: {}{}", added.len(), names.join(", "), if added.len() > 3 { "…" } else { "" }));
        } else if !quiet {
            toast(&a, "No new games found");
        }
        // Art second: lookups can take a while and the grid is already usable.
        if cfg.fetch_art {
            let got = gio::spawn_blocking(move || crate::art::fetch_missing(&games)).await.unwrap_or_default();
            if !got.is_empty() {
                refresh_grid(&a);
            }
        }
        a.scanning.set(false);
        a.scan_btn.set_label("󰑐  SCAN");
        a.scan_btn.set_sensitive(true);
    });
}

// ── install jobs strip ─────────────────────────────────────────────────────────────────────

fn poll_jobs(app: &AppRc) {
    let jobs = install::jobs();
    let sig: HashMap<String, String> = jobs.iter().map(|j| (j.id.clone(), format!("{}|{}", j.phase, j.message))).collect();
    if sig == *app.job_state.borrow() {
        return;
    }
    // Jobs that finished while we watched: reload + select the new game.
    let mut finished = None;
    for j in &jobs {
        let prev = app.job_state.borrow().get(&j.id).cloned();
        if j.phase == "done" && prev.map(|p| !p.starts_with("done")).unwrap_or(false) {
            finished = j.game_id.clone();
        }
    }
    *app.job_state.borrow_mut() = sig;
    while let Some(c) = app.jobs_box.first_child() {
        app.jobs_box.remove(&c);
    }
    for j in jobs.iter().take(4) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("job");
        if j.phase == "done" || j.phase == "failed" {
            row.add_css_class(&j.phase);
        }
        let phase = match j.phase.as_str() {
            "preparing" => "PREPARING",
            "installing" => "INSTALLING",
            "finding" => "FINDING GAME",
            "done" => "INSTALLED",
            "failed" => "FAILED",
            _ => "WORKING",
        };
        let ph = label(phase, "job-phase");
        ph.set_width_chars(13);
        row.append(&ph);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 1);
        text.set_hexpand(true);
        text.append(&label(&j.name, "job-name"));
        let msg = label(&j.message, "job-msg");
        msg.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.append(&msg);
        row.append(&text);
        let logb = button("LOG", "chip");
        logb.set_valign(gtk::Align::Center);
        let logp = crate::config::log_dir().join(format!("install-{}.log", crate::model::slugify(&j.name)));
        logb.connect_clicked(move |_| open_path(&logp));
        row.append(&logb);
        if let Some(gid) = j.game_id.clone().filter(|_| j.phase == "done") {
            let b = button("SHOW", "chip");
            b.set_valign(gtk::Align::Center);
            let a = app.clone();
            b.connect_clicked(move |_| select(&a, &gid));
            row.append(&b);
        }
        if j.phase == "done" || j.phase == "failed" {
            let x = button("󰅖", "close");
            x.set_valign(gtk::Align::Center);
            x.set_tooltip_text(Some("Dismiss finished installs"));
            let a = app.clone();
            x.connect_clicked(move |_| {
                install::clear_finished();
                poll_jobs(&a);
            });
            row.append(&x);
        }
        app.jobs_box.append(&row);
    }
    if let Some(gid) = finished {
        reload(app);
        select(app, &gid);
        let name = app.lib.borrow().get(&gid).map(|g| g.name.clone()).unwrap_or_default();
        toast(app, &format!("{name} is installed and in your library"));
    }
}

// ── small helpers ──────────────────────────────────────────────────────────────────────────

pub fn toast(app: &AppRc, msg: &str) {
    app.toast_label.set_text(msg);
    app.toast.set_reveal_child(true);
    let generation = app.toast_gen.get().wrapping_add(1);
    app.toast_gen.set(generation);
    let a = app.clone();
    glib::timeout_add_seconds_local_once(4, move || {
        if a.toast_gen.get() == generation {
            a.toast.set_reveal_child(false);
        }
    });
}

pub fn open_path(p: &std::path::Path) {
    let _ = std::process::Command::new("xdg-open").arg(p).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().map(|mut c| {
        std::thread::spawn(move || c.wait());
    });
}

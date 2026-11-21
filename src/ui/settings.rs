//! Per-game settings and global preferences. One options editor serves both: for a game every
//! field shows the inherited default ("Default (On)") and can be reset to it; for the global
//! defaults "Not set" means the built-in behaviour.

use super::{AppRc, button, dialogs, label};
use crate::config::{Config, expand, tilde};
use crate::launch::split_args;
use crate::model::{Gamescope, LaunchOptions, RunnerKind};
use crate::runners;
use gtk::{glib, prelude::*};
use gtk4 as gtk;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

type Opts = Rc<RefCell<LaunchOptions>>;
type Commit = Rc<dyn Fn(&dyn Fn(&mut Gamescope))>;
type AddRow = Rc<dyn Fn(&str, &str)>;
type Collector = Box<dyn Fn(&mut Config)>;

fn onoff(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "On",
        Some(false) => "Off",
        None => "Off",
    }
}

/// Shell-quote a command vector back into one line for editing.
fn join_cmd(v: &[String]) -> String {
    v.iter()
        .map(|a| {
            if a.is_empty() || a.contains(char::is_whitespace) || a.contains('"') {
                format!("'{}'", a.replace('\'', "'\\''"))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ── layout pieces ─────────────────────────────────────────────────────────────────────────

fn row(
    name: &str,
    help: &str,
    control: &impl IsA<gtk::Widget>,
    hint: Option<&gtk::Label>,
) -> gtk::Box {
    let r = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    r.add_css_class("opt");
    let left = gtk::Box::new(gtk::Orientation::Vertical, 2);
    left.set_hexpand(true);
    left.set_valign(gtk::Align::Center);
    left.append(&label(name, "opt-name"));
    if !help.is_empty() {
        let h = label(help, "opt-help");
        h.set_wrap(true);
        h.set_max_width_chars(46);
        left.append(&h);
    }
    r.append(&left);
    let right = gtk::Box::new(gtk::Orientation::Vertical, 3);
    right.set_valign(gtk::Align::Center);
    right.set_size_request(380, -1);
    right.set_hexpand(false);
    control.as_ref().set_hexpand(true);
    right.append(control);
    if let Some(h) = hint {
        h.set_xalign(1.0);
        right.append(h);
    }
    r.append(&right);
    r
}

fn group(page: &gtk::Box, title: &str) {
    page.append(&label(title, "group-title"));
}

fn page(title: &str, sub: &str) -> gtk::Box {
    let p = gtk::Box::new(gtk::Orientation::Vertical, 0);
    p.add_css_class("page");
    p.append(&label(title, "page-title"));
    if !sub.is_empty() {
        let s = label(sub, "page-sub");
        s.set_wrap(true);
        p.append(&s);
    }
    p
}

fn dd(items: &[String]) -> gtk::DropDown {
    gtk::DropDown::from_strings(&items.iter().map(String::as_str).collect::<Vec<_>>())
}

fn entry(text: &str, placeholder: &str) -> gtk::Entry {
    let e = gtk::Entry::new();
    e.set_text(text);
    e.set_placeholder_text(Some(placeholder));
    e
}

fn entry_browse(win: &gtk::Window, e: &gtk::Entry, folder: bool, title: &'static str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    e.set_hexpand(true);
    b.append(e);
    let br = button("󰉋", "");
    br.set_tooltip_text(Some("Browse"));
    let (w, e2) = (win.clone(), e.clone());
    br.connect_clicked(move |_| {
        let cur = expand(e2.text().trim());
        let start = if cur.is_dir() {
            Some(cur.clone())
        } else {
            cur.parent().map(|p| p.to_path_buf())
        };
        let e3 = e2.clone();
        if folder {
            dialogs::pick_folder(&w, title, start, move |p| e3.set_text(&tilde(&p)));
        } else {
            let d = gtk::FileDialog::builder().title(title).modal(true).build();
            if let Some(s) = start.filter(|p| p.is_dir()) {
                d.set_initial_folder(Some(&gtk::gio::File::for_path(s)));
            }
            d.open(Some(&w), gtk::gio::Cancellable::NONE, move |r| {
                if let Some(p) = r.ok().and_then(|f| f.path()) {
                    e3.set_text(&p.to_string_lossy());
                }
            });
        }
    });
    b.append(&br);
    b
}

fn path_or_none(s: &str) -> Option<PathBuf> {
    let s = s.trim();
    (!s.is_empty()).then(|| expand(s))
}

// ── options editor ────────────────────────────────────────────────────────────────────────

struct Ed {
    opts: Opts,
    base: LaunchOptions,
    global: bool,
}

impl Ed {
    fn hint(&self, text: String, set: bool) -> gtk::Label {
        let l = label(&text, "opt-inherit");
        if set {
            l.add_css_class("set");
        }
        l
    }

    /// Default / On / Off.
    fn tri(
        &self,
        name: &str,
        help: &str,
        get: fn(&LaunchOptions) -> Option<bool>,
        set: fn(&mut LaunchOptions, Option<bool>),
    ) -> gtk::Box {
        let first = if self.global {
            "Not set".to_string()
        } else {
            format!("Default ({})", onoff(get(&self.base)))
        };
        let d = dd(&[first, "On".into(), "Off".into()]);
        d.set_selected(match get(&self.opts.borrow()) {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        });
        let o = self.opts.clone();
        d.connect_selected_notify(move |d| {
            set(
                &mut o.borrow_mut(),
                match d.selected() {
                    0 => None,
                    1 => Some(true),
                    _ => Some(false),
                },
            )
        });
        row(name, help, &d, None)
    }

    fn text(
        &self,
        name: &str,
        help: &str,
        get: fn(&LaunchOptions) -> Option<String>,
        set: fn(&mut LaunchOptions, Option<String>),
    ) -> gtk::Box {
        let base = get(&self.base).filter(|s| !s.is_empty());
        let ph = match (&base, self.global) {
            (Some(b), false) => format!("default: {b}"),
            _ => "not set".to_string(),
        };
        let e = entry(&get(&self.opts.borrow()).unwrap_or_default(), &ph);
        let o = self.opts.clone();
        e.connect_changed(move |e| {
            let t = e.text().to_string();
            set(&mut o.borrow_mut(), (!t.trim().is_empty()).then_some(t));
        });
        row(name, help, &e, None)
    }

    fn int(
        &self,
        name: &str,
        help: &str,
        get: fn(&LaunchOptions) -> Option<i32>,
        set: fn(&mut LaunchOptions, Option<i32>),
    ) -> gtk::Box {
        let ph = match (get(&self.base), self.global) {
            (Some(b), false) => format!("default: {b}"),
            _ => "not set".to_string(),
        };
        let e = entry(
            &get(&self.opts.borrow())
                .map(|v| v.to_string())
                .unwrap_or_default(),
            &ph,
        );
        let o = self.opts.clone();
        e.connect_changed(move |e| {
            let v = e.text().trim().parse::<i32>().ok();
            set(&mut o.borrow_mut(), v);
            if e.text().trim().is_empty() || v.is_some() {
                e.remove_css_class("error");
            } else {
                e.add_css_class("error");
            }
        });
        row(name, help, &e, None)
    }

    /// Default + fixed values (value, label).
    fn choice(
        &self,
        name: &str,
        help: &str,
        values: Vec<(String, String)>,
        get: fn(&LaunchOptions) -> Option<String>,
        set: fn(&mut LaunchOptions, Option<String>),
    ) -> gtk::Box {
        let base = get(&self.base);
        let first = if self.global {
            "Not set".to_string()
        } else {
            let bl = base.as_ref().and_then(|b| {
                values
                    .iter()
                    .find(|(v, _)| v == b)
                    .map(|(_, l)| l.clone())
                    .or(Some(b.clone()))
            });
            format!("Default ({})", bl.unwrap_or_else(|| "auto".into()))
        };
        let mut labels = vec![first];
        labels.extend(values.iter().map(|(_, l)| l.clone()));
        let cur = get(&self.opts.borrow());
        // A saved value that isn't in the list any more (deleted Proton build): keep it visible.
        let mut values = values;
        if let Some(c) = cur
            .as_ref()
            .filter(|c| !values.iter().any(|(v, _)| v == *c))
        {
            values.push((c.clone(), format!("{c} (missing)")));
            labels.push(format!("{c} (missing)"));
        }
        let d = dd(&labels);
        d.set_selected(
            cur.and_then(|c| values.iter().position(|(v, _)| *v == c))
                .map(|i| i as u32 + 1)
                .unwrap_or(0),
        );
        let o = self.opts.clone();
        d.connect_selected_notify(move |d| {
            let i = d.selected() as usize;
            set(
                &mut o.borrow_mut(),
                if i == 0 {
                    None
                } else {
                    values.get(i - 1).map(|(v, _)| v.clone())
                },
            );
        });
        row(name, help, &d, None)
    }

    fn gamescope(&self, page: &gtk::Box) {
        let gs = Rc::new(RefCell::new(
            self.opts
                .borrow()
                .gamescope_opts
                .clone()
                .or_else(|| self.base.gamescope_opts.clone())
                .unwrap_or_default(),
        ));
        let loading = Rc::new(Cell::new(false));
        let status = self.hint(String::new(), false);
        let set_status = {
            let (status, o, global) = (status.clone(), self.opts.clone(), self.global);
            move || {
                let own = o.borrow().gamescope_opts.is_some();
                status.set_text(if own {
                    "custom for this game"
                } else if global {
                    "not set"
                } else {
                    "using the defaults"
                });
                if own {
                    status.add_css_class("set");
                } else {
                    status.remove_css_class("set");
                }
            }
        };
        set_status();
        let commit: Commit = {
            let (gs, o, loading, set_status) = (
                gs.clone(),
                self.opts.clone(),
                loading.clone(),
                set_status.clone(),
            );
            Rc::new(move |f: &dyn Fn(&mut Gamescope)| {
                if loading.get() {
                    return;
                }
                f(&mut gs.borrow_mut());
                o.borrow_mut().gamescope_opts = Some(gs.borrow().clone());
                set_status();
            })
        };
        let res = |w: u32, h: u32| {
            if w > 0 && h > 0 {
                format!("{w}x{h}")
            } else {
                String::new()
            }
        };
        let parse = |s: &str| -> (u32, u32) {
            s.trim()
                .split_once(['x', 'X'])
                .and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)))
                .unwrap_or((0, 0))
        };
        let g0 = gs.borrow().clone();
        let game_res = entry(&res(g0.width, g0.height), "native, e.g. 1920x1080");
        let out_res = entry(&res(g0.out_width, g0.out_height), "monitor, e.g. 2560x1440");
        let refresh = entry(
            &if g0.refresh > 0 {
                g0.refresh.to_string()
            } else {
                String::new()
            },
            "e.g. 240",
        );
        let filters = ["none", "fsr", "nis", "linear", "nearest", "pixel"];
        let filter = dd(&filters.iter().map(|s| s.to_uppercase()).collect::<Vec<_>>());
        filter.set_selected(filters.iter().position(|f| *f == g0.filter).unwrap_or(0) as u32);
        let fullscreen = gtk::Switch::new();
        let grab = gtk::Switch::new();
        let hdr = gtk::Switch::new();
        for s in [&fullscreen, &grab, &hdr] {
            s.set_halign(gtk::Align::End);
        }
        fullscreen.set_active(g0.fullscreen);
        grab.set_active(g0.grab_cursor);
        hdr.set_active(g0.hdr);
        let extra = entry(&g0.extra, "more gamescope flags");

        {
            let c = commit.clone();
            game_res.connect_changed(move |e| {
                let (w, h) = parse(&e.text());
                c(&|g| {
                    g.width = w;
                    g.height = h;
                })
            });
            let c = commit.clone();
            out_res.connect_changed(move |e| {
                let (w, h) = parse(&e.text());
                c(&|g| {
                    g.out_width = w;
                    g.out_height = h;
                })
            });
            let c = commit.clone();
            refresh.connect_changed(move |e| {
                let r = e.text().trim().parse().unwrap_or(0);
                c(&|g| g.refresh = r)
            });
            let c = commit.clone();
            filter.connect_selected_notify(move |d| {
                let f = if d.selected() == 0 {
                    String::new()
                } else {
                    filters[d.selected() as usize].to_string()
                };
                c(&|g| g.filter = f.clone())
            });
            let c = commit.clone();
            fullscreen.connect_active_notify(move |s| {
                let v = s.is_active();
                c(&|g| g.fullscreen = v)
            });
            let c = commit.clone();
            grab.connect_active_notify(move |s| {
                let v = s.is_active();
                c(&|g| g.grab_cursor = v)
            });
            let c = commit.clone();
            hdr.connect_active_notify(move |s| {
                let v = s.is_active();
                c(&|g| g.hdr = v)
            });
            let c = commit.clone();
            extra.connect_changed(move |e| {
                let t = e.text().to_string();
                c(&|g| g.extra = t.clone())
            });
        }

        let reset = button(if self.global { "CLEAR" } else { "USE DEFAULTS" }, "chip");
        {
            let (o, base, gs, loading, set_status) = (
                self.opts.clone(),
                self.base.gamescope_opts.clone(),
                gs.clone(),
                loading.clone(),
                set_status.clone(),
            );
            let (game_res, out_res, refresh, filter, fullscreen, grab, hdr, extra) = (
                game_res.clone(),
                out_res.clone(),
                refresh.clone(),
                filter.clone(),
                fullscreen.clone(),
                grab.clone(),
                hdr.clone(),
                extra.clone(),
            );
            reset.connect_clicked(move |_| {
                o.borrow_mut().gamescope_opts = None;
                let g = base.clone().unwrap_or_default();
                *gs.borrow_mut() = g.clone();
                loading.set(true);
                game_res.set_text(&res(g.width, g.height));
                out_res.set_text(&res(g.out_width, g.out_height));
                refresh.set_text(&if g.refresh > 0 {
                    g.refresh.to_string()
                } else {
                    String::new()
                });
                filter
                    .set_selected(filters.iter().position(|f| *f == g.filter).unwrap_or(0) as u32);
                fullscreen.set_active(g.fullscreen);
                grab.set_active(g.grab_cursor);
                hdr.set_active(g.hdr);
                extra.set_text(&g.extra);
                loading.set(false);
                set_status();
            });
        }
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let gt = label("GAMESCOPE OPTIONS", "group-title");
        gt.set_hexpand(true);
        head.append(&gt);
        status.set_valign(gtk::Align::Center);
        head.append(&status);
        reset.set_valign(gtk::Align::Center);
        head.append(&reset);
        page.append(&head);
        page.append(&row(
            "Game resolution",
            "What the game renders at (-w/-h).",
            &game_res,
            None,
        ));
        page.append(&row(
            "Output resolution",
            "What gamescope shows on the monitor (-W/-H).",
            &out_res,
            None,
        ));
        page.append(&row(
            "Refresh rate",
            "Nested refresh rate (-r).",
            &refresh,
            None,
        ));
        page.append(&row(
            "Upscaling filter",
            "How the game image is scaled up (-F).",
            &filter,
            None,
        ));
        page.append(&row("Fullscreen", "", &fullscreen, None));
        page.append(&row(
            "Grab cursor",
            "Keep the mouse inside the game (--force-grab-cursor).",
            &grab,
            None,
        ));
        page.append(&row("HDR", "--hdr-enabled", &hdr, None));
        page.append(&row(
            "Extra flags",
            "Added before the -- separator.",
            &extra,
            None,
        ));
    }

    fn env(&self, page: &gtk::Box) {
        let rows: Rc<RefCell<Vec<(gtk::Box, gtk::Entry, gtk::Entry)>>> =
            Rc::new(RefCell::new(vec![]));
        let list = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let sync: Rc<dyn Fn()> = {
            let (rows, o) = (rows.clone(), self.opts.clone());
            Rc::new(move || {
                let mut m = BTreeMap::new();
                for (_, k, v) in rows.borrow().iter() {
                    let k = k.text().trim().to_string();
                    if !k.is_empty() {
                        m.insert(k, v.text().to_string());
                    }
                }
                o.borrow_mut().env = m;
            })
        };
        let add_row: AddRow = {
            let (rows, list, sync) = (rows.clone(), list.clone(), sync.clone());
            Rc::new(move |k: &str, v: &str| {
                let r = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                let ke = entry(k, "NAME");
                ke.set_width_chars(26);
                let ve = entry(v, "value");
                ve.set_hexpand(true);
                let x = button("󰅖", "close");
                r.append(&ke);
                r.append(&ve);
                r.append(&x);
                list.append(&r);
                let s = sync.clone();
                ke.connect_changed(move |_| s());
                let s = sync.clone();
                ve.connect_changed(move |_| s());
                let (rows2, list2, r2, s) = (rows.clone(), list.clone(), r.clone(), sync.clone());
                x.connect_clicked(move |_| {
                    list2.remove(&r2);
                    rows2.borrow_mut().retain(|(b, _, _)| b != &r2);
                    s();
                });
                rows.borrow_mut().push((r, ke, ve));
            })
        };
        for (k, v) in self.opts.borrow().env.clone() {
            add_row(&k, &v);
        }
        page.append(&list);
        let add = button("󰐕  ADD VARIABLE", "chip");
        add.set_halign(gtk::Align::Start);
        add.set_margin_top(8);
        {
            let ar = add_row.clone();
            add.connect_clicked(move |_| ar("", ""));
        }
        page.append(&add);
        if !self.global && !self.base.env.is_empty() {
            group(page, "INHERITED FROM DEFAULTS");
            for (k, v) in &self.base.env {
                page.append(&label(&format!("{k}={v}"), "v mono"));
            }
            page.append(&label(
                "A variable with the same name here wins.",
                "opt-help",
            ));
        }
    }
}

/// Pages for the options editor plus the widgets that depend on the runner.
struct OptPages {
    pages: Vec<(&'static str, gtk::Box)>,
    windows_only: Vec<gtk::Widget>,
    proton_only: Vec<gtk::Widget>,
    wine_only: Vec<gtk::Widget>,
}

impl OptPages {
    fn set_runner(&self, r: Option<RunnerKind>) {
        // None = global defaults: show everything.
        let win = r.map(|r| r.is_windows()).unwrap_or(true);
        for w in &self.windows_only {
            w.set_visible(win);
        }
        for w in &self.proton_only {
            w.set_visible(r.map(|r| r == RunnerKind::Proton).unwrap_or(true));
        }
        for w in &self.wine_only {
            w.set_visible(r.map(|r| r == RunnerKind::Wine).unwrap_or(true));
        }
    }
}

fn option_pages(ed: &Ed, cfg: &Config) -> OptPages {
    let mut windows_only: Vec<gtk::Widget> = vec![];
    let mut proton_only: Vec<gtk::Widget> = vec![];
    let mut wine_only: Vec<gtk::Widget> = vec![];

    // RUNNER
    let p_runner = page(
        "Runner",
        "Which Proton or Wine build runs the game, and Wine-level switches.",
    );
    let mut protons: Vec<(String, String)> = vec![(
        "latest".into(),
        match runners::find_proton("latest") {
            Some(b) => format!("Latest ({})", b.name),
            None => "Latest".into(),
        },
    )];
    protons.extend(
        runners::protons_with(Some(cfg))
            .into_iter()
            .map(|b| (b.name.clone(), format!("{}  · {}", b.name, b.origin))),
    );
    let r = ed.choice(
        "Proton build",
        "GE-Proton is recommended. Latest follows new installs.",
        protons,
        |o| o.proton.clone(),
        |o, v| o.proton = v,
    );
    proton_only.push(r.clone().upcast());
    p_runner.append(&r);
    let wines: Vec<(String, String)> = runners::wines()
        .into_iter()
        .map(|b| (b.name.clone(), format!("{}  · {}", b.name, b.origin)))
        .collect();
    let r = ed.choice(
        "Wine build",
        "Plain Wine (no Proton extras).",
        wines,
        |o| o.wine.clone(),
        |o, v| o.wine = v,
    );
    wine_only.push(r.clone().upcast());
    p_runner.append(&r);
    let syncs = [
        ("default", "Auto"),
        ("ntsync", "NTSync"),
        ("fsync", "Fsync"),
        ("esync", "Esync"),
        ("off", "Off (debug)"),
    ]
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .to_vec();
    p_runner.append(&ed.choice(
        "Sync",
        "Thread synchronisation. Auto picks NTSync when the kernel has it.",
        syncs,
        |o| o.sync.clone(),
        |o, v| o.sync = v,
    ));
    let r = ed.tri(
        "FSR 4 upgrade",
        "GE-Proton swaps FSR 3.1 for FSR 4 on RDNA 4 (PROTON_FSR4_UPGRADE).",
        |o| o.fsr4,
        |o, v| o.fsr4 = v,
    );
    proton_only.push(r.clone().upcast());
    p_runner.append(&r);
    let r = ed.tri(
        "Wine Wayland",
        "Native Wayland window instead of XWayland (PROTON_ENABLE_WAYLAND).",
        |o| o.wine_wayland,
        |o, v| o.wine_wayland = v,
    );
    proton_only.push(r.clone().upcast());
    p_runner.append(&r);
    let r = ed.tri(
        "WineD3D (OpenGL)",
        "Use WineD3D instead of DXVK, for old games that break on Vulkan.",
        |o| o.wined3d,
        |o, v| o.wined3d = v,
    );
    proton_only.push(r.clone().upcast());
    p_runner.append(&r);
    p_runner.append(&ed.text(
        "DXVK HUD",
        "DXVK's own overlay, e.g. fps,compiler.",
        |o| o.dxvk_hud.clone(),
        |o, v| o.dxvk_hud = v,
    ));

    // DISPLAY
    let p_display = page("Display", "Overlay and the gamescope micro-compositor.");
    p_display.append(&ed.tri(
        "MangoHud",
        "FPS / frame-time overlay.",
        |o| o.mangohud,
        |o, v| o.mangohud = v,
    ));
    p_display.append(&ed.text(
        "MangoHud config",
        "Extra settings on top of your MangoHud.conf, e.g. fps_limit=141.",
        |o| o.mangohud_config.clone(),
        |o, v| o.mangohud_config = v,
    ));
    p_display.append(&ed.tri(
        "Gamescope",
        "Run inside gamescope (upscaling, fps limits, fixed resolution).",
        |o| o.gamescope,
        |o, v| o.gamescope = v,
    ));
    ed.gamescope(&p_display);

    // SYSTEM
    let p_system = page("System", "CPU, priority and how the process is wrapped.");
    p_system.append(&ed.tri(
        "GameMode",
        "Feral gamemoderun: CPU governor + priority while playing.",
        |o| o.gamemode,
        |o, v| o.gamemode = v,
    ));
    let wrap_help = if cfg.wrapper.is_empty() {
        "No wrapper set in Preferences → Launch.".to_string()
    } else {
        format!("Runs through: {}", join_cmd(&cfg.wrapper))
    };
    p_system.append(&ed.tri(
        "Launch wrapper",
        &wrap_help,
        |o| o.use_wrapper,
        |o, v| o.use_wrapper = v,
    ));
    p_system.append(&ed.text(
        "CPU cores",
        "Pin to these CPUs (taskset list), e.g. 0-7 or 0,2,4.",
        |o| o.cpu_set.clone(),
        |o, v| o.cpu_set = v,
    ));
    p_system.append(&ed.int(
        "Nice",
        "Process priority, -20 (highest) to 19. Below 0 needs permission.",
        |o| o.nice,
        |o, v| o.nice = v,
    ));

    // SCRIPTS
    let p_scripts = page(
        "Scripts",
        "Shell commands around the game. They get the game's environment.",
    );
    p_scripts.append(&ed.text(
        "Before launch",
        "Runs before the game starts.",
        |o| o.prelaunch.clone(),
        |o, v| o.prelaunch = v,
    ));
    p_scripts.append(&ed.tri(
        "Wait for it",
        "Start the game only after the pre-launch command exits.",
        |o| o.prelaunch_wait,
        |o, v| o.prelaunch_wait = v,
    ));
    p_scripts.append(&ed.text(
        "After exit",
        "Runs once the game and everything it started has closed.",
        |o| o.postexit.clone(),
        |o, v| o.postexit = v,
    ));
    let r = ed.tri(
        "Stop prefix on exit",
        "wineserver -k after the game, so leftover launchers don't linger.",
        |o| o.kill_prefix_on_exit,
        |o, v| o.kill_prefix_on_exit = v,
    );
    windows_only.push(r.clone().upcast());
    p_scripts.append(&r);
    p_scripts.append(&ed.tri(
        "Launch log",
        "Keep the game's output in the log folder.",
        |o| o.log,
        |o, v| o.log = v,
    ));

    // ENVIRONMENT
    let p_env = page(
        "Environment",
        "Extra variables for the game. These win over everything hearth sets.",
    );
    ed.env(&p_env);

    windows_only.push(p_runner.clone().upcast());
    OptPages {
        pages: vec![
            ("RUNNER", p_runner),
            ("DISPLAY", p_display),
            ("SYSTEM", p_system),
            ("SCRIPTS", p_scripts),
            ("ENVIRONMENT", p_env),
        ],
        windows_only,
        proton_only,
        wine_only,
    }
}

/// Left section list + stack. Returns the root and a fn to show/hide a section button with its page.
fn sectioned(pages: &[(&str, gtk::Box)]) -> (gtk::Box, Vec<gtk::Button>) {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.set_vexpand(true);
    let side = gtk::Box::new(gtk::Orientation::Vertical, 0);
    side.add_css_class("sections");
    let stack = gtk::Stack::new();
    stack.set_hexpand(true);
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_transition_duration(90);
    let mut buttons: Vec<gtk::Button> = vec![];
    let mut scrolls: Vec<gtk::ScrolledWindow> = vec![];
    for (i, (name, p)) in pages.iter().enumerate() {
        let sc = gtk::ScrolledWindow::new();
        sc.set_hscrollbar_policy(gtk::PolicyType::Never);
        sc.set_child(Some(p));
        stack.add_named(&sc, Some(name));
        scrolls.push(sc);
        let b = gtk::Button::new();
        b.add_css_class("nav");
        let r = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        r.append(&label(&format!("{:02}", i + 1), "nav-idx"));
        r.append(&label(name, "nav-name"));
        b.set_child(Some(&r));
        side.append(&b);
        buttons.push(b);
        // A page hidden by the runner hides its button too.
        let b2 = buttons[i].clone();
        p.connect_visible_notify(move |p| b2.set_visible(p.is_visible()));
        buttons[i].set_visible(p.is_visible());
    }
    for (i, b) in buttons.iter().enumerate() {
        let (stack, all, name) = (stack.clone(), buttons.clone(), pages[i].0.to_string());
        b.connect_clicked(move |me| {
            stack.set_visible_child_name(&name);
            for x in &all {
                x.remove_css_class("active");
            }
            me.add_css_class("active");
        });
    }
    if let Some(b) = buttons.first() {
        b.add_css_class("active");
    }
    let sections = pages
        .iter()
        .zip(scrolls)
        .zip(buttons.iter())
        .map(|(((name, p), scroll), b)| super::settings_search::Section {
            name: name.to_string(),
            page: p.clone(),
            scroll,
            button: b.clone(),
        })
        .collect();
    super::settings_search::attach(&side, &stack, &root, sections);
    root.append(&side);
    root.append(&stack);
    (root, buttons)
}

// ── per-game dialog ───────────────────────────────────────────────────────────────────────

pub fn game_dialog(app: &AppRc, id: &str) {
    let Some(g0) = app.lib.borrow().get(id).cloned() else {
        return;
    };
    let cfg = app.cfg.borrow().clone();
    let (win, body, foot) = dialogs::modal(app, "GAME SETTINGS", &g0.name, 900, 700);
    body.remove_css_class("dialog-body");
    let game = Rc::new(RefCell::new(g0.clone()));
    let opts: Opts = Rc::new(RefCell::new(g0.options.clone()));
    let ed = Ed {
        opts: opts.clone(),
        base: cfg.defaults.clone(),
        global: false,
    };

    // GAME page
    let p_game = page("Game", "What to run and where it lives.");
    let name = entry(&g0.name, "Name");
    {
        let gm = game.clone();
        name.connect_changed(move |e| gm.borrow_mut().name = e.text().to_string());
    }
    p_game.append(&row("Name", "", &name, None));
    let runner_dd = dd(&RunnerKind::ALL
        .iter()
        .map(|r| r.label().to_string())
        .collect::<Vec<_>>());
    runner_dd.set_selected(
        RunnerKind::ALL
            .iter()
            .position(|r| *r == g0.runner)
            .unwrap_or(0) as u32,
    );
    p_game.append(&row(
        "Runner",
        "Proton for Windows games, Linux for native ones.",
        &runner_dd,
        None,
    ));

    let exe = entry(
        &g0.exe
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        "/path/to/game.exe",
    );
    {
        let gm = game.clone();
        exe.connect_changed(move |e| gm.borrow_mut().exe = path_or_none(&e.text()));
    }
    let exe_row = row(
        "Executable",
        "The .exe, AppImage, binary or script.",
        &entry_browse(&win, &exe, false, "Pick the executable"),
        None,
    );
    p_game.append(&exe_row);
    let args = entry(&g0.args, "none");
    {
        let gm = game.clone();
        args.connect_changed(move |e| gm.borrow_mut().args = e.text().to_string());
    }
    let args_row = row(
        "Arguments",
        "Passed to the game, shell-style quoting.",
        &args,
        None,
    );
    p_game.append(&args_row);
    let cmd = entry(&join_cmd(&g0.command), "program --flag");
    {
        let gm = game.clone();
        cmd.connect_changed(move |e| gm.borrow_mut().command = split_args(&e.text()));
    }
    let cmd_row = row(
        "Command",
        "Full command line (from the app's .desktop entry).",
        &cmd,
        None,
    );
    p_game.append(&cmd_row);
    let wd = entry(
        &g0.workdir.as_ref().map(|p| tilde(p)).unwrap_or_default(),
        "the executable's folder",
    );
    {
        let gm = game.clone();
        wd.connect_changed(move |e| gm.borrow_mut().workdir = path_or_none(&e.text()));
    }
    let wd_row = row(
        "Working folder",
        "",
        &entry_browse(&win, &wd, true, "Working folder"),
        None,
    );
    p_game.append(&wd_row);
    let pfx = entry(
        &g0.prefix.as_ref().map(|p| tilde(p)).unwrap_or_default(),
        &format!("new: {}", tilde(&crate::launch::default_prefix(&cfg, &g0))),
    );
    {
        let gm = game.clone();
        pfx.connect_changed(move |e| gm.borrow_mut().prefix = path_or_none(&e.text()));
    }
    let pfx_row = row(
        "Wine prefix",
        "Each game can have its own; shared prefixes work too.",
        &entry_browse(&win, &pfx, true, "Wine prefix folder"),
        None,
    );
    p_game.append(&pfx_row);
    let appid = entry(
        &g0.steam_appid.map(|a| a.to_string()).unwrap_or_default(),
        "none",
    );
    {
        let gm = game.clone();
        appid.connect_changed(move |e| gm.borrow_mut().steam_appid = e.text().trim().parse().ok());
    }
    let appid_row = row(
        "Steam app id",
        "Steam games: the app to start. Others: art lookup and umu protonfixes.",
        &appid,
        None,
    );
    p_game.append(&appid_row);
    let steam_note = label(
        "Steam starts this game, so Steam's own Properties (launch options, Proton version) apply. hearth keeps the name, art, favorite and hidden state.",
        "note",
    );
    steam_note.set_wrap(true);
    steam_note.set_margin_top(12);
    p_game.append(&steam_note);

    let op = option_pages(&ed, &cfg);
    let mut pages: Vec<(&str, gtk::Box)> = vec![("GAME", p_game.clone())];
    pages.extend(op.pages.iter().cloned());
    let (root, _btns) = sectioned(&pages);
    body.append(&root);

    let op = Rc::new(op);
    let apply_runner = {
        let (op, exe_row, args_row, cmd_row, wd_row, pfx_row, steam_note) = (
            op.clone(),
            exe_row.clone(),
            args_row.clone(),
            cmd_row.clone(),
            wd_row.clone(),
            pfx_row.clone(),
            steam_note.clone(),
        );
        move |r: RunnerKind| {
            let steam = r == RunnerKind::Steam;
            op.set_runner(Some(r));
            for (_, p) in op.pages.iter() {
                if steam {
                    p.set_visible(false);
                }
            }
            if !steam {
                // RUNNER page visibility is handled by set_runner; the rest come back.
                for (n, p) in op.pages.iter() {
                    if *n != "RUNNER" {
                        p.set_visible(true);
                    }
                }
            }
            exe_row.set_visible(matches!(
                r,
                RunnerKind::Proton | RunnerKind::Wine | RunnerKind::Native
            ));
            args_row.set_visible(!steam);
            cmd_row.set_visible(r == RunnerKind::Command);
            wd_row.set_visible(!steam);
            pfx_row.set_visible(r.is_windows());
            steam_note.set_visible(steam);
        }
    };
    apply_runner(g0.runner);
    {
        let gm = game.clone();
        let ar = apply_runner.clone();
        runner_dd.connect_selected_notify(move |d| {
            let r = RunnerKind::ALL[d.selected() as usize % RunnerKind::ALL.len()];
            gm.borrow_mut().runner = r;
            ar(r);
        });
    }
    let _ = appid_row;

    let cmdb = button("󰆍  COMMAND", "");
    cmdb.set_tooltip_text(Some("Preview the launch command with these settings"));
    let cancel = button("CANCEL", "");
    let save = button("SAVE", "accent");
    foot.prepend(&cmdb);
    foot.append(&cancel);
    foot.append(&save);
    {
        let (gm, opts, win2, cfg) = (game.clone(), opts.clone(), win.clone(), cfg.clone());
        cmdb.connect_clicked(move |_| {
            let mut g = gm.borrow().clone();
            g.options = opts.borrow().clone();
            let text = match crate::launch::plan(&g, &cfg) {
                Ok(p) => p.render(),
                Err(e) => format!("Can't build the command: {e}"),
            };
            let pop = gtk::Popover::new();
            let l = label(&text, "cmd");
            l.set_wrap(true);
            l.set_wrap_mode(gtk::pango::WrapMode::Char);
            l.set_selectable(true);
            l.set_max_width_chars(90);
            pop.set_child(Some(&l));
            pop.set_parent(&win2.child().unwrap());
            pop.set_position(gtk::PositionType::Top);
            pop.popup();
            pop.connect_closed(|p| p.unparent());
        });
    }
    {
        let w = win.clone();
        cancel.connect_clicked(move |_| w.close());
    }
    {
        let (a, w, gm, opts, old_name) = (
            app.clone(),
            win.clone(),
            game.clone(),
            opts.clone(),
            g0.name.clone(),
        );
        save.connect_clicked(move |_| {
            let mut g = gm.borrow().clone();
            g.name = g.name.trim().to_string();
            if g.name.is_empty() {
                super::toast(&a, "A game needs a name");
                return;
            }
            g.options = opts.borrow().clone();
            if g.name != old_name {
                crate::art::reset_lookup(&g.id);
            }
            let id = g.id.clone();
            match a.lib.borrow_mut().upsert(g) {
                Ok(()) => {}
                Err(e) => {
                    super::toast(&a, &format!("Couldn't save: {e}"));
                    return;
                }
            }
            *a.selected.borrow_mut() = Some(id);
            super::refresh_grid(&a);
            w.close();
        });
    }
    win.present();
}

// ── preferences ───────────────────────────────────────────────────────────────────────────

fn lines_view(lines: &[String]) -> gtk::TextView {
    let tv = gtk::TextView::new();
    tv.buffer().set_text(&lines.join("\n"));
    tv.set_monospace(true);
    tv.set_size_request(-1, 64);
    tv.set_wrap_mode(gtk::WrapMode::None);
    tv
}

fn view_lines(tv: &gtk::TextView) -> Vec<String> {
    let b = tv.buffer();
    b.text(&b.start_iter(), &b.end_iter(), false)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

pub fn prefs_dialog(app: &AppRc) {
    let cfg0 = app.cfg.borrow().clone();
    let (win, body, foot) = dialogs::modal(app, "PREFERENCES", &tilde(&Config::path()), 940, 720);
    body.remove_css_class("dialog-body");
    let cfg = Rc::new(RefCell::new(cfg0.clone()));

    // Every field below writes into `cfg` on save via these collectors.
    let mut collect: Vec<Collector> = vec![];
    let sw = |on: bool| {
        let s = gtk::Switch::new();
        s.set_active(on);
        s.set_halign(gtk::Align::End);
        s.set_hexpand(false);
        s
    };

    // LIBRARY
    let p_lib = page(
        "Library",
        "Where hearth looks for games. One folder per line.",
    );
    let game_dirs = lines_view(&cfg0.game_dirs);
    p_lib.append(&row(
        "Game folders",
        "Each sub-folder is a game: repack installs, portable and native games.",
        &game_dirs,
        None,
    ));
    let prefix_dirs = lines_view(&cfg0.prefix_dirs);
    p_lib.append(&row(
        "Prefix folders",
        "Folders of Wine prefixes; games installed inside them are found too.",
        &prefix_dirs,
        None,
    ));
    let lutris = sw(cfg0.lutris_import);
    p_lib.append(&row(
        "Import Lutris",
        "Games, settings, covers and play time from Lutris.",
        &lutris,
        None,
    ));
    let lutris_dir = entry(&cfg0.lutris_dir, "~/.local/share/lutris");
    p_lib.append(&row(
        "Lutris folder",
        "",
        &entry_browse(&win, &lutris_dir, true, "Lutris data folder"),
        None,
    ));
    let desktop = sw(cfg0.desktop_entries);
    p_lib.append(&row(
        "App entries",
        "Add .desktop apps in the Game category (flatpaks, emulators).",
        &desktop,
        None,
    ));
    let art = sw(cfg0.fetch_art);
    p_lib.append(&row(
        "Fetch cover art",
        "Look up covers for non-Steam games on the Steam store (sends the title only).",
        &art,
        None,
    ));
    {
        let (gd, pd, l, ld, d, a) = (
            game_dirs.clone(),
            prefix_dirs.clone(),
            lutris.clone(),
            lutris_dir.clone(),
            desktop.clone(),
            art.clone(),
        );
        collect.push(Box::new(move |c| {
            c.game_dirs = view_lines(&gd);
            c.prefix_dirs = view_lines(&pd);
            c.lutris_import = l.is_active();
            c.lutris_dir = ld.text().trim().to_string();
            c.desktop_entries = d.is_active();
            c.fetch_art = a.is_active();
        }));
    }
    let forget = button("BRING BACK REMOVED GAMES", "chip");
    forget.set_halign(gtk::Align::Start);
    forget.set_margin_top(12);
    {
        let a = app.clone();
        forget.connect_clicked(move |_| {
            crate::library::forget_removed();
            super::toast(&a, "Removed games come back on the next scan");
        });
    }
    p_lib.append(&forget);

    // INSTALLS
    let p_inst = page(
        "Installs",
        "Where INSTALL FROM .EXE puts games and prefixes.",
    );
    let install_root = entry(&cfg0.install_root, "~/Games");
    p_inst.append(&row(
        "Games go to",
        "Each install gets <this>/<name>.",
        &entry_browse(&win, &install_root, true, "Install folder"),
        None,
    ));
    let prefix_root = entry(&cfg0.prefix_root, "~/Games/Prefixes");
    p_inst.append(&row(
        "Prefixes go to",
        "Each install gets its own prefix <this>/<name>.",
        &entry_browse(&win, &prefix_root, true, "Prefix folder"),
        None,
    ));
    let drive = entry(&cfg0.install_drive, "x");
    drive.set_max_length(1);
    p_inst.append(&row(
        "Drive letter",
        "Mapped inside every new prefix, so installers see the games folder.",
        &drive,
        None,
    ));
    let drive_t = entry(&cfg0.install_drive_target, "~");
    p_inst.append(&row(
        "Drive points at",
        "X: → this folder. With ~ the default path becomes X:\\Games\\<name>.",
        &entry_browse(&win, &drive_t, true, "Drive target"),
        None,
    ));
    let downloads = entry(&cfg0.downloads_dir, "~/Downloads");
    p_inst.append(&row(
        "Installers are in",
        "Where the .exe picker opens.",
        &entry_browse(&win, &downloads, true, "Downloads"),
        None,
    ));
    {
        let (ir, pr, d, dt, dl) = (
            install_root.clone(),
            prefix_root.clone(),
            drive.clone(),
            drive_t.clone(),
            downloads.clone(),
        );
        collect.push(Box::new(move |c| {
            c.install_root = ir.text().trim().to_string();
            c.prefix_root = pr.text().trim().to_string();
            let l = d.text().trim().to_lowercase();
            if l.chars()
                .next()
                .map(|ch| ch.is_ascii_alphabetic() && ch != 'c' && ch != 'z')
                .unwrap_or(false)
            {
                c.install_drive = l;
            }
            c.install_drive_target = dt.text().trim().to_string();
            c.downloads_dir = dl.text().trim().to_string();
        }));
    }

    // STEAM + LAUNCH
    let p_launch = page("Launch", "How games are started.");
    let steam_root = entry(&cfg0.steam_root, "~/.local/share/Steam");
    p_launch.append(&row(
        "Steam folder",
        "",
        &entry_browse(&win, &steam_root, true, "Steam folder"),
        None,
    ));
    let steam_cmd = entry(
        &join_cmd(&cfg0.steam_command),
        "steam steam://rungameid/{appid}",
    );
    p_launch.append(&row(
        "Steam command",
        "{appid} is replaced. Point it at your own Steam wrapper if you have one.",
        &steam_cmd,
        None,
    ));
    let wrapper = entry(&join_cmd(&cfg0.wrapper), "none");
    p_launch.append(&row(
        "Launch wrapper",
        "Wraps games that have \"Launch wrapper\" on, e.g. a VPN split-tunnel command.",
        &wrapper,
        None,
    ));
    group(&p_launch, "MANGOHUD ENVIRONMENT");
    let mh_note = label(
        "Set when MangoHud is on. Default MANGOHUD=1; use your own layer's switch if you build MangoHud yourself.",
        "opt-help",
    );
    mh_note.set_wrap(true);
    p_launch.append(&mh_note);
    // Reuse the env editor on a scratch options value.
    let mh_opts: Opts = Rc::new(RefCell::new(LaunchOptions {
        env: cfg0.mangohud_env.clone(),
        ..Default::default()
    }));
    Ed {
        opts: mh_opts.clone(),
        base: LaunchOptions::default(),
        global: true,
    }
    .env(&p_launch);
    let extra_dirs = lines_view(&cfg0.runner_dirs);
    group(&p_launch, "MORE PROTON / WINE FOLDERS");
    p_launch.append(&row(
        "Runner folders",
        "Searched for Proton builds besides Steam's and Lutris's.",
        &extra_dirs,
        None,
    ));
    {
        let (sr, sc, w, mh, rd) = (
            steam_root.clone(),
            steam_cmd.clone(),
            wrapper.clone(),
            mh_opts.clone(),
            extra_dirs.clone(),
        );
        collect.push(Box::new(move |c| {
            c.steam_root = sr.text().trim().to_string();
            let s = split_args(&sc.text());
            if !s.is_empty() {
                c.steam_command = s;
            }
            c.wrapper = split_args(&w.text());
            c.mangohud_env = mh.borrow().env.clone();
            c.runner_dirs = view_lines(&rd);
        }));
    }

    // DEFAULTS (global launch options)
    let def_opts: Opts = Rc::new(RefCell::new(cfg0.defaults.clone()));
    let ed = Ed {
        opts: def_opts.clone(),
        base: LaunchOptions::default(),
        global: true,
    };
    let op = option_pages(&ed, &cfg0);
    op.set_runner(None);

    // TOOLS
    let p_tools = page("Tools", "What hearth found on this machine.");
    for (n, p) in runners::tools() {
        let st = label(
            if p.is_some() { "FOUND" } else { "MISSING" },
            if p.is_some() { "tool-ok" } else { "tool-miss" },
        );
        let r = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        r.add_css_class("opt");
        let nl = label(n, "opt-name");
        nl.set_width_chars(14);
        r.append(&nl);
        st.set_width_chars(8);
        r.append(&st);
        let pl = label(
            &p.map(|p| tilde(&p)).unwrap_or_else(|| "—".into()),
            "v mono",
        );
        pl.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        pl.set_hexpand(true);
        r.append(&pl);
        p_tools.append(&r);
    }
    group(&p_tools, "PROTON");
    for b in runners::protons_with(Some(&cfg0)) {
        let r = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        r.add_css_class("opt");
        let nl = label(&b.name, "opt-name");
        nl.set_width_chars(22);
        r.append(&nl);
        let ol = label(&b.origin.to_uppercase(), "tool-ok");
        ol.set_width_chars(8);
        r.append(&ol);
        let pl = label(&tilde(&b.path), "v mono");
        pl.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        pl.set_hexpand(true);
        r.append(&pl);
        p_tools.append(&r);
    }

    let mut pages: Vec<(&str, gtk::Box)> = vec![
        ("LIBRARY", p_lib),
        ("INSTALLS", p_inst),
        ("LAUNCH", p_launch),
    ];
    let names = [
        "DEF · RUNNER",
        "DEF · DISPLAY",
        "DEF · SYSTEM",
        "DEF · SCRIPTS",
        "DEF · ENV",
    ];
    for (i, (_, p)) in op.pages.iter().enumerate() {
        pages.push((names[i], p.clone()));
    }
    pages.push(("TOOLS", p_tools));
    let (root, _) = sectioned(&pages);
    body.append(&root);

    let cancel = button("CANCEL", "");
    let save = button("SAVE", "accent");
    foot.append(&cancel);
    foot.append(&save);
    {
        let w = win.clone();
        cancel.connect_clicked(move |_| w.close());
    }
    {
        let (a, w) = (app.clone(), win.clone());
        save.connect_clicked(move |_| {
            let mut c = cfg.borrow().clone();
            for f in &collect {
                f(&mut c);
            }
            c.defaults = def_opts.borrow().clone();
            c.ui = a.cfg.borrow().ui.clone();
            if let Err(e) = c.save() {
                super::toast(&a, &format!("Couldn't save: {e}"));
                return;
            }
            let rescan = c.game_dirs != a.cfg.borrow().game_dirs
                || c.prefix_dirs != a.cfg.borrow().prefix_dirs
                || c.lutris_import != a.cfg.borrow().lutris_import
                || c.desktop_entries != a.cfg.borrow().desktop_entries;
            *a.cfg.borrow_mut() = c;
            super::refresh_grid(&a);
            if rescan {
                super::scan(&a, false);
            }
            w.close();
        });
    }
    let _ = glib::MainContext::default();
    win.present();
}

//! Settings search: one box above the section list finds a section or any setting inside one,
//! shows the hits as "SECTION › GROUP" breadcrumbs, and jumps to the row (scrolled into view and
//! flashed) on click or Enter. Rows are read from the live widgets, so every option a page shows
//! is searchable without a separate index, and options hidden by the current runner stay hidden.

use super::label;
use gtk::{glib, prelude::*};
use gtk4 as gtk;
use std::rc::Rc;
use std::time::Duration;

const RESULTS: &str = "__search";
const MAX_HITS: usize = 60;

/// One page the search can jump into.
pub struct Section {
    pub name: String,
    pub page: gtk::Box,
    pub scroll: gtk::ScrolledWindow,
    pub button: gtk::Button,
}

struct Hit {
    section: usize,
    /// Row to scroll to; None = the section itself.
    row: Option<gtk::Widget>,
    name: String,
    crumb: String,
    help: String,
    score: u32,
}

/// Adds the search box to the top of `side` and a results page to `stack`. Typing anywhere in
/// `capture` (when no text field has focus) starts a search.
pub fn attach(
    side: &gtk::Box,
    stack: &gtk::Stack,
    capture: &impl IsA<gtk::Widget>,
    sections: Vec<Section>,
) {
    let sections = Rc::new(sections);
    let entry = gtk::SearchEntry::new();
    entry.set_placeholder_text(Some("Search settings"));
    entry.add_css_class("settings-search");
    entry.set_key_capture_widget(Some(capture));
    side.prepend(&entry);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list.add_css_class("page");
    let sc = gtk::ScrolledWindow::new();
    sc.set_hscrollbar_policy(gtk::PolicyType::Never);
    sc.set_child(Some(&list));
    stack.add_named(&sc, Some(RESULTS));

    // Leaving the search (nav click, Escape, cleared box) goes back to the active section.
    let back = {
        let (stack, sections) = (stack.clone(), sections.clone());
        let side = side.clone();
        move || {
            side.remove_css_class("searching");
            let i = sections
                .iter()
                .position(|s| s.button.has_css_class("active"))
                .unwrap_or(0);
            if let Some(s) = sections.get(i) {
                stack.set_visible_child_name(&s.name);
            }
        }
    };
    for s in sections.iter() {
        let e = entry.clone();
        s.button.connect_clicked(move |_| {
            if !e.text().is_empty() {
                e.set_text("");
            }
        });
    }

    let first: Rc<std::cell::RefCell<Option<gtk::Button>>> = Rc::default();
    {
        let (stack, sections, list, sc, first, back, side) = (
            stack.clone(),
            sections.clone(),
            list.clone(),
            sc.clone(),
            first.clone(),
            back.clone(),
            side.clone(),
        );
        entry.connect_search_changed(move |e| {
            let q = e.text().trim().to_lowercase();
            if q.is_empty() {
                back();
                return;
            }
            fill(&list, &sections, &q, &first, e);
            sc.vadjustment().set_value(0.0);
            side.add_css_class("searching");
            stack.set_visible_child_name(RESULTS);
        });
    }
    {
        let first = first.clone();
        entry.connect_activate(move |e| {
            if e.text().trim().is_empty() {
                return;
            }
            if let Some(b) = first.borrow().clone() {
                b.emit_clicked();
            }
        });
    }
    entry.connect_stop_search(move |e| {
        e.set_text("");
        back();
    });
}

fn fill(
    list: &gtk::Box,
    sections: &Rc<Vec<Section>>,
    q: &str,
    first: &Rc<std::cell::RefCell<Option<gtk::Button>>>,
    entry: &gtk::SearchEntry,
) {
    while let Some(c) = list.first_child() {
        list.remove(&c);
    }
    let terms: Vec<&str> = q.split_whitespace().collect();
    let mut hits = collect(sections, &terms);
    hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.section.cmp(&b.section)));
    let total = hits.len();
    hits.truncate(MAX_HITS);

    let head = label(
        &match total {
            0 => "No results".to_string(),
            1 => "1 result".to_string(),
            n if n > MAX_HITS => format!("{MAX_HITS} of {n} results"),
            n => format!("{n} results"),
        },
        "page-title",
    );
    list.append(&head);
    if total == 0 {
        let s = label(
            &format!(
                "Nothing matches \u{201c}{q}\u{201d}. Try a shorter word, like \u{201c}fps\u{201d} or \u{201c}prefix\u{201d}."
            ),
            "page-sub",
        );
        s.set_wrap(true);
        list.append(&s);
    }
    *first.borrow_mut() = None;
    for h in hits {
        let b = gtk::Button::new();
        b.add_css_class("search-hit");
        let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let crumb = label(&h.crumb, "hit-crumb");
        crumb.set_xalign(0.0);
        col.append(&crumb);
        let name = gtk::Label::new(None);
        name.set_markup(&mark(&h.name, &terms));
        name.set_xalign(0.0);
        name.add_css_class("hit-name");
        col.append(&name);
        if !h.help.is_empty() {
            let help = gtk::Label::new(None);
            help.set_markup(&mark(&h.help, &terms));
            help.set_xalign(0.0);
            help.add_css_class("hit-help");
            help.set_ellipsize(gtk::pango::EllipsizeMode::End);
            col.append(&help);
        }
        b.set_child(Some(&col));
        let (sections, e) = (sections.clone(), entry.clone());
        b.connect_clicked(move |_| jump(&sections, h.section, h.row.clone(), &e));
        if first.borrow().is_none() {
            *first.borrow_mut() = Some(b.clone());
        }
        list.append(&b);
    }
}

fn jump(sections: &Rc<Vec<Section>>, i: usize, row: Option<gtk::Widget>, entry: &gtk::SearchEntry) {
    let Some(s) = sections.get(i) else { return };
    s.button.emit_clicked(); // switches the page, marks it active, clears the search
    entry.set_text("");
    let (page, scroll) = (s.page.clone(), s.scroll.clone());
    // Wait one beat so the page is mapped and allocated before measuring the row.
    glib::timeout_add_local_once(Duration::from_millis(110), move || {
        let Some(row) = row else {
            scroll.vadjustment().set_value(0.0);
            return;
        };
        if let Some(p) = row.compute_point(&page, &gtk::graphene::Point::zero()) {
            let adj = scroll.vadjustment();
            let y = (p.y() as f64 - adj.page_size() * 0.3)
                .clamp(0.0, (adj.upper() - adj.page_size()).max(0.0));
            adj.set_value(y);
        }
        row.add_css_class("search-flash");
        if let Some(w) = focusable(&row) {
            w.grab_focus();
        }
        glib::timeout_add_local_once(Duration::from_millis(1400), move || {
            row.remove_css_class("search-flash")
        });
    });
}

/// First control in a row that takes focus, so Space/arrows work right after the jump.
fn focusable(w: &gtk::Widget) -> Option<gtk::Widget> {
    let mut c = w.first_child();
    while let Some(x) = c {
        if x.is_visible() && x.is_focusable() && x.is_sensitive() && !x.is::<gtk::Label>() {
            return Some(x);
        }
        if let Some(f) = focusable(&x) {
            return Some(f);
        }
        c = x.next_sibling();
    }
    None
}

fn collect(sections: &[Section], terms: &[&str]) -> Vec<Hit> {
    let mut hits = vec![];
    for (i, s) in sections.iter().enumerate() {
        if !s.page.is_visible() || !s.button.is_visible() {
            continue;
        }
        let title = first_text(s.page.upcast_ref(), "page-title").unwrap_or_else(|| s.name.clone());
        let sub = first_text(s.page.upcast_ref(), "page-sub").unwrap_or_default();
        if let Some(score) = score(&title, &format!("{} {sub}", s.name), terms) {
            hits.push(Hit {
                section: i,
                row: None,
                name: title.clone(),
                crumb: "SECTION".into(),
                help: sub,
                score: score + 5,
            });
        }
        let mut group = String::new();
        walk(
            s.page.upcast_ref(),
            &mut group,
            &mut |row, name, help, group| {
                let crumb = if group.is_empty() {
                    s.name.clone()
                } else {
                    format!("{}  \u{203a}  {}", s.name, group)
                };
                if let Some(sc) = score(name, &format!("{help} {group} {} {title}", s.name), terms)
                {
                    hits.push(Hit {
                        section: i,
                        row: Some(row.clone()),
                        name: name.to_string(),
                        crumb,
                        help: help.to_string(),
                        score: sc,
                    });
                }
            },
        );
    }
    hits
}

/// Visits every visible `.opt` row in page order, tracking the last `.group-title` seen.
fn walk(w: &gtk::Widget, group: &mut String, f: &mut dyn FnMut(&gtk::Widget, &str, &str, &str)) {
    let mut c = w.first_child();
    while let Some(x) = c {
        c = x.next_sibling();
        if !x.is_visible() {
            continue;
        }
        if x.has_css_class("group-title") {
            if let Some(l) = x.downcast_ref::<gtk::Label>() {
                *group = l.text().to_string();
            }
            continue;
        }
        if x.has_css_class("opt") {
            if let Some(name) = first_text(&x, "opt-name") {
                let help = first_text(&x, "opt-help").unwrap_or_default();
                f(&x, &name, &help, group);
            }
            continue;
        }
        walk(&x, group, f);
    }
}

fn first_text(w: &gtk::Widget, class: &str) -> Option<String> {
    let mut c = w.first_child();
    while let Some(x) = c {
        if let Some(l) = x
            .downcast_ref::<gtk::Label>()
            .filter(|_| x.has_css_class(class))
        {
            return Some(l.text().to_string());
        }
        if let Some(t) = first_text(&x, class) {
            return Some(t);
        }
        c = x.next_sibling();
    }
    None
}

/// Every term must appear in the name or the extra text. Name hits rank above help/group hits,
/// word starts above the middle of a word.
fn score(name: &str, extra: &str, terms: &[&str]) -> Option<u32> {
    let n = name.to_lowercase();
    let x = extra.to_lowercase();
    let mut total = 0;
    for t in terms {
        let s = if n.starts_with(t) {
            40
        } else if word_start(&n, t) {
            30
        } else if n.contains(t) {
            20
        } else if word_start(&x, t) {
            8
        } else if x.contains(t) {
            4
        } else {
            return None;
        };
        total += s;
    }
    Some(total)
}

fn word_start(hay: &str, t: &str) -> bool {
    hay.match_indices(t).any(|(i, _)| {
        i == 0
            || !hay[..i]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
    })
}

/// Pango markup with the matched terms highlighted.
fn mark(text: &str, terms: &[&str]) -> String {
    let lower = text.to_lowercase();
    // Byte ranges line up only when lowercasing keeps lengths; fall back to plain text if not.
    if lower.len() != text.len() {
        return glib::markup_escape_text(text).to_string();
    }
    let mut on = vec![false; text.len()];
    for t in terms.iter().filter(|t| !t.is_empty()) {
        for (i, _) in lower.match_indices(t) {
            on[i..i + t.len()].iter_mut().for_each(|b| *b = true);
        }
    }
    let mut out = String::new();
    let mut cur = false;
    let mut seg = String::new();
    for (i, ch) in text.char_indices() {
        if on[i] != cur {
            push(&mut out, &seg, cur);
            seg.clear();
            cur = on[i];
        }
        seg.push(ch);
    }
    push(&mut out, &seg, cur);
    out
}

fn push(out: &mut String, seg: &str, hl: bool) {
    if seg.is_empty() {
        return;
    }
    let e = glib::markup_escape_text(seg);
    if hl {
        out.push_str(&format!(
            "<span foreground=\"#ff8c1a\" weight=\"bold\">{e}</span>"
        ));
    } else {
        out.push_str(&e);
    }
}

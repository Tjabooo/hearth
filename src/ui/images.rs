//! Art loading: decode + downscale in a worker thread, build the texture on the main thread,
//! cache by (path, size). Covers are loaded at 2x tile size so they stay sharp on HiDPI.

use gtk4 as gtk;
use gtk::{gdk, gdk_pixbuf, glib, prelude::*};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

struct Raw {
    w: i32,
    h: i32,
    stride: usize,
    alpha: bool,
    bytes: glib::Bytes,
}

fn decode(path: &Path, w: i32, h: i32) -> Option<Raw> {
    let pb = gdk_pixbuf::Pixbuf::from_file_at_scale(path, w, h, true).ok()?;
    Some(Raw { w: pb.width(), h: pb.height(), stride: pb.rowstride() as usize, alpha: pb.has_alpha(), bytes: pb.read_pixel_bytes() })
}

fn texture(r: Raw) -> gdk::Texture {
    let fmt = if r.alpha { gdk::MemoryFormat::R8g8b8a8 } else { gdk::MemoryFormat::R8g8b8 };
    gdk::MemoryTexture::new(r.w, r.h, fmt, &r.bytes, r.stride).upcast()
}

type Key = (PathBuf, i32, u64);

#[derive(Clone, Default)]
pub struct Cache {
    map: Rc<RefCell<HashMap<Key, gdk::Texture>>>,
}

fn mtime(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Cache {
    /// Show `path` (scaled to fit w x h) in `pic`; `done(true)` once something is shown.
    pub fn load(&self, pic: &gtk::Picture, path: &Path, w: i32, h: i32, done: impl Fn(bool) + 'static) {
        let key = (path.to_path_buf(), w, mtime(path));
        if let Some(t) = self.map.borrow().get(&key) {
            pic.set_paintable(Some(t));
            done(true);
            return;
        }
        let map = self.map.clone();
        let weak = pic.downgrade();
        let p = path.to_path_buf();
        glib::spawn_future_local(async move {
            let raw = gtk::gio::spawn_blocking(move || decode(&p, w, h)).await.ok().flatten();
            let Some(raw) = raw else {
                done(false);
                return;
            };
            let tex = texture(raw);
            map.borrow_mut().insert(key, tex.clone());
            if let Some(pic) = weak.upgrade() {
                pic.set_paintable(Some(&tex));
            }
            done(true);
        });
    }

    /// Drop cached versions of one file (after "Change cover").
    pub fn forget(&self, path: &Path) {
        self.map.borrow_mut().retain(|k, _| k.0 != path);
    }
}

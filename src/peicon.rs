//! Pull the main icon out of a Windows .exe (PE resources: RT_GROUP_ICON -> RT_ICON) and save its
//! largest image. Modern icons embed PNGs (written as-is); older ones are DIBs, written as a
//! one-image .ico (gdk-pixbuf reads both by content, whatever the file name says).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;
/// Don't read absurd resource sections (some games pack hundreds of MB there).
const MAX_RSRC: u64 = 64 << 20;

fn u16le(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32le(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

struct Rsrc {
    data: Vec<u8>,
    /// RVA of `data[0]`.
    rva: u32,
}

impl Rsrc {
    /// Entries of the directory at `off`: (id or name-offset, is_named, target, is_dir)
    fn entries(&self, off: usize) -> Vec<(u32, bool, usize, bool)> {
        let d = &self.data;
        let (Some(named), Some(ids)) = (u16le(d, off + 12), u16le(d, off + 14)) else {
            return vec![];
        };
        (0..(named as usize + ids as usize).min(4096))
            .filter_map(|i| {
                let e = off + 16 + i * 8;
                let name = u32le(d, e)?;
                let to = u32le(d, e + 4)?;
                Some((
                    name & 0x7fff_ffff,
                    name & 0x8000_0000 != 0,
                    (to & 0x7fff_ffff) as usize,
                    to & 0x8000_0000 != 0,
                ))
            })
            .collect()
    }

    /// First leaf below a directory entry (any name, any language).
    fn first_leaf(&self, mut off: usize, mut is_dir: bool) -> Option<&[u8]> {
        for _ in 0..4 {
            if !is_dir {
                let rva = u32le(&self.data, off)?;
                let size = u32le(&self.data, off + 4)? as usize;
                let start = rva.checked_sub(self.rva)? as usize;
                return self.data.get(start..start.checked_add(size)?);
            }
            let (_, _, t, d) = *self.entries(off).first()?;
            off = t;
            is_dir = d;
        }
        None
    }
}

fn read_rsrc(path: &Path) -> Option<Rsrc> {
    let mut f = File::open(path).ok()?;
    let mut head = vec![0u8; 4096];
    let n = f.read(&mut head).ok()?;
    head.truncate(n);
    if head.get(0..2)? != b"MZ" {
        return None;
    }
    let pe = u32le(&head, 0x3c)? as usize;
    if head.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    let coff = pe + 4;
    let nsec = u16le(&head, coff + 2)? as usize;
    let opt_size = u16le(&head, coff + 16)? as usize;
    let opt = coff + 20;
    let dirs = match u16le(&head, opt)? {
        0x10b => opt + 96,
        0x20b => opt + 112,
        _ => return None,
    };
    let rsrc_rva = u32le(&head, dirs + 2 * 8)?;
    if rsrc_rva == 0 {
        return None;
    }
    let secs = opt + opt_size;
    for i in 0..nsec.min(96) {
        let s = secs + i * 40;
        let vsize = u32le(&head, s + 8)?;
        let va = u32le(&head, s + 12)?;
        let raw_size = u32le(&head, s + 16)?;
        let raw_ptr = u32le(&head, s + 20)?;
        if rsrc_rva >= va && rsrc_rva < va + vsize.max(raw_size) {
            let len = (raw_size as u64).min(MAX_RSRC);
            let skip = (rsrc_rva - va) as u64;
            f.seek(SeekFrom::Start(raw_ptr as u64 + skip)).ok()?;
            let mut data = vec![0u8; len.saturating_sub(skip) as usize];
            f.read_exact(&mut data).ok()?;
            return Some(Rsrc {
                data,
                rva: rsrc_rva,
            });
        }
    }
    None
}

/// The largest image of the exe's first icon group. Returns (bytes, edge in px).
pub fn best_icon(exe: &Path) -> Option<(Vec<u8>, u32)> {
    let r = read_rsrc(exe)?;
    let top = r.entries(0);
    let icons_dir = top.iter().find(|e| !e.1 && e.0 == RT_ICON && e.3)?.2;
    let group_dir = top.iter().find(|e| !e.1 && e.0 == RT_GROUP_ICON && e.3)?.2;
    // The app's icon is the first group (Explorer shows the same one).
    let (_, _, g_off, g_dir) = *r.entries(group_dir).first()?;
    let grp = r.first_leaf(g_off, g_dir)?;
    let count = u16le(grp, 4)? as usize;
    let mut best: Option<(u32, u16, u16)> = None; // (edge, bits, id)
    for i in 0..count.min(64) {
        let e = 6 + i * 14;
        let w = *grp.get(e)? as u32;
        let edge = if w == 0 { 256 } else { w };
        let bits = u16le(grp, e + 6)?;
        let id = u16le(grp, e + 12)?;
        if best
            .map(|(be, bb, _)| (edge, bits) > (be, bb))
            .unwrap_or(true)
        {
            best = Some((edge, bits, id));
        }
    }
    let (edge, bits, id) = best?;
    let icons = r.entries(icons_dir);
    let (_, _, off, is_dir) = *icons.iter().find(|e| !e.1 && e.0 == id as u32)?;
    let img = r.first_leaf(off, is_dir)?;
    if img.starts_with(b"\x89PNG") {
        return Some((img.to_vec(), edge));
    }
    // DIB -> single-image .ico
    let mut ico = Vec::with_capacity(22 + img.len());
    ico.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
    let b = if edge >= 256 { 0 } else { edge as u8 };
    ico.extend_from_slice(&[b, b, 0, 0]);
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&bits.to_le_bytes());
    ico.extend_from_slice(&(img.len() as u32).to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes());
    ico.extend_from_slice(img);
    Some((ico, edge))
}

/// Save the exe's icon to `dst` if it's at least `min_edge` px. True on success.
pub fn extract(exe: &Path, dst: &Path, min_edge: u32) -> bool {
    let Some((bytes, edge)) = best_icon(exe) else {
        return false;
    };
    if edge < min_edge {
        return false;
    }
    if let Some(d) = dst.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    std::fs::write(dst, bytes).is_ok()
}

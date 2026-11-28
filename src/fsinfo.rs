//! What a directory listing needs, shared by lvc (its panels) and lfz (the preview of a directory): the entries
//! of ONE directory with their kind, size and time, a natural name order, and the small formatting helpers
//! (sizes, dates).

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    /// A symlink whose target is a directory (entered like one).
    LinkDir,
    /// A symlink to a file, or a broken one.
    Link,
    Other,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    /// Seconds since the epoch.
    pub mtime: i64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// Where a symlink points.
    pub target: Option<String>,
    /// A symlink whose target is gone.
    pub broken: bool,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, Kind::Dir | Kind::LinkDir)
    }

    pub fn is_exec(&self) -> bool {
        self.kind == Kind::File && self.mode & 0o111 != 0
    }

    /// The `..` row: the parent directory.
    pub fn parent() -> Entry {
        Entry { name: "..".into(), kind: Kind::Dir, size: 0, mtime: 0, mode: 0o40755, uid: 0, gid: 0, target: None, broken: false }
    }

    pub fn is_parent(&self) -> bool {
        self.name == ".."
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Name,
    Ext,
    Size,
    Time,
}

impl Sort {
    pub fn next(self) -> Sort {
        match self {
            Sort::Name => Sort::Ext,
            Sort::Ext => Sort::Size,
            Sort::Size => Sort::Time,
            Sort::Time => Sort::Name,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Sort::Name => "name",
            Sort::Ext => "ext",
            Sort::Size => "size",
            Sort::Time => "time",
        }
    }
}

/// One entry from its path (`lstat`, and `stat` through a symlink).
pub fn entry(path: &Path, name: String) -> Option<Entry> {
    let md = std::fs::symlink_metadata(path).ok()?;
    let ft = md.file_type();
    let (kind, target, broken, size) = if ft.is_symlink() {
        let target = std::fs::read_link(path).ok().map(|t| t.to_string_lossy().into_owned());
        match std::fs::metadata(path) {
            Ok(t) if t.is_dir() => (Kind::LinkDir, target, false, md.len()),
            Ok(t) => (Kind::Link, target, false, t.len()),
            Err(_) => (Kind::Link, target, true, md.len()),
        }
    } else if ft.is_dir() {
        (Kind::Dir, None, false, md.len())
    } else if ft.is_file() {
        (Kind::File, None, false, md.len())
    } else {
        (Kind::Other, None, false, md.len())
    };
    Some(Entry { name, kind, size, mtime: md.mtime(), mode: md.permissions().mode(), uid: md.uid(), gid: md.gid(), target, broken })
}

/// The entries of `dir`: directories first, then by `sort`; hidden ones only with `hidden`. The
/// `..` row is NOT included (the panel adds it).
pub fn list_dir(dir: &Path, hidden: bool, sort: Sort, reverse: bool) -> std::io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let Ok(e) = e else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        if !hidden && name.starts_with('.') {
            continue;
        }
        if let Some(en) = entry(&e.path(), name) {
            out.push(en);
        }
    }
    sort_entries(&mut out, sort, reverse);
    Ok(out)
}

pub fn sort_entries(v: &mut [Entry], sort: Sort, reverse: bool) {
    v.sort_by(|a, b| {
        // directories first, whatever the order
        let d = b.is_dir().cmp(&a.is_dir());
        if d != std::cmp::Ordering::Equal {
            return d;
        }
        let o = match sort {
            Sort::Name => natural_cmp(&a.name, &b.name),
            Sort::Ext => ext(&a.name).cmp(ext(&b.name)).then_with(|| natural_cmp(&a.name, &b.name)),
            Sort::Size => b.size.cmp(&a.size).then_with(|| natural_cmp(&a.name, &b.name)),
            Sort::Time => b.mtime.cmp(&a.mtime).then_with(|| natural_cmp(&a.name, &b.name)),
        };
        if reverse { o.reverse() } else { o }
    });
}

fn ext(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, e)) if !stem.is_empty() => e,
        _ => "",
    }
}

/// Case-insensitive, with digit runs compared as numbers (`file2` before `file10`).
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut n1 = String::new();
                while let Some(c) = x.peek().copied().filter(char::is_ascii_digit) {
                    n1.push(c);
                    x.next();
                }
                let mut n2 = String::new();
                while let Some(d) = y.peek().copied().filter(char::is_ascii_digit) {
                    n2.push(d);
                    y.next();
                }
                let (t1, t2) = (n1.trim_start_matches('0'), n2.trim_start_matches('0'));
                let o = t1.len().cmp(&t2.len()).then_with(|| t1.cmp(t2));
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

/// `1.2K`, `34M`, ... (1024-based, as `ls -h`).
pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 6] = ["K", "M", "G", "T", "P", "E"];
    if n < 1024 {
        return n.to_string();
    }
    let mut v = n as f64 / 1024.0;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v < 10.0 { format!("{v:.1}{}", UNITS[u]) } else { format!("{v:.0}{}", UNITS[u]) }
}

/// Local time of `t` (seconds since the epoch) as (year, month, day, hour, minute, second).
pub fn local_time(t: i64) -> (i32, u32, u32, u32, u32, u32) {
    // SAFETY: localtime_r only writes into the tm we own.
    unsafe {
        let tt: libc::time_t = t as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&tt, &mut tm);
        (tm.tm_year + 1900, (tm.tm_mon + 1) as u32, tm.tm_mday as u32, tm.tm_hour as u32, tm.tm_min as u32, tm.tm_sec as u32)
    }
}

/// `2026-11-21 10:42` (this year) / `2025-03-02  2025` style is not needed: always the full date.
pub fn date_string(t: i64) -> String {
    let (y, mo, d, h, mi, _) = local_time(t);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}")
}

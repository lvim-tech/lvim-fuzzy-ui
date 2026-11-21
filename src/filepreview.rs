//! The built-in file preview: files are read off the UI thread and kept in a
//! small LRU cache (a re-visited file is instant), like lvim-picker's async
//! file preview. Directories list their entries.

use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::SystemTime;

const CACHE: usize = 32;
const MAX_BYTES: u64 = 4 << 20;
const MAX_LINES: usize = 20_000;

#[derive(Debug)]
pub struct View {
    pub path: String,
    pub lines: Vec<String>,
    pub is_dir: bool,
    /// Why there is nothing to show (binary, unreadable, too large, ...).
    pub note: Option<String>,
    pub truncated: bool,
    /// Syntax spans per line (syntect; empty for unknown languages).
    pub spans: Vec<Vec<crate::syntax::Span>>,
    /// The spans are done (or there are none to make): the text shows at
    /// once, the colours follow when the file is still the one shown.
    pub highlighted: bool,
    mtime: Option<SystemTime>,
}

#[derive(Default)]
struct Cache {
    entries: Vec<Arc<View>>,
}

impl Cache {
    fn get(&mut self, path: &str) -> Option<Arc<View>> {
        let i = self.entries.iter().position(|v| v.path == path)?;
        let v = self.entries.remove(i);
        // a changed file is read again
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if mtime != v.mtime {
            return None;
        }
        self.entries.insert(0, v.clone());
        Some(v)
    }

    fn put(&mut self, v: Arc<View>) {
        self.entries.retain(|e| e.path != v.path);
        self.entries.insert(0, v);
        self.entries.truncate(CACHE);
    }
}

pub struct Loader {
    tx: Sender<String>,
    cache: Arc<Mutex<Cache>>,
    /// The file shown now: a highlight for another one is given up.
    wanted: Arc<Mutex<String>>,
}

impl Loader {
    pub fn new(wake: Sender<()>) -> Loader {
        let (tx, rx): (Sender<String>, Receiver<String>) = channel();
        let cache = Arc::new(Mutex::new(Cache::default()));
        let wanted: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let c = cache.clone();
        let w = wanted.clone();
        thread::spawn(move || {
            while let Ok(mut path) = rx.recv() {
                while let Ok(p) = rx.try_recv() {
                    path = p;
                }
                // 1. the text, at once (a key held down moves on quickly); a
                // file that makes the reader fail must not end this thread
                // (every later preview would stay as it is)
                let cached = c.lock().unwrap().get(&path);
                let v = match cached {
                    Some(v) => v,
                    None => {
                        let read_one = std::panic::catch_unwind(|| read(&path));
                        let v = Arc::new(read_one.unwrap_or_else(|_| View {
                            path: path.clone(),
                            lines: Vec::new(),
                            is_dir: false,
                            note: Some("cannot read this file".into()),
                            truncated: false,
                            spans: Vec::new(),
                            highlighted: true,
                            mtime: None,
                        }));
                        c.lock().unwrap().put(v.clone());
                        let _ = wake.send(());
                        v
                    }
                };
                // 2. the colours, while the file is still the one shown
                if v.highlighted {
                    continue;
                }
                let moved_on = || *w.lock().unwrap() != path;
                // a parser failure: the text stays, without colours
                let spans = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::syntax::highlight_file_until(&path, &v.lines, 5000, &moved_on)
                }))
                .unwrap_or(Some(Vec::new()));
                if let Some(spans) = spans {
                    let done = View {
                        path: v.path.clone(),
                        lines: v.lines.clone(),
                        is_dir: v.is_dir,
                        note: v.note.clone(),
                        truncated: v.truncated,
                        spans,
                        highlighted: true,
                        mtime: v.mtime,
                    };
                    c.lock().unwrap().put(Arc::new(done));
                    let _ = wake.send(());
                }
            }
        });
        Loader { tx, cache, wanted }
    }

    /// The cached view of `path` (perhaps not coloured yet), or None (a load
    /// is started).
    pub fn view(&self, path: &str) -> Option<Arc<View>> {
        {
            let mut w = self.wanted.lock().unwrap();
            if *w != path {
                *w = path.to_string();
            }
        }
        let cached = self.cache.lock().unwrap().get(path);
        match cached {
            Some(v) if v.highlighted => Some(v),
            Some(v) => {
                // shown as text; the colours are (still) to come
                let _ = self.tx.send(path.to_string());
                Some(v)
            }
            None => {
                let _ = self.tx.send(path.to_string());
                None
            }
        }
    }
}

fn read(path: &str) -> View {
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let mut v = View {
        path: path.to_string(),
        lines: Vec::new(),
        is_dir: false,
        note: None,
        truncated: false,
        spans: Vec::new(),
        highlighted: true,
        mtime,
    };
    let p = Path::new(path);
    if p.is_dir() {
        v.is_dir = true;
        match std::fs::read_dir(p) {
            Ok(rd) => {
                let mut dirs = Vec::new();
                let mut files = Vec::new();
                for e in rd.filter_map(Result::ok) {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if e.path().is_dir() {
                        dirs.push(format!("{name}/"));
                    } else {
                        files.push(name);
                    }
                }
                dirs.sort();
                files.sort();
                v.lines = dirs.into_iter().chain(files).collect();
                if v.lines.is_empty() {
                    v.note = Some("empty directory".into());
                }
            }
            Err(e) => v.note = Some(e.to_string()),
        }
        return v;
    }
    let file = match std::fs::File::open(p) {
        Ok(f) => f,
        Err(e) => {
            v.note = Some(e.to_string());
            return v;
        }
    };
    use std::io::Read;
    let mut buf = Vec::new();
    if let Err(e) = file.take(MAX_BYTES).read_to_end(&mut buf) {
        v.note = Some(e.to_string());
        return v;
    }
    if buf[..buf.len().min(8192)].contains(&0) {
        v.note = Some("binary file".into());
        return v;
    }
    let text = String::from_utf8_lossy(&buf);
    for (i, l) in text.lines().enumerate() {
        if i >= MAX_LINES {
            v.truncated = true;
            break;
        }
        v.lines.push(l.trim_end_matches('\r').to_string());
    }
    if std::fs::metadata(p).is_ok_and(|m| m.len() > MAX_BYTES) {
        v.truncated = true;
    }
    // the colours come in a second step (Loader), only for the file still shown
    v.highlighted = false;
    v
}

#[cfg(test)]
mod hold_tests {
    use super::*;

    /// A key held down: a new file every frame (33 ms). The text of each one
    /// must show while it is the current one, not only after the keys stop.
    // a timing test (30 highlighted files, a frame each): only meaningful in an optimized build
    #[test]
    #[cfg_attr(debug_assertions, ignore = "timing: run with --release")]
    fn a_held_key_shows_every_file() {
        let dir = std::env::temp_dir().join(format!("lvf-hold-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let body: String = (0..5000)
            .map(|i| format!("fn f{i}(x: u32) -> u32 {{ let y = x * {i}; if y > 3 {{ y }} else {{ x + 1 }} }}\n"))
            .collect();
        let paths: Vec<String> = (0..30)
            .map(|n| {
                let p = dir.join(format!("f{n}.rs"));
                std::fs::write(&p, format!("// {n}\n{body}")).unwrap();
                p.to_string_lossy().into_owned()
            })
            .collect();
        let (wake, _w) = std::sync::mpsc::channel();
        let loader = Loader::new(wake);
        let mut shown = 0;
        for p in &paths {
            // two frames on this file, as the key repeats
            let mut ok = false;
            for _ in 0..2 {
                std::thread::sleep(std::time::Duration::from_millis(33));
                ok |= loader
                    .view(p)
                    .is_some_and(|v| v.lines.first().is_some_and(|l| l.contains("//")));
            }
            shown += ok as usize;
        }
        std::fs::remove_dir_all(&dir).ok();
        eprintln!(
            "held key: {shown} of {} files shown while current",
            paths.len()
        );
        assert!(shown >= paths.len() * 8 / 10, "{shown} of {}", paths.len());
    }
}

//! File icons as IMAGES: the Material Icon Theme SVGs (the set lvim-icons ships), drawn by the terminal
//! through kitty's unicode placeholders — the same recipe as lvim-icons' image mode in Neovim
//! (lvim-icons/lua/lvim-icons/{material,image,placeholders}.lua + lvim-image's kitty protocol):
//!
//! - the icon is looked up in the theme's manifest (`material-icons.json`: file names, extensions,
//!   folder names, closed / open);
//! - its SVG is rasterised into a transparent canvas exactly 2 cells × 1 cell in pixels, the icon a square
//!   of 72% of the cell height, centred;
//! - the RGBA is sent ONCE (lazily, the first time the icon is drawn) as a virtual placement:
//!   `ESC_G a=T,U=1,i=ID,c=2,r=1,t=d,f=32,s=W,v=H,q=2;<base64> ESC\`, in 4096-byte chunks;
//! - in the text, two cells `U+10EEEE` + row / column diacritics, whose FOREGROUND (truecolor) is the image
//!   id: the terminal draws the image there. The fg is data, not a colour: it must never be dimmed or
//!   blended (a selected row changes only the background).
//!
//! Images are used where the terminal takes placeholders: kitty and ghostty directly, zellij when its build
//! accepts them (`zellij = true`, as lvim-image's `zellij_placeholders`), never inside tmux (no passthrough
//! here). Anything else — or no cell size, or no icon set — keeps the Nerd Font glyphs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// How icons are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Nerd Font glyphs.
    Font,
    /// Images when the terminal takes them, else glyphs (the default).
    Auto,
    /// Images (still glyphs where the terminal cannot show them).
    Image,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        Some(match s {
            "font" => Mode::Font,
            "auto" => Mode::Auto,
            "image" | "svg" => Mode::Image,
            _ => return None,
        })
    }
}

/// The shared place of the Material set, one for every lvim program (lvim-icons in Neovim, lvim-fuzzy,
/// lvim-commander): `$XDG_DATA_HOME/lvim-icons/material` (`~/.local/share/...`).
pub fn shared_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .map(|d| d.join("lvim-icons/material"))
}

/// Where the Material set is read from: `$LVIM_ICONS_MATERIAL`, then the shared place.
pub fn material_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = std::env::var_os("LVIM_ICONS_MATERIAL").filter(|d| !d.is_empty()) {
        v.push(PathBuf::from(d));
    }
    v.extend(shared_dir());
    v
}

/// The set this program carries (scripts/pack-material.sh packs it from lvim-icons).
const EMBEDDED: &[u8] = include_bytes!("../assets/material.tar.gz");

/// `5.40.0` → [5, 40, 0] (missing / odd parts are 0): versions compare numerically.
fn version_key(v: &str) -> Vec<u64> {
    v.trim().split('.').map(|p| p.trim().parse().unwrap_or(0)).collect()
}

fn read_version(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("VERSION")).ok().map(|s| s.trim().to_string())
}

/// The version of the embedded set.
pub fn embedded_version() -> Option<String> {
    let gz = flate2::read::GzDecoder::new(EMBEDDED);
    let mut ar = tar::Archive::new(gz);
    for e in ar.entries().ok()? {
        let mut e = e.ok()?;
        if e.path().ok()?.file_name().is_some_and(|n| n == "VERSION") {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut e, &mut s).ok()?;
            return Some(s.trim().to_string());
        }
    }
    None
}

/// What `install_shared` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Installed {
    /// Written (it was missing, or older): the version now there.
    Wrote(String),
    /// The shared place already had this version or a newer one.
    Current(String),
}

/// Put the embedded set into the shared place when it is missing there or OLDER (never a downgrade: a
/// newer set from another program stays). Atomic: unpacked next to it, then swapped in with renames, so a
/// program reading it — or a second one installing at the same time — never sees half a set.
pub fn install_shared() -> Result<Installed, String> {
    let dir = shared_dir().ok_or("no home directory")?;
    install_into(&dir)
}

fn install_into(dir: &Path) -> Result<Installed, String> {
    let ours = embedded_version().ok_or("the embedded icon set has no VERSION")?;
    if let Some(theirs) = read_version(dir)
        && dir.join("material-icons.json").is_file()
        && version_key(&theirs) >= version_key(&ours)
    {
        return Ok(Installed::Current(theirs));
    }
    let parent = dir.parent().ok_or("no parent directory")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let pid = std::process::id();
    let tmp = parent.join(format!(".material-new-{pid}"));
    let old = parent.join(format!(".material-old-{pid}"));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let unpack = tar::Archive::new(flate2::read::GzDecoder::new(EMBEDDED)).unpack(&tmp);
    if let Err(e) = unpack {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!("unpacking the icons: {e}"));
    }
    // swap: the current one aside (if any), the new one in, the old one gone
    let had = dir.exists();
    if had && let Err(e) = std::fs::rename(dir, &old) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!("{}: {e}", dir.display()));
    }
    if let Err(e) = std::fs::rename(&tmp, dir) {
        if had {
            let _ = std::fs::rename(&old, dir);
        }
        let _ = std::fs::remove_dir_all(&tmp);
        // another program put a set in at the same moment: fine if it is as new
        if let Some(theirs) = read_version(dir)
            && version_key(&theirs) >= version_key(&ours)
        {
            return Ok(Installed::Current(theirs));
        }
        return Err(format!("{}: {e}", dir.display()));
    }
    if had {
        let _ = std::fs::remove_dir_all(&old);
    }
    Ok(Installed::Wrote(ours))
}

/// The theme's manifest: which icon a file / folder gets.
struct Manifest {
    dir: PathBuf,
    defs: HashMap<String, String>,
    file_names: HashMap<String, String>,
    file_exts: HashMap<String, String>,
    folder_names: HashMap<String, String>,
    folder_names_open: HashMap<String, String>,
    file: String,
    folder: String,
    folder_open: String,
}

fn str_map(v: &serde_json::Value, key: &str) -> HashMap<String, String> {
    v.get(key)
        .and_then(|m| m.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.to_lowercase(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

impl Manifest {
    fn load(dir: &Path) -> Option<Manifest> {
        let text = std::fs::read_to_string(dir.join("material-icons.json")).ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        let defs = v
            .get("iconDefinitions")?
            .as_object()?
            .iter()
            .filter_map(|(k, d)| {
                let p = d.get("iconPath")?.as_str()?;
                Some((k.clone(), p.rsplit('/').next()?.to_string()))
            })
            .collect();
        let s = |k: &str, dflt: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or(dflt).to_string();
        Some(Manifest {
            dir: dir.to_path_buf(),
            defs,
            file_names: str_map(&v, "fileNames"),
            file_exts: str_map(&v, "fileExtensions"),
            folder_names: str_map(&v, "folderNames"),
            folder_names_open: str_map(&v, "folderNamesExpanded"),
            file: s("file", "file"),
            folder: s("folder", "folder"),
            folder_open: s("folderExpanded", "folder-open"),
        })
    }

    /// The icon id for a path (lvim-icons' material.lua): a folder by its name; a file by its exact name,
    /// then each dotted tail as an extension, the longest first; then the generic one.
    fn icon_id(&self, path: &str, is_dir: bool, open: bool) -> String {
        let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or(path).to_lowercase();
        if is_dir {
            let (m, d) = if open {
                (&self.folder_names_open, &self.folder_open)
            } else {
                (&self.folder_names, &self.folder)
            };
            return m.get(&name).cloned().unwrap_or_else(|| d.clone());
        }
        if let Some(id) = self.file_names.get(&name) {
            return id.clone();
        }
        let parts: Vec<&str> = name.split('.').collect();
        for i in 1..parts.len() {
            let ext = parts[i..].join(".");
            if let Some(id) = self.file_exts.get(&ext) {
                return id.clone();
            }
        }
        self.file.clone()
    }

    fn svg_path(&self, id: &str) -> Option<PathBuf> {
        let f = self.defs.get(id)?;
        let p = self.dir.join(f);
        p.is_file().then_some(p)
    }
}

struct State {
    on: bool,
    cell: (u32, u32),
    manifest: Option<Manifest>,
    /// icon id → image id (sent)
    sent: HashMap<String, u32>,
    /// Bytes for the terminal, written before the next frame.
    pending: Vec<u8>,
    seq: u32,
    base: u32,
    /// the pictures of the previews (media): their ids, deleted on exit
    media: Vec<u32>,
    media_seq: u32,
    /// Counts the times the terminal lost its images ([`invalidate`]).
    epoch: u64,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| {
        let pid = std::process::id();
        let h = (pid ^ (pid >> 5) ^ (pid >> 10)) & 0x3FF;
        Mutex::new(State {
            on: false,
            cell: (0, 0),
            manifest: None,
            sent: HashMap::new(),
            pending: Vec::new(),
            seq: 0,
            base: h << 14,
            media: Vec::new(),
            media_seq: 0,
            epoch: 0,
        })
    })
}

/// Does the terminal take kitty placeholders?
fn terminal_takes_placeholders(zellij: bool) -> bool {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if env("TMUX").is_some() {
        return false; // no passthrough here
    }
    let kitty = env("KITTY_WINDOW_ID").is_some() || env("TERM").is_some_and(|t| t.contains("kitty"));
    let ghostty = env("GHOSTTY_RESOURCES_DIR").is_some()
        || env("GHOSTTY_BIN_DIR").is_some()
        || env("TERM_PROGRAM").is_some_and(|t| t.eq_ignore_ascii_case("ghostty"));
    if env("ZELLIJ").is_some() {
        return zellij && (kitty || ghostty);
    }
    kitty || ghostty
}

/// The terminal's cell size in pixels: `TIOCGWINSZ` on the controlling terminal.
fn cell_size_ioctl() -> Option<(u32, u32)> {
    let f = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").ok()?;
    use std::os::fd::AsRawFd;
    // SAFETY: TIOCGWINSZ writes a winsize into the struct we pass.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::ioctl(f.as_raw_fd(), libc::TIOCGWINSZ, &mut ws) };
    if r != 0 || ws.ws_xpixel == 0 || ws.ws_ypixel == 0 || ws.ws_col == 0 || ws.ws_row == 0 {
        return None;
    }
    Some((
        (ws.ws_xpixel / ws.ws_col) as u32,
        (ws.ws_ypixel / ws.ws_row) as u32,
    ))
}

/// The cell size by asking the terminal (`CSI 16 t` → `CSI 6;h;w t`), for when the ioctl has no pixels
/// (a multiplexer). Only in raw mode, before anything else reads the terminal.
fn cell_size_query() -> Option<(u32, u32)> {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    let mut f = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").ok()?;
    f.write_all(b"\x1b[16t").ok()?;
    f.flush().ok()?;
    let mut buf = Vec::new();
    let end = std::time::Instant::now() + std::time::Duration::from_millis(300);
    while std::time::Instant::now() < end {
        let mut pfd = libc::pollfd {
            fd: f.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll on one descriptor we own.
        let n = unsafe { libc::poll(&mut pfd, 1, 50) };
        if n <= 0 {
            continue;
        }
        let mut b = [0u8; 64];
        let k = f.read(&mut b).ok()?;
        buf.extend_from_slice(&b[..k]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\x1b[6;") {
            let rest = &buf[i + 4..];
            if let Some(t) = rest.iter().position(|&c| c == b't') {
                let s = std::str::from_utf8(&rest[..t]).ok()?;
                let (h, w) = s.split_once(';')?;
                let (h, w): (u32, u32) = (h.parse().ok()?, w.parse().ok()?);
                return (h > 0 && w > 0).then_some((w, h));
            }
        }
    }
    None
}

/// Turn images on or off. Call once, in raw mode, after the terminal is set up and before input is read
/// (it may ask the terminal its cell size). `zellij`: zellij's build accepts placeholders. Returns whether
/// icons are images.
pub fn init(mode: Mode, zellij: bool) -> bool {
    let mut st = state().lock().unwrap();
    st.on = false;
    if mode == Mode::Font || !terminal_takes_placeholders(zellij) {
        return false;
    }
    // the shared set: put ours there when it is missing or older
    let _ = install_shared();
    let Some(manifest) = material_dirs().iter().find_map(|d| Manifest::load(d)) else {
        return false;
    };
    let Some(cell) = cell_size_ioctl().or_else(cell_size_query) else {
        return false;
    };
    st.cell = cell;
    st.manifest = Some(manifest);
    st.on = true;
    true
}

pub fn enabled() -> bool {
    state().lock().unwrap().on
}

/// The cell size changed (a font change): re-measure, and re-send every image under its id so the cells
/// already on screen show the new pixels.
pub fn resized() {
    let mut st = state().lock().unwrap();
    if !st.on {
        return;
    }
    let Some(cell) = cell_size_ioctl() else { return };
    if cell == st.cell {
        return;
    }
    st.cell = cell;
    let ids: Vec<(String, u32)> = st.sent.iter().map(|(k, v)| (k.clone(), *v)).collect();
    for (icon, id) in ids {
        if let Some(p) = st.manifest.as_ref().and_then(|m| m.svg_path(&icon))
            && let Some(bytes) = transmit(&p, id, st.cell)
        {
            st.pending.extend(bytes);
        }
    }
}

/// The image id for a path's icon (sending it the first time), or None (images off / no SVG).
pub fn image_id(path: &str, is_dir: bool) -> Option<u32> {
    let mut st = state().lock().unwrap();
    if !st.on {
        return None;
    }
    let icon = st.manifest.as_ref()?.icon_id(path, is_dir, false);
    if let Some(&id) = st.sent.get(&icon) {
        return Some(id);
    }
    let svg = st.manifest.as_ref()?.svg_path(&icon)?;
    st.seq = st.seq % 0x3FFF + 1;
    let id = st.base | st.seq;
    let bytes = transmit(&svg, id, st.cell)?;
    st.pending.extend(bytes);
    st.sent.insert(icon, id);
    Some(id)
}

/// What to write to the terminal before the next frame (new images), then cleared.
pub fn take_pending() -> Vec<u8> {
    std::mem::take(&mut state().lock().unwrap().pending)
}

/// The terminal lost the images it was sent (the program left the alternate screen to run another program and came
/// back: kitty drops an alternate screen's images when the screen is switched). Forget what was sent, so every icon
/// is sent again the next time it is drawn, and bump [`epoch`] so a drawn picture is sent again too. Nothing is
/// deleted: the images are already gone.
pub fn invalidate() {
    let mut st = state().lock().unwrap();
    st.sent.clear();
    st.media.clear();
    st.epoch += 1;
}

/// How many times [`invalidate`] was called: a picture sent in an earlier epoch is gone from the terminal.
pub fn epoch() -> u64 {
    state().lock().unwrap().epoch
}

/// Delete every image sent (on exit).
pub fn cleanup() -> Vec<u8> {
    let mut st = state().lock().unwrap();
    let mut out = Vec::new();
    for id in st.sent.values().chain(st.media.iter()) {
        out.extend_from_slice(format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").as_bytes());
    }
    st.sent.clear();
    st.media.clear();
    out
}

/// The two placeholder cells (row 0, columns 0 and 1); their fg must be the image id (see `id_color`).
pub const CELLS: [&str; 2] = ["\u{10EEEE}\u{0305}\u{0305}", "\u{10EEEE}\u{0305}\u{030D}"];

/// The image id as the cells' truecolor foreground.
pub fn id_color(id: u32) -> crossterm::style::Color {
    crossterm::style::Color::Rgb {
        r: (id >> 16) as u8,
        g: (id >> 8) as u8,
        b: id as u8,
    }
}

/// Rasterise `svg` into the 2×1-cell canvas: the icon a square of 72% of the cell height, centred,
/// straight (not premultiplied) RGBA.
pub fn rasterize(svg: &Path, cell: (u32, u32)) -> Option<(Vec<u8>, u32, u32)> {
    use resvg::{tiny_skia, usvg};
    let (cw, ch) = cell;
    let (w, h) = (2 * cw, ch);
    let size = ((2 * cw) as f32).min(ch as f32 * 0.72).floor().max(1.0);
    let data = std::fs::read(svg).ok()?;
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).ok()?;
    let ts = tree.size();
    let scale = size / ts.width().max(ts.height());
    let tx = ((w as f32 - ts.width() * scale) / 2.0).floor();
    let ty = ((h as f32 - ts.height() * scale) / 2.0).floor();
    let mut pm = tiny_skia::Pixmap::new(w, h)?;
    resvg::render(&tree, tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, tx, ty), &mut pm.as_mut());
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for p in pm.pixels() {
        let c = p.demultiply();
        out.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some((out, w, h))
}

/// The kitty command that sends `svg` as image `id` with a 2×1 virtual placement (`a=T,U=1` together:
/// a separate transmit + place does not render through placeholders).
fn transmit(svg: &Path, id: u32, cell: (u32, u32)) -> Option<Vec<u8>> {
    let (rgba, w, h) = rasterize(svg, cell)?;
    let b64 = base64(&rgba);
    let mut out = Vec::new();
    let chunks: Vec<&[u8]> = b64.as_bytes().chunks(4096).collect();
    for (i, c) in chunks.iter().enumerate() {
        let more = if i + 1 < chunks.len() { 1 } else { 0 };
        if i == 0 {
            out.extend_from_slice(
                format!("\x1b_Ga=T,U=1,i={id},c=2,r=1,t=d,f=32,s={w},v={h},m={more},q=2;").as_bytes(),
            );
        } else {
            out.extend_from_slice(format!("\x1b_Gm={more},q=2;").as_bytes());
        }
        out.extend_from_slice(c);
        out.extend_from_slice(b"\x1b\\");
    }
    Some(out)
}

use crate::util::base64;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_the_shared_set_never_downgrading() {
        let base = std::env::temp_dir().join(format!("lvf-icons-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("lvim-icons/material");
        let ours = embedded_version().expect("embedded VERSION");
        // missing → written
        assert_eq!(install_into(&dir).unwrap(), Installed::Wrote(ours.clone()));
        assert!(dir.join("material-icons.json").is_file() && dir.join("rust.svg").is_file());
        // the same → left
        assert_eq!(install_into(&dir).unwrap(), Installed::Current(ours.clone()));
        // older → rewritten
        std::fs::write(dir.join("VERSION"), "0.1.0\n").unwrap();
        assert_eq!(install_into(&dir).unwrap(), Installed::Wrote(ours.clone()));
        // newer (another program's) → kept
        std::fs::write(dir.join("VERSION"), "999.0.0\n").unwrap();
        assert_eq!(install_into(&dir).unwrap(), Installed::Current("999.0.0".into()));
        assert!(!base.join("lvim-icons").read_dir().unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".material-")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn versions_compare_as_numbers() {
        assert!(version_key("5.40.0") > version_key("5.9.9"));
        assert!(version_key("5.40.1") > version_key("5.40"));
    }

    #[test]
    fn invalidating_bumps_the_epoch_and_forgets_what_was_sent() {
        let before = epoch();
        invalidate();
        assert_eq!(epoch(), before + 1);
        // nothing is sent (images are off in a test) and nothing is left to be taken
        assert!(state().lock().unwrap().sent.is_empty());
    }

    #[test]
    fn base64_is_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    fn material() -> Option<PathBuf> {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lvim-icons/assets/material");
        d.join("material-icons.json").is_file().then_some(d)
    }

    #[test]
    fn looks_icons_up_like_lvim_icons() {
        let Some(d) = material() else { return };
        let m = Manifest::load(&d).unwrap();
        assert_eq!(m.icon_id("src/main.rs", false, false), "rust");
        assert_ne!(m.icon_id("Cargo.toml", false, false), "file");
        assert_eq!(m.icon_id("package.json", false, false), m.file_names["package.json"]);
        assert_eq!(m.icon_id("x/src", true, false), "folder-src");
        assert_eq!(m.icon_id("x/src/", true, true), "folder-src-open");
        assert_eq!(m.icon_id("nothing.zzzq", false, false), "file");
        assert!(m.svg_path("rust").is_some());
    }

    #[test]
    fn rasterizes_into_two_cells() {
        let Some(d) = material() else { return };
        let (rgba, w, h) = rasterize(&d.join("rust.svg"), (10, 20)).unwrap();
        assert_eq!((w, h), (20, 20));
        assert_eq!(rgba.len(), 20 * 20 * 4);
        // drawn in the middle, transparent at the corners
        assert_eq!(rgba[3], 0);
        let mid = ((10 * w + 10) * 4) as usize;
        assert!(rgba[mid + 3] > 0, "the icon covers the centre");
        let cmd = transmit(&d.join("rust.svg"), 7, (10, 20)).unwrap();
        let s = String::from_utf8_lossy(&cmd);
        assert!(s.starts_with("\x1b_Ga=T,U=1,i=7,c=2,r=1,t=d,f=32,s=20,v=20,m="));
    }
}

/// Are images shown through kitty placeholders (`init` found a terminal that takes them)?
pub fn placeholders() -> bool {
    state().lock().unwrap().on
}

/// The cell size in pixels (0 × 0 when unknown).
pub fn cell_size() -> (u32, u32) {
    state().lock().unwrap().cell
}

/// A fresh id for a preview's picture (a range of its own beside the icons'), deleted on exit.
pub fn new_media_id() -> u32 {
    let mut st = state().lock().unwrap();
    st.media_seq = st.media_seq % 0x3FFF + 1;
    // the icons use base | 1..0x3FFF; the pictures the next pid-hash slot's range
    let id = ((st.base >> 14 ^ 0x200) & 0x3FF) << 14 | st.media_seq;
    st.media.push(id);
    id
}

/// Forget a preview picture (its delete command is the caller's to send).
pub fn drop_media_id(id: u32) {
    state().lock().unwrap().media.retain(|&m| m != id);
}

/// Bytes for the terminal before the next frame (a preview's picture, its delete).
pub fn push_pending(bytes: &[u8]) {
    state().lock().unwrap().pending.extend_from_slice(bytes);
}

/// The kitty row / column diacritics: placeholder cell (row r, column c) is U+10EEEE + DIACRITICS[r] +
/// DIACRITICS[c].
pub const DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1,
    0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8,
    0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB,
    0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954,
    0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76,
    0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71,
    0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
    0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1,
    0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7,
    0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2,
    0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE,
    0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1,
    0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED,
    0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1,
    0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186, 0x1D187,
    0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

//! Previews of what is not text: images, SVG, video (a frame), PDF (the first page), fonts (a sample),
//! audio (its cover and tags) — a picture (RGBA) plus lines about the file.
//!
//! The picture is made by this crate where it can (`image` for the raster formats, `resvg` for SVG) and
//! by the usual programs where it cannot (`ffmpegthumbnailer` / `ffmpeg`, `pdftoppm`, `fontimage`), each
//! optional: without one the preview is its lines alone. Everything here blocks — call it off the UI
//! thread and cache the result.
//!
//! Showing it: [`transmit`] + [`crate::canvas::Canvas::put_image`] (kitty graphics through unicode
//! placeholders, which survive a multiplexer and a diff renderer), or [`crate::canvas::Canvas::put_halfblocks`]
//! anywhere else (two pixels a cell, `▀` in truecolor).

use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Svg,
    Video,
    Pdf,
    Font,
    Audio,
    /// office documents: through LibreOffice to PDF, then its first page
    Document,
}

/// The extensions of each kind: the ONE list — `kind_of` here, and the preview plugin's default `[config.types.*]`
/// groups are made from it.
pub const EXTENSIONS: &[(Kind, &[&str])] = &[
    (
        Kind::Image,
        &[
            "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "ico", "avif", "heic", "heif", "jxl", "pnm", "ppm", "pgm",
            "pbm", "tga", "qoi",
        ],
    ),
    (Kind::Svg, &["svg", "svgz"]),
    (Kind::Video, &["mp4", "mkv", "webm", "mov", "avi", "m4v", "wmv", "flv", "mpg", "mpeg", "ts", "3gp", "ogv"]),
    (Kind::Pdf, &["pdf"]),
    (Kind::Font, &["ttf", "otf", "woff", "woff2", "ttc", "pfb"]),
    (Kind::Audio, &["mp3", "flac", "ogg", "opus", "m4a", "wav", "aac", "wma", "alac", "aiff", "ape"]),
    (
        Kind::Document,
        &["docx", "doc", "odt", "rtf", "xlsx", "xls", "ods", "pptx", "ppt", "odp", "fodt", "docm", "xlsm"],
    ),
];

/// The name of a kind (the JSON of `lvim-fuzzy-cli preview`).
pub fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Image => "image",
        Kind::Svg => "svg",
        Kind::Video => "video",
        Kind::Pdf => "pdf",
        Kind::Font => "font",
        Kind::Audio => "audio",
        Kind::Document => "document",
    }
}

/// What kind of media `path` is, by its extension (None: not media — text, an archive, a binary).
pub fn kind_of(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    EXTENSIONS.iter().find(|(_, exts)| exts.contains(&ext.as_str())).map(|(k, _)| *k)
}

/// The preview of the media file `path` as JSON, for the preview plugin: the kind, the lines about the file
/// (`info`: label, value), how many positions it has (`positions`), what this machine lacks (`missing`) and the
/// picture as a PNG file in the cache (`picture`: path, width, height), fitted into `max` pixels. None: not media.
pub fn describe_json(path: &Path, max: (u32, u32), skip: u32) -> Option<serde_json::Value> {
    let kind = kind_of(path)?;
    let m = load(path, kind, max, skip);
    let skip = if matches!(kind, Kind::Pdf | Kind::Document | Kind::Video) { skip } else { 0 };
    let png = cache_file(path, "pic", skip, max, "png").filter(|p| p.is_file());
    Some(serde_json::json!({
        "kind": kind_name(kind),
        "info": m.info,
        "positions": m.positions,
        "missing": m.missing,
        "picture": match (&png, &m.picture) {
            (Some(p), Some((_, w, h))) => serde_json::json!({ "path": p, "width": w, "height": h }),
            _ => serde_json::Value::Null,
        },
    }))
}

/// A preview: the picture (straight RGBA, width, height) when there is one, and lines about the file.
#[derive(Clone, Debug, Default)]
pub struct Media {
    pub picture: Option<(Vec<u8>, u32, u32)>,
    /// (label, value): the value's parts are separated by `  ·  `
    pub info: Vec<(String, String)>,
    /// how many positions `skip` can take (PDF / document pages, video steps): seek stops at the last
    pub positions: u32,
    /// What this machine lacks for a full preview of this file (a program that is not installed): one line each,
    /// shown under the facts.
    pub missing: Vec<String>,
}

fn have(prog: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(prog).is_file()))
}

fn output(prog: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(prog).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

fn run(prog: &str, args: &[&str]) -> bool {
    Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A temporary file for a program's picture, removed when dropped.
struct Temp(std::path::PathBuf);

impl Temp {
    fn new(ext: &str) -> Temp {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        Temp(std::env::temp_dir().join(format!("lvim-media-{}-{n}.{ext}", std::process::id())))
    }
    fn s(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Straight RGBA, its width and height, and the source's own size.
type Decoded = (Vec<u8>, u32, u32, (u32, u32));

/// Decode a raster file and fit it into `max` pixels (never enlarged), straight RGBA.
fn decode(path: &Path, max: (u32, u32)) -> Option<Decoded> {
    // turned as the camera meant it (EXIF orientation)
    let mut dec = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.into_decoder().ok()?;
    let orient = image::ImageDecoder::orientation(&mut dec).ok();
    let mut img = image::DynamicImage::from_decoder(dec).ok()?;
    if let Some(o) = orient {
        img.apply_orientation(o);
    }
    let orig = (img.width(), img.height());
    let img = if orig.0 > max.0 || orig.1 > max.1 {
        // the fast box filter: a preview, not a print
        img.thumbnail(max.0.max(1), max.1.max(1))
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some((rgba.into_raw(), w, h, orig))
}

fn svg(path: &Path, max: (u32, u32)) -> Option<Decoded> {
    let data = std::fs::read(path).ok()?;
    let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
    let size = tree.size();
    let orig = (size.width().ceil() as u32, size.height().ceil() as u32);
    let scale = (max.0 as f32 / size.width()).min(max.1 as f32 / size.height());
    let (w, h) = ((size.width() * scale).round().max(1.0) as u32, (size.height() * scale).round().max(1.0) as u32);
    let mut pm = resvg::tiny_skia::Pixmap::new(w, h)?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pm.as_mut());
    let mut px = pm.take();
    // tiny-skia is premultiplied; the terminals want straight alpha
    for p in px.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    Some((px, w, h, orig))
}

fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < U.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", U[i]) }
}

/// Lines about a video / audio file from ffprobe: format, length, each stream, the tags.
fn probe(path: &str) -> Vec<(String, String)> {
    if !have("ffprobe") {
        return Vec::new();
    }
    let Some(out) = output("ffprobe", &["-v", "error", "-show_format", "-show_streams", "-of", "json", path]) else {
        return Vec::new();
    };
    let Ok(j) = serde_json::from_str::<serde_json::Value>(&out) else {
        return Vec::new();
    };
    let s = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let n = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_u64().or_else(|| x.as_str()?.parse().ok()));
    let mut lines = Vec::new();
    let fmt = &j["format"];
    if let Some(f) = s(fmt, "format_long_name") {
        lines.push(("format".into(), f));
    }
    if let Some(d) = s(fmt, "duration").and_then(|d| d.parse::<f64>().ok()) {
        let t = d as u64;
        let len = if t >= 3600 { format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60) } else { format!("{}:{:02}", t / 60, t % 60) };
        let br = n(fmt, "bit_rate").map(|b| format!("  ·  {} kb/s", b / 1000)).unwrap_or_default();
        lines.push(("length".into(), format!("{len}{br}")));
    }
    for st in j["streams"].as_array().into_iter().flatten() {
        let codec = s(st, "codec_name").unwrap_or_default();
        match s(st, "codec_type").as_deref() {
            Some("video") if st["disposition"]["attached_pic"].as_u64() != Some(1) => {
                let fps = s(st, "avg_frame_rate")
                    .and_then(|r| {
                        let (a, b) = r.split_once('/')?;
                        let f = a.parse::<f64>().ok()? / b.parse::<f64>().ok()?.max(1.0);
                        (f > 0.0).then(|| format!("  ·  {f:.2} fps"))
                    })
                    .unwrap_or_default();
                lines.push((
                    "video".into(),
                    format!("{codec}  ·  {} × {}{fps}", n(st, "width").unwrap_or(0), n(st, "height").unwrap_or(0)),
                ));
            }
            Some("audio") => {
                let hz = n(st, "sample_rate").map(|r| format!("  ·  {r} Hz")).unwrap_or_default();
                let ch = n(st, "channels").map(|c| format!("  ·  {c} ch")).unwrap_or_default();
                lines.push(("audio".into(), format!("{codec}{hz}{ch}")));
            }
            Some("subtitle") => lines.push(("subtitle".into(), codec)),
            _ => {}
        }
    }
    if let Some(tags) = fmt["tags"].as_object() {
        for key in ["title", "artist", "album", "date", "genre"] {
            if let Some(v) = tags.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).and_then(|(_, v)| v.as_str()) {
                lines.push((key.into(), v.to_string()));
            }
        }
    }
    lines
}

/// The first page of a PDF as a picture, and its pdfinfo lines.
fn pdf_page(
    pdf: &str,
    page: u32,
    max: (u32, u32),
    big: (u32, u32),
    info: &mut Vec<(String, String)>,
    pages: &mut u32,
) -> Option<(Vec<u8>, u32, u32)> {
    if have("pdfinfo")
        && let Some(out) = output("pdfinfo", &[pdf])
    {
        for l in out.lines() {
            let Some((k, v)) = l.split_once(':') else { continue };
            let v = v.trim();
            if k == "Pages" {
                *pages = v.parse().unwrap_or(1);
                continue;
            }
            if !v.is_empty() && matches!(k, "Title" | "Author" | "Page size" | "Producer" | "CreationDate") {
                let k = match k {
                    "CreationDate" => "created".to_string(),
                    k => k.to_lowercase(),
                };
                info.push((k, v.to_string()));
            }
        }
    }
    let page = page.clamp(1, (*pages).max(1));
    info.insert(0, ("page".into(), format!("{page} / {}", (*pages).max(1))));
    let t = Temp::new("png");
    let stem = t.0.with_extension("");
    let pg = page.to_string();
    let ok = have("pdftoppm")
        && run(
            "pdftoppm",
            &["-png", "-f", &pg, "-l", &pg, "-singlefile", "-scale-to", &big.0.max(big.1).to_string(), pdf, &stem.to_string_lossy()],
        );
    if ok { decode(&t.0, max).map(|(px, w, h, _)| (px, w, h)) } else { None }
}

/// An office document as PDF (LibreOffice, headless), in a temporary directory removed with the result.
/// One at a time (LibreOffice does not take two conversions at once), with a profile of this process's.
fn office_pdf(path: &Path) -> Option<(std::path::PathBuf, Temp)> {
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let prog = ["soffice", "libreoffice"].into_iter().find(|p| have(p))?;
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    // converted once per version of the file: kept in the cache (every page is read from it)
    if let Some(c) = cache_file(path, "office", 0, (0, 0), "pdf")
        && c.is_file()
    {
        return Some((c, Temp::new("none")));
    }
    let out = Temp::new("d");
    std::fs::create_dir_all(&out.0).ok()?;
    let profile = std::env::temp_dir().join(format!("lvim-media-soffice-{}", std::process::id()));
    let ok = run(
        prog,
        &[
            &format!("-env:UserInstallation=file://{}", profile.display()),
            "--headless",
            "--convert-to",
            "pdf",
            "--outdir",
            &out.s(),
            &path.to_string_lossy(),
        ],
    );
    let pdf = out.0.join(path.file_stem()?).with_extension("pdf");
    if !(ok && pdf.is_file()) {
        return None;
    }
    if let Some(c) = cache_file(path, "office", 0, (0, 0), "pdf")
        && std::fs::rename(&pdf, &c).is_ok()
    {
        let _ = std::fs::remove_dir(&out.0);
        return Some((c, Temp::new("none")));
    }
    Some((pdf, out))
}

/// The cache of previews (survives restarts): `$XDG_CACHE_HOME/lvim-media/`.
pub fn cache_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"))
        .join("lvim-media")
}

/// Keep the cache under `limit` bytes: the least recently written files go first.
pub fn prune_cache(limit: u64) {
    let Ok(rd) = std::fs::read_dir(cache_dir()) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, std::path::PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| (m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    for (_, len, p) in files {
        if total <= limit {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total -= len;
        }
    }
}

/// A cache file for `path` as it is now (size and times in the key: a changed file is a new key), what
/// (`what`), the position and the size.
fn cache_file(path: &Path, what: &str, skip: u32, max: (u32, u32), ext: &str) -> Option<std::path::PathBuf> {
    use std::hash::{Hash, Hasher};
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (std::path::absolute(path).ok()?, m.len(), m.mtime(), m.mtime_nsec(), m.ctime(), what, skip, max).hash(&mut h);
    let d = cache_dir();
    std::fs::create_dir_all(&d).ok()?;
    Some(d.join(format!("{:016x}.{ext}", h.finish())))
}

/// The preview of `path` (of kind `kind`) at position `skip` (a page, a point of a video), the picture fitted
/// into `max` pixels — from the disk cache when it was made before for this version of the file.
pub fn load(path: &Path, kind: Kind, max: (u32, u32), skip: u32) -> Media {
    let mut m = load_cached(path, kind, max, skip);
    m.missing = missing(kind, path);
    m
}

/// What `kind` of file needs that is not installed here — the programs the preview calls (a picture, SVG and the
/// text previews need none). One short line each, for the host to show under the facts.
pub fn missing(kind: Kind, path: &Path) -> Vec<String> {
    missing_with(kind, path, &have)
}

/// [`missing`] with the question "is this program installed?" given (a test says what is installed).
pub fn missing_with(kind: Kind, path: &Path, have: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut v = Vec::new();
    let any = |ps: &[&str]| ps.iter().any(|p| have(p));
    match kind {
        Kind::Image => {
            let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            if matches!(ext.as_str(), "avif" | "heic" | "heif" | "jxl") && !have("magick") {
                v.push(format!(".{ext} pictures need ImageMagick (magick)"));
            }
        }
        Kind::Svg => {}
        Kind::Video => {
            if !any(&["ffmpegthumbnailer", "ffmpeg"]) {
                v.push("a video frame needs ffmpegthumbnailer or ffmpeg".into());
            }
            if !have("ffprobe") {
                v.push("video details need ffprobe (ffmpeg)".into());
            }
        }
        Kind::Audio => {
            if !have("ffprobe") {
                v.push("audio details need ffprobe (ffmpeg)".into());
            }
            if !have("ffmpeg") {
                v.push("the cover picture needs ffmpeg".into());
            }
        }
        Kind::Pdf | Kind::Document => {
            if kind == Kind::Document && !any(&["soffice", "libreoffice"]) {
                v.push("a document preview needs LibreOffice (soffice)".into());
            }
            if !have("pdftoppm") {
                v.push("a page picture needs pdftoppm (poppler)".into());
            }
            if !have("pdfinfo") {
                v.push("page count and details need pdfinfo (poppler)".into());
            }
        }
        Kind::Font => {
            if !have("fc-scan") {
                v.push("font details need fc-scan (fontconfig)".into());
            }
            if !have("fontimage") {
                v.push("the font specimen needs fontimage (fontforge)".into());
            }
        }
    }
    v
}

fn load_cached(path: &Path, kind: Kind, max: (u32, u32), skip: u32) -> Media {
    let skip = if matches!(kind, Kind::Pdf | Kind::Document | Kind::Video) { skip } else { 0 };
    let png = cache_file(path, "pic", skip, max, "png");
    let meta = png.as_ref().map(|p| p.with_extension("json"));
    if let Some(meta) = &meta
        && let Ok(text) = std::fs::read_to_string(meta)
        && let Ok(j) = serde_json::from_str::<serde_json::Value>(&text)
    {
        let info = j["info"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| Some((p[0].as_str()?.to_string(), p[1].as_str()?.to_string())))
            .collect();
        let positions = j["positions"].as_u64().unwrap_or(1) as u32;
        let picture = png.as_ref().filter(|p| p.is_file()).and_then(|p| decode(p, max)).map(|(px, w, h, _)| (px, w, h));
        return Media { picture, info, positions, missing: Vec::new() };
    }
    let m = compute(path, kind, max, skip);
    // into the cache: the picture as PNG, the lines as JSON (written last: its presence means "complete")
    if let (Some(png), Some(meta)) = (&png, &meta) {
        if let Some((px, w, h)) = &m.picture {
            let _ = image::RgbaImage::from_raw(*w, *h, px.clone()).map(|i| i.save(png));
        }
        let j = serde_json::json!({ "info": m.info, "positions": m.positions });
        let _ = std::fs::write(meta, j.to_string());
    }
    m
}

fn compute(path: &Path, kind: Kind, max: (u32, u32), skip: u32) -> Media {
    let mut positions = 1u32;
    let ps = path.to_string_lossy().into_owned();
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    // the programs render at a size worth looking at; `decode` fits it into `max` after
    let big = (max.0.max(800), max.1.max(600));
    let mut info = Vec::new();
    let picture = match kind {
        Kind::Image => {
            let d = decode(path, max).or_else(|| {
                // a format the crate does not decode (HEIC, AVIF, JXL...): through ImageMagick
                let t = Temp::new("png");
                let geo = format!("{}x{}>", max.0, max.1);
                (have("magick") && run("magick", &[&format!("{ps}[0]"), "-resize", &geo, &t.s()]))
                    .then(|| decode(&t.0, max))
                    .flatten()
            });
            if let Some((_, _, _, (ow, oh))) = &d {
                let fmt = path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
                info.push(("dimensions".into(), format!("{ow} × {oh}")));
                info.push(("format".into(), fmt));
            }
            d.map(|(px, w, h, _)| (px, w, h))
        }
        Kind::Svg => {
            let d = svg(path, max);
            if let Some((_, _, _, (ow, oh))) = &d {
                info.push(("dimensions".into(), format!("{ow} × {oh}")));
                info.push(("format".into(), "SVG".into()));
            }
            d.map(|(px, w, h, _)| (px, w, h))
        }
        Kind::Video => {
            info.extend(probe(&ps));
            let t = Temp::new("png");
            let side = big.0.max(big.1).to_string();
            // the position: 5 % and 5 % a step (J / K), up to 95 %
            positions = 19;
            let pct = (5 + skip.min(18) * 5).to_string() + "%";
            info.insert(0, ("position".into(), pct.clone()));
            let ok = (have("ffmpegthumbnailer") && run("ffmpegthumbnailer", &["-i", &ps, "-o", &t.s(), "-s", &side, "-t", &pct]))
                || (have("ffmpeg")
                    && run(
                        "ffmpeg",
                        &["-v", "error", "-y", "-ss", "3", "-i", &ps, "-frames:v", "1", "-vf", &format!("scale={}:-1", big.0), &t.s()],
                    ));
            if ok { decode(&t.0, max).map(|(px, w, h, _)| (px, w, h)) } else { None }
        }
        Kind::Audio => {
            info.extend(probe(&ps));
            // the embedded cover, when there is one
            let t = Temp::new("png");
            let ok = have("ffmpeg") && run("ffmpeg", &["-v", "error", "-y", "-i", &ps, "-an", "-frames:v", "1", &t.s()]);
            if ok { decode(&t.0, max).map(|(px, w, h, _)| (px, w, h)) } else { None }
        }
        Kind::Pdf => pdf_page(&ps, skip + 1, max, big, &mut info, &mut positions),
        Kind::Document => {
            let fmt = path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
            info.push(("format".into(), fmt));
            match office_pdf(path) {
                Some((pdf, dir)) => {
                    let pic = pdf_page(&pdf.to_string_lossy(), skip + 1, max, big, &mut info, &mut positions);
                    // those come from the conversion (LibreOffice, now), not from the document
                    info.retain(|(k, _)| k != "producer" && k != "created");
                    if pdf.parent() != Some(cache_dir().as_path()) {
                        let _ = std::fs::remove_file(&pdf);
                        let _ = std::fs::remove_dir(&dir.0);
                    }
                    pic
                }
                None => None,
            }
        }
        Kind::Font => {
            if have("fc-scan")
                && let Some(out) = output("fc-scan", &["--format", "%{family[0]}\\n%{style[0]}\\n%{fontformat}\\n", &ps])
            {
                let mut it = out.lines();
                let fam = it.next().unwrap_or("");
                let style = it.next().unwrap_or("");
                let fmt = it.next().unwrap_or("");
                if !fam.is_empty() {
                    info.push(("family".into(), fam.to_string()));
                }
                if !style.is_empty() {
                    info.push(("style".into(), style.to_string()));
                }
                if !fmt.is_empty() {
                    info.push(("format".into(), fmt.to_string()));
                }
            }
            let t = Temp::new("png");
            let ok = have("fontimage")
                && run(
                    "fontimage",
                    &[
                        "-o",
                        &t.s(),
                        "--width",
                        &big.0.to_string(),
                        "--pixelsize",
                        "40",
                        "--text",
                        "ABCDEFGHIJKLM",
                        "--text",
                        "abcdefghijklm",
                        "--pixelsize",
                        "28",
                        "--text",
                        "0123456789 !?&@ -> => <= ==",
                        "--text",
                        "Съешь же ещё этих мягких булок",
                        "--pixelsize",
                        "20",
                        "--text",
                        "The quick brown fox jumps over the lazy dog",
                        &ps,
                    ],
                );
            if ok { decode(&t.0, max).map(|(px, w, h, _)| (px, w, h)) } else { None }
        }
    };
    if picture.is_none() && info.is_empty() {
        info.push(("preview".into(), "none (a program for it is missing?)".into()));
    }
    // the file's size, last, for every kind
    info.push(("file size".into(), human(size)));
    Media { picture, info, positions, missing: Vec::new() }
}

/// The kitty command sending `rgba` (w × h) as image `id`, placed virtually over `cols` × `rows` cells
/// (shown through [`crate::canvas::Canvas::put_image`]).
pub fn transmit(id: u32, rgba: &[u8], w: u32, h: u32, cols: u16, rows: u16) -> Vec<u8> {
    // as little as possible goes through the terminal (and a multiplexer): RGB when nothing is see-through,
    // zlib-compressed (fast level: the time is in the bytes, not the squeezing)
    let opaque = rgba.chunks_exact(4).all(|p| p[3] == 255);
    let raw: Vec<u8> = if opaque { rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect() } else { rgba.to_vec() };
    let fmt = if opaque { 24 } else { 32 };
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    let packed = std::io::Write::write_all(&mut z, &raw).ok().and_then(|_| z.finish().ok());
    let (data, o) = match packed {
        Some(p) if p.len() < raw.len() => (p, ",o=z"),
        _ => (raw, ""),
    };
    let b64 = crate::util::base64(&data);
    let mut out = Vec::new();
    let chunks: Vec<&[u8]> = b64.as_bytes().chunks(4096).collect();
    for (i, c) in chunks.iter().enumerate() {
        let more = if i + 1 < chunks.len() { 1 } else { 0 };
        if i == 0 {
            out.extend_from_slice(
                format!("\x1b_Ga=T,U=1,i={id},c={cols},r={rows},t=d,f={fmt}{o},s={w},v={h},m={more},q=2;").as_bytes(),
            );
        } else {
            out.extend_from_slice(format!("\x1b_Gm={more},q=2;").as_bytes());
        }
        out.extend_from_slice(c);
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

/// Can the terminal read our files (it runs on this machine)? Not over ssh; `LVIM_MEDIA_DIRECT=1` forces
/// sending the bytes.
fn local_terminal() -> bool {
    std::env::var_os("SSH_CONNECTION").is_none()
        && std::env::var_os("SSH_TTY").is_none()
        && std::env::var("LVIM_MEDIA_DIRECT").map_or(true, |v| v.is_empty() || v == "0")
}

/// Like [`transmit`], but the pixels go through a temporary file the terminal reads (and deletes) itself
/// (`t=t`): a few dozen bytes through the terminal and a multiplexer instead of megabytes. Falls back to
/// [`transmit`] over ssh or when the file cannot be written.
pub fn transmit_fast(id: u32, rgba: &[u8], w: u32, h: u32, cols: u16, rows: u16) -> Vec<u8> {
    if !local_terminal() {
        return transmit(id, rgba, w, h, cols, rows);
    }
    let opaque = rgba.chunks_exact(4).all(|p| p[3] == 255);
    let raw: Vec<u8> = if opaque { rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect() } else { rgba.to_vec() };
    let fmt = if opaque { 24 } else { 32 };
    // kitty takes a temporary file only from a temp directory and with this marker in its name
    let path = std::env::temp_dir().join(format!("lvim-tty-graphics-protocol-{}-{id}.rgb", std::process::id()));
    if std::fs::write(&path, &raw).is_err() {
        return transmit(id, rgba, w, h, cols, rows);
    }
    let p64 = crate::util::base64(path.to_string_lossy().as_bytes());
    format!("\x1b_Ga=T,U=1,i={id},c={cols},r={rows},t=t,f={fmt},s={w},v={h},q=2;{p64}\x1b\\").into_bytes()
}

/// Remove the temporary files of [`transmit_fast`] a terminal did not take (on exit).
pub fn cleanup_files() {
    // the LibreOffice profile of this process (documents)
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("lvim-media-soffice-{}", std::process::id())));
    let prefix = format!("lvim-tty-graphics-protocol-{}-", std::process::id());
    if let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// The kitty command deleting image `id` (and its placements).
pub fn delete(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").into_bytes()
}

/// How many cells `w` × `h` pixels take with cells of `cell` pixels (at least 1 × 1).
pub fn cells_for(w: u32, h: u32, cell: (u32, u32)) -> (u16, u16) {
    let (cw, ch) = (cell.0.max(1), cell.1.max(1));
    (w.div_ceil(cw).max(1) as u16, h.div_ceil(ch).max(1) as u16)
}

#[cfg(test)]
mod tests {
    #[test]
    fn says_what_is_missing() {
        let none = |_: &str| false;
        let all = |_: &str| true;
        let only = |names: &'static [&'static str]| move |p: &str| names.contains(&p);
        let p = Path::new("x");
        // everything installed: nothing to say
        for k in [Kind::Image, Kind::Svg, Kind::Video, Kind::Audio, Kind::Pdf, Kind::Document, Kind::Font] {
            assert!(missing_with(k, p, &all).is_empty(), "{k:?}");
        }
        // nothing installed
        assert_eq!(missing_with(Kind::Video, p, &none).len(), 2);
        assert_eq!(missing_with(Kind::Pdf, p, &none).len(), 2);
        assert_eq!(missing_with(Kind::Document, p, &none).len(), 3);
        assert_eq!(missing_with(Kind::Font, p, &none).len(), 2);
        assert!(missing_with(Kind::Image, p, &none).is_empty()); // the crate decodes png / jpeg ...
        assert!(missing_with(Kind::Image, Path::new("a.HEIC"), &none)[0].contains("ImageMagick"));
        // one of two thumbnailers is enough; the details still say ffprobe
        let m = missing_with(Kind::Video, p, &only(&["ffmpeg"]));
        assert_eq!(m, vec!["video details need ffprobe (ffmpeg)".to_string()]);
        assert!(missing_with(Kind::Document, p, &only(&["soffice", "pdftoppm", "pdfinfo"])).is_empty());
    }

    use super::*;

    #[test]
    fn kinds_by_extension() {
        assert_eq!(kind_of(Path::new("a/b.PNG")), Some(Kind::Image));
        assert_eq!(kind_of(Path::new("x.mkv")), Some(Kind::Video));
        assert_eq!(kind_of(Path::new("doc.pdf")), Some(Kind::Pdf));
        assert_eq!(kind_of(Path::new("f.woff2")), Some(Kind::Font));
        assert_eq!(kind_of(Path::new("s.flac")), Some(Kind::Audio));
        assert_eq!(kind_of(Path::new("main.rs")), None);
    }

    #[test]
    fn decodes_and_fits_a_png() {
        let p = std::env::temp_dir().join(format!("lvim-media-test-{}.png", std::process::id()));
        image::RgbaImage::from_pixel(400, 200, image::Rgba([10, 20, 30, 255])).save(&p).unwrap();
        let m = compute(&p, Kind::Image, (100, 100), 0);
        let (px, w, h) = m.picture.expect("decoded");
        assert_eq!((w, h), (100, 50));
        assert_eq!(&px[..4], &[10, 20, 30, 255]);
        assert_eq!(m.info[0], ("dimensions".to_string(), "400 × 200".to_string()));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn cells_round_up() {
        assert_eq!(cells_for(100, 41, (10, 20)), (10, 3));
    }
}

#[cfg(test)]
mod timing {
    #[test]
    #[ignore]
    fn time_a_photo() {
        let p = std::path::Path::new(&std::env::var("LVF_PHOTO").unwrap()).to_path_buf();
        let t = std::time::Instant::now();
        let m = super::load(&p, super::Kind::Image, (1200, 900), 0);
        let (px, w, h) = m.picture.unwrap();
        let t1 = t.elapsed();
        let tx = super::transmit(1, &px, w, h, 100, 40);
        eprintln!("load {t1:?}  transmit {} bytes  build {:?}", tx.len(), t.elapsed() - t1);
    }
}

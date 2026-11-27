//! What a preview PROVIDER (a plugin: `lvim-fuzzy-plugin-preview`, a user's own) supplies, and how it is drawn. The plugin
//! COMPUTES — the facts about the file, a listing, a table, the programs it lacked — and answers with one
//! `preview` instruction; this module reads that answer and the ui DRAWS it, the same in lvc and lfz (the Neovim
//! picker draws the same answer in its own buffers). A plugin never draws.
//!
//! ```json
//! {"op":"preview",
//!  "facts":[["page","1 / 3"]],                       // a table of facts, shown on top
//!  "missing":["a page picture needs pdftoppm"],      // what this machine lacks, red, under the facts
//!  "positions":3,                                    // how many positions it has (pages, video points)
//!  "picture":{"path":"/cache/x.png","width":900,"height":700},
//!  "body":{"type":"entries","rows":[{"name":"a","dir":false,"size":3,"mtime":0}]}}
//!   // body: entries | table {rows:[[..]]} | tree {rows:[{depth,name,dir,size}]} | text {lines:[..], ansi:bool}
//!   // "renderer": "nvim" | "builtin" — no body: the tool shows the file itself, with Neovim or its own highlighter
//! {"op":"pass"}                                      // not mine — the next provider may have it
//! ```

use serde_json::Value;

use crate::archive::Row;
use crate::canvas::{Canvas, Rect, str_width, truncate};
use crate::fsinfo::{Entry, Kind};
use crate::listview;
use crate::lvim::Look;
use crate::theme::Style;
use crate::{iconimg, media};
use std::time::{Duration, Instant};

/// A picture file the provider made (a PNG in its cache).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pic {
    pub path: String,
    pub width: u32,
    pub height: u32,
}

/// The main part of a preview, under the facts.
#[derive(Clone, Debug)]
pub enum Body {
    None,
    Entries(Vec<Entry>),
    Table(Vec<Vec<String>>),
    Tree(Vec<Row>),
    /// Lines of text; `true` = they carry ANSI colours (what `bat` prints).
    Text(Vec<String>, bool),
}

#[derive(Clone, Debug)]
pub struct Preview {
    pub facts: Vec<(String, String)>,
    pub missing: Vec<String>,
    pub positions: u32,
    pub picture: Option<Pic>,
    pub body: Body,
    /// "nvim" or "builtin": the provider has no body, it tells the tool HOW to show this file (Neovim, or the
    /// tool's own highlighter); the tool does it. The Neovim picker follows it too.
    pub renderer: Option<String>,
}

/// A provider's answer.
#[derive(Clone, Debug)]
pub enum Answer {
    Preview(Preview),
    /// Not this provider's file: ask the next.
    Pass,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn entry_of(r: &Value) -> Entry {
    let b = |k: &str| r.get(k).and_then(Value::as_bool).unwrap_or(false);
    let dir = b("dir");
    let link = b("link");
    let kind = match (dir, link) {
        (true, true) => Kind::LinkDir,
        (true, false) => Kind::Dir,
        (false, true) => Kind::Link,
        _ => Kind::File,
    };
    Entry {
        name: s(r, "name"),
        kind,
        size: r.get("size").and_then(Value::as_u64).unwrap_or(0),
        mtime: r.get("mtime").and_then(Value::as_i64).unwrap_or(0),
        mode: if b("exec") { 0o755 } else { 0o644 },
        uid: 0,
        gid: 0,
        target: None,
        broken: b("broken"),
    }
}

/// Read a plugin's answer (the JSON array of instructions it returned): the first `preview` (or `pass`) in it.
/// A plugin that answers with neither is told so (`Err`).
pub fn parse(answer: &str) -> Result<Answer, String> {
    let v: Value = serde_json::from_str(answer).map_err(|e| format!("bad JSON: {e}"))?;
    let list = v.as_array().ok_or("the answer must be an array of instructions")?;
    for ins in list {
        match ins.get("op").and_then(Value::as_str) {
            Some("pass") => return Ok(Answer::Pass),
            Some("preview") => return Ok(Answer::Preview(preview_of(ins))),
            _ => {}
        }
    }
    Err("the answer has no `preview` (or `pass`) instruction".into())
}

fn preview_of(ins: &Value) -> Preview {
    let facts = ins
        .get("facts")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|r| Some((r.get(0)?.as_str()?.to_string(), r.get(1)?.as_str()?.to_string()))).collect())
        .unwrap_or_default();
    let missing = ins.get("missing").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
    let picture = ins.get("picture").filter(|p| p.is_object()).map(|p| Pic {
        path: s(p, "path"),
        width: p.get("width").and_then(Value::as_u64).unwrap_or(0) as u32,
        height: p.get("height").and_then(Value::as_u64).unwrap_or(0) as u32,
    });
    let body = match ins.get("body") {
        Some(b) => {
            let rows = b.get("rows").and_then(Value::as_array).cloned().unwrap_or_default();
            match b.get("type").and_then(Value::as_str) {
                Some("entries") => Body::Entries(rows.iter().map(entry_of).collect()),
                Some("table") => Body::Table(
                    rows.iter().map(|r| r.as_array().map(|c| c.iter().map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string())).collect()).unwrap_or_default()).collect(),
                ),
                Some("tree") => Body::Tree(
                    rows.iter()
                        .map(|r| Row {
                            depth: r.get("depth").and_then(Value::as_u64).unwrap_or(0) as usize,
                            name: s(r, "name"),
                            dir: r.get("dir").and_then(Value::as_bool).unwrap_or(false),
                            size: r.get("size").and_then(Value::as_u64).unwrap_or(0),
                        })
                        .collect(),
                ),
                Some("text") => Body::Text(
                    b.get("lines").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
                    b.get("ansi").and_then(Value::as_bool).unwrap_or(false),
                ),
                _ => Body::None,
            }
        }
        None => Body::None,
    };
    let renderer = ins.get("renderer").and_then(Value::as_str).map(str::to_string);
    Preview { facts, missing, positions: ins.get("positions").and_then(Value::as_u64).unwrap_or(1).max(1) as u32, picture, body, renderer }
}

/// How big a picture a host should ask the provider for, in pixels, for an area of `inner` cells: what the terminal
/// can show (kitty graphics: the area's pixels) or half blocks (a cell is one pixel wide and two high).
pub fn wanted_px(inner: Rect) -> (u32, u32) {
    if iconimg::placeholders() {
        let cell = match iconimg::cell_size() {
            (0, _) | (_, 0) => (10, 20),
            c => c,
        };
        (inner.w as u32 * cell.0, inner.h as u32 * cell.1)
    } else {
        (inner.w as u32, inner.h as u32 * 2)
    }
}

/// Draws a provider's answer, and keeps what a host must keep between frames: the picture on the terminal (kitty
/// graphics where it takes them, else half blocks), the 30 ms a picture waits after the cursor moved (holding j / k
/// sends nothing), the decoded picture. One per host; the same drawing in lvc, lfz and every other host.
pub struct Drawer {
    since: Instant,
    shown: Option<(String, u32)>,
    wait: bool,
    decoded: Option<(String, Vec<u8>, u32, u32)>,
    /// The `iconimg::epoch` the picture on screen was sent in: when the terminal lost its images (another program ran
    /// full screen and the program came back) the picture is sent again.
    epoch: u64,
    /// `[image_transfer] file`: the terminal reads the picture from a temporary file (fast) instead of the
    /// bytes through the terminal.
    pub transfer_file: bool,
}

impl Drawer {
    pub fn new(transfer_file: bool) -> Drawer {
        Drawer { since: Instant::now(), shown: None, wait: false, decoded: None, epoch: iconimg::epoch(), transfer_file }
    }

    /// The cursor moved to another file (or a seek): its picture waits 30 ms before it goes out.
    pub fn moved(&mut self) {
        self.since = Instant::now();
    }

    /// True once after a draw that held its picture back: redraw within 10 ms.
    pub fn take_wait(&mut self) -> bool {
        std::mem::take(&mut self.wait)
    }

    /// The picture on screen is no longer wanted (another kind of preview took its place): take it off.
    pub fn forget(&mut self) {
        forget_shown(&mut self.shown);
    }

    /// Draw `p` into `inner`: the facts, the lines of what is missing (red), then the picture and / or the body,
    /// which scrolls by `offset`. `key` names what is shown (the file and the position in it): the picture is sent
    /// to the terminal once per key. Returns how many rows the body has (for the host's scroll keys).
    pub fn draw(&mut self, cv: &mut Canvas, l: &Look, inner: Rect, p: &Preview, key: &str, offset: &mut usize) -> usize {
        let lim = inner.x + inner.w;
        if self.epoch != iconimg::epoch() {
            // the terminal dropped its images: what was shown is gone (no delete to send), send it again
            self.epoch = iconimg::epoch();
            self.shown = None;
        }
        let Some(pic) = p.picture.as_ref().filter(|pic| pic.width > 0 && pic.height > 0) else {
            self.forget();
            return draw_lines(cv, l, inner, p, lim, offset);
        };
        // a WIDE, LOW area (the preview above or below the list): the facts in a column on the left and the
        // picture beside them, not one under the other — the picture would be a strip in what is left of the height
        let side = inner.w >= 60 && inner.w as u32 >= inner.h as u32 * 3;
        let lw = p.facts.iter().map(|(k, _)| str_width(k)).max().unwrap_or(0);
        let facts_w = p.facts.iter().map(|(_, v)| lw + 2 + str_width(v)).max().unwrap_or(0) as u16;
        let miss_w = p.missing.iter().map(|m| str_width(m) + 3).max().unwrap_or(0) as u16;
        let (fact_rows, info_lim) = if side {
            (p.facts.len().min(inner.h as usize) as u16, inner.x + (facts_w.max(miss_w) + 3).min(inner.w / 2))
        } else {
            (p.facts.len().min(inner.h as usize / 3) as u16, lim)
        };
        if fact_rows > 0 {
            listview::draw_facts(cv, l, Rect { x: inner.x, y: inner.y, w: info_lim - inner.x, h: fact_rows }, &p.facts[..fact_rows as usize]);
        }
        let red = listview::tint(l, "red", l.panel, l.panel.bg);
        let miss_rows = (p.missing.len() as u16).min(inner.h.saturating_sub(fact_rows));
        for (i, m) in p.missing.iter().take(miss_rows as usize).enumerate() {
            cv.put(inner.x + 1, inner.y + fact_rows + i as u16, &format!("✗ {m}"), red, info_lim);
        }
        let area = if side {
            Rect { x: info_lim + 1, y: inner.y, w: lim.saturating_sub(info_lim + 1), h: inner.h }
        } else {
            let top = fact_rows + miss_rows;
            Rect { x: inner.x, y: inner.y + top, w: inner.w, h: inner.h.saturating_sub(top) }
        };
        if area.w == 0 || area.h == 0 {
            return 0;
        }
        // the picture file the provider made (a PNG in its cache), decoded once
        if self.decoded.as_ref().is_none_or(|(path, ..)| path != &pic.path) {
            self.decoded = image::open(&pic.path).ok().map(|i| {
                let r = i.to_rgba8();
                let (w, h) = (r.width(), r.height());
                (pic.path.clone(), r.into_raw(), w, h)
            });
        }
        let Some((_, px, w, h)) = self.decoded.as_ref() else {
            cv.put(area.x + 1, area.y, "✗ the picture cannot be read", red, lim);
            return 0;
        };
        let (w, h) = (*w, *h);
        if iconimg::placeholders() {
            let cell = match iconimg::cell_size() {
                (0, _) | (_, 0) => (10, 20),
                c => c,
            };
            // the cells it takes, shrunk (aspect kept) to what is left
            let (mut cols, mut rows) = media::cells_for(w, h, cell);
            if rows > area.h || cols > area.w {
                let f = (area.h as f32 / rows as f32).min(area.w as f32 / cols as f32);
                cols = ((cols as f32 * f).round() as u16).max(1);
                rows = ((rows as f32 * f).round() as u16).max(1);
            }
            let key = format!("{key}|{}x{}", area.w, area.h);
            if self.shown.as_ref().is_none_or(|(k, _)| k != &key) {
                if self.since.elapsed() < Duration::from_millis(30) {
                    self.wait = true;
                    return 0;
                }
                forget_shown(&mut self.shown);
                let id = iconimg::new_media_id();
                let bytes = if self.transfer_file { media::transmit_fast(id, px, w, h, cols, rows) } else { media::transmit(id, px, w, h, cols, rows) };
                iconimg::push_pending(&bytes);
                self.shown = Some((key, id));
            }
            if let Some((_, id)) = &self.shown {
                cv.put_image(area.x + area.w.saturating_sub(cols) / 2, area.y, cols, rows, *id);
            }
        } else {
            forget_shown(&mut self.shown);
            // half blocks: the picture averaged down to what is left (each cell takes the mean of the source pixels it covers,
            // so thin lines and edges stay smooth instead of being picked out by nearest-pixel sampling)
            let f = ((area.h as f32 * 2.0) / h as f32).min(area.w as f32 / w as f32).min(1.0);
            let (tw, th) = (((w as f32 * f) as u32).max(1), ((h as f32 * f) as u32).max(1));
            let mut out = Vec::with_capacity((tw * th * 4) as usize);
            for y in 0..th {
                let (y0, y1) = ((y * h / th).min(h - 1), (((y + 1) * h).div_ceil(th)).clamp(y * h / th + 1, h));
                for x in 0..tw {
                    let (x0, x1) = ((x * w / tw).min(w - 1), (((x + 1) * w).div_ceil(tw)).clamp(x * w / tw + 1, w));
                    let (mut acc, mut n) = ([0u32; 4], 0u32);
                    for sy in y0..y1 {
                        for sx in x0..x1 {
                            let i = ((sy * w + sx) * 4) as usize;
                            for c in 0..4 {
                                acc[c] += px[i + c] as u32;
                            }
                            n += 1;
                        }
                    }
                    let n = n.max(1);
                    out.extend_from_slice(&[(acc[0] / n) as u8, (acc[1] / n) as u8, (acc[2] / n) as u8, (acc[3] / n) as u8]);
                }
            }
            cv.put_halfblocks(area.x + area.w.saturating_sub(tw as u16) / 2, area.y, &out, tw, th);
        }
        0
    }
}

/// Take the picture on screen off the terminal.
fn forget_shown(shown: &mut Option<(String, u32)>) {
    if let Some((_, old)) = shown.take() {
        iconimg::push_pending(&media::delete(old));
        iconimg::drop_media_id(old);
    }
}

/// The facts on top, the lines of what is missing (red) under them, then the body, which scrolls by `offset`.
fn draw_lines(cv: &mut Canvas, l: &Look, inner: Rect, p: &Preview, lim: u16, offset: &mut usize) -> usize {
    let mut top = 0u16;
    if !p.facts.is_empty() {
        let rows = (p.facts.len() as u16).min(inner.h);
        listview::draw_facts(cv, l, Rect { h: rows, ..inner }, &p.facts);
        top += rows;
    }
    let red = listview::tint(l, "red", l.panel, l.panel.bg);
    for m in &p.missing {
        if top >= inner.h {
            break;
        }
        let line = truncate(&format!("✗ {m}"), (inner.w as usize).saturating_sub(2));
        cv.put(inner.x + 1, inner.y + top, &line, red, lim);
        top += 1;
    }
    let body = Rect { x: inner.x, y: inner.y + top, w: inner.w, h: inner.h.saturating_sub(top) };
    if body.h == 0 {
        return 0;
    }
    match &p.body {
        Body::None => 0,
        Body::Entries(v) => {
            let refs: Vec<&Entry> = v.iter().filter(|e| !e.is_parent()).collect();
            listview::draw_dir(cv, l, body, &refs, offset)
        }
        Body::Table(rows) => listview::draw_table(cv, l, body, rows, offset),
        Body::Tree(rows) => listview::draw_archive(cv, l, body, rows, offset),
        Body::Text(lines, ansi) => {
            *offset = (*offset).min(lines.len().saturating_sub(body.h as usize));
            // the colours of the lines above carry on into the ones below (a bat theme sets one for many lines)
            let mut carry = l.panel;
            for line in lines.iter().take(*offset) {
                carry = crate::ansi::parse(line, carry).2;
            }
            for (i, line) in lines.iter().skip(*offset).take(body.h as usize).enumerate() {
                let y = body.y + i as u16;
                if !*ansi {
                    let w = body.w as usize;
                    let shown = if str_width(line) > w.saturating_sub(1) { truncate(line, w.saturating_sub(1)) } else { line.clone() };
                    cv.put(body.x + 1, y, &shown, l.panel, lim);
                    continue;
                }
                let (text, runs, end) = crate::ansi::parse(line, carry);
                carry = end;
                let mut x = body.x + 1;
                for (bi, ch) in text.char_indices() {
                    if ch.is_control() {
                        continue;
                    }
                    let st = crate::ansi::style_at(&runs, bi);
                    let st = Style { fg: st.fg.or(l.panel.fg), bg: st.bg.or(l.panel.bg), ..st };
                    x = cv.put(x, y, &ch.to_string(), st, lim);
                    if x >= lim {
                        break;
                    }
                }
            }
            lines.len()
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_kind_of_body() {
        let a = parse(
            r#"[{"op":"preview","facts":[["type","binary file"]],"missing":["x needs y"],"positions":3,
                 "picture":{"path":"/c/a.png","width":10,"height":20},
                 "body":{"type":"entries","rows":[{"name":"d","dir":true,"size":0,"mtime":5},{"name":"x","exec":true,"size":3}]}}]"#,
        )
        .unwrap();
        let Answer::Preview(p) = a else { panic!("{a:?}") };
        assert_eq!(p.facts, vec![("type".to_string(), "binary file".to_string())]);
        assert_eq!((p.missing.len(), p.positions), (1, 3));
        assert_eq!(p.picture, Some(Pic { path: "/c/a.png".into(), width: 10, height: 20 }));
        let Body::Entries(v) = p.body else { panic!() };
        assert!(v[0].is_dir() && v[1].is_exec() && !v[0].is_exec());

        let t = parse(r#"[{"op":"preview","body":{"type":"table","rows":[["a","b"],["1",2]]}}]"#).unwrap();
        let Answer::Preview(Preview { body: Body::Table(rows), .. }) = t else { panic!() };
        assert_eq!(rows, vec![vec!["a".to_string(), "b".to_string()], vec!["1".to_string(), "2".to_string()]]);

        let r = parse(r#"[{"op":"preview","body":{"type":"tree","rows":[{"depth":1,"name":"n","dir":false,"size":7}]}}]"#).unwrap();
        let Answer::Preview(Preview { body: Body::Tree(rows), .. }) = r else { panic!() };
        assert_eq!((rows[0].depth, rows[0].size, rows[0].dir), (1, 7, false));

        let x = parse(r#"[{"op":"preview","body":{"type":"text","lines":["a","b"]}}]"#).unwrap();
        assert!(matches!(x, Answer::Preview(Preview { body: Body::Text(ref l, false), .. }) if l.len() == 2));
        let a = parse(r#"[{"op":"preview","body":{"type":"text","ansi":true,"lines":["x"]}},{"op":"preview","renderer":"nvim"}]"#).unwrap();
        assert!(matches!(a, Answer::Preview(Preview { body: Body::Text(_, true), renderer: None, .. })));
        let r = parse(r#"[{"op":"preview","renderer":"builtin"}]"#).unwrap();
        assert!(matches!(r, Answer::Preview(Preview { renderer: Some(ref s), .. }) if s == "builtin"));
    }

    #[test]
    fn pass_and_mistakes() {
        assert!(matches!(parse(r#"[{"op":"pass"}]"#).unwrap(), Answer::Pass));
        // a message before the preview is skipped; the first preview / pass decides
        assert!(matches!(parse(r#"[{"op":"message","text":"x"},{"op":"preview"}]"#).unwrap(), Answer::Preview(_)));
        assert!(parse("[]").is_err());
        assert!(parse(r#"[{"op":"message","text":"x"}]"#).is_err());
        assert!(parse("{}").is_err());
        assert!(parse("nope").is_err());
    }

    #[test]
    fn drawing_does_not_panic_on_small_or_empty_areas() {
        let l = crate::lvim::Look::new(crate::lvim::Theme::terminal());
        let mut cv = Canvas::new(40, 6);
        let p = Preview {
            facts: vec![("a".into(), "b".into())],
            missing: vec!["m".repeat(80)],
            positions: 1,
            picture: None,
            body: Body::Text((0..20).map(|i| format!("line {i}")).collect(), false),
            renderer: None,
        };
        let mut off = 100;
        let mut d = Drawer::new(true);
        let total = d.draw(&mut cv, &l, Rect { x: 0, y: 0, w: 40, h: 6 }, &p, "k", &mut off);
        assert_eq!(total, 20);
        assert_eq!(off, 20 - 4); // clamped: 6 rows less the fact and the missing line
        let tiny = d.draw(&mut cv, &l, Rect { x: 0, y: 0, w: 5, h: 1 }, &p, "k", &mut 0);
        assert_eq!(tiny, 0);
    }
}

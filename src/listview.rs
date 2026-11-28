//! The previews of a DIRECTORY, a CSV / TSV TABLE, an ARCHIVE and a BINARY file, drawn by lvc and lfz with the same
//! code, so they look alike. The host reads the data (it knows when to do it off the UI thread) and passes it in;
//! each `draw_*` clamps the scroll `offset` it is given and returns how many rows there are to scroll.

use crossterm::style::Attribute;

use crate::archive::Row;
use crate::canvas::{Canvas, Rect, str_width, truncate};
use crate::fsinfo::{Entry, Kind, date_string, human_size};
use crate::lvim::Look;
use crate::theme::Style;

/// A style from the theme's colour `name` over `fallback`'s attributes, on `bg`.
pub fn tint(l: &Look, name: &str, fallback: Style, bg: Option<crossterm::style::Color>) -> Style {
    Style { fg: l.theme.color(name).or(fallback.fg), bg, ..fallback }
}

/// The bar over a preview's text, in the colour of the search bar: which lines are shown (from – to / all) and, at the right end, how to scroll
/// (only when there is something to scroll and room for it); an empty file or folder says `No data`. `r` is the bar's row (one high); the text
/// is the rows under it.
pub fn scroll_bar(cv: &mut Canvas, l: &Look, r: Rect, off: usize, total: usize, visible: usize) {
    let field = crate::lvim::search_field(l);
    cv.fill(Rect { x: r.x, y: r.y, w: r.w, h: 1 }, field);
    let left = if total == 0 { " 0 / 0".to_string() } else { format!(" {}–{} / {}", off + 1, (off + visible).min(total), total) };
    let lim = r.x + r.w;
    cv.put(r.x, r.y, &left, field, lim);
    if total > visible {
        let hint = "C-d C-u to scroll ";
        let hw = str_width(hint) as u16;
        if r.w > hw + str_width(&left) as u16 + 1 {
            cv.put(lim - hw, r.y, hint, field, lim);
        }
    }
    if total == 0 {
        cv.put(r.x, r.y + 1, " No data", l.count, lim);
    }
}

/// The thumb of a scrollbar: one column at `x`, `rows` high from `y`, for `total` rows of which `rows` show from `off`. Nothing when everything shows.
pub fn thumb(cv: &mut Canvas, x: u16, y: u16, rows: usize, off: usize, total: usize, st: Style) {
    if rows == 0 || total <= rows {
        return;
    }
    let bar = ((rows * rows) / total).max(1);
    let max_off = total - rows;
    let pos = off.min(max_off) * (rows - bar) / max_off;
    for k in 0..bar {
        cv.put(x, y + (pos + k) as u16, "┃", st, x + 1);
    }
}

/// The thumb for a text that wraps: as long at every place (`rows` of the pane over `total` lines), where the lines shown (`shown`, from `top0`, the first one counted
/// from 0) are not always as many as the rows.
pub fn thumb_view(cv: &mut Canvas, x: u16, y: u16, rows: usize, top0: usize, shown: usize, total: usize, st: Style) {
    if rows == 0 || total <= shown.max(1) {
        return;
    }
    let bar = ((rows * rows) / total).max(1).min(rows);
    let pos = (top0 * (rows - bar) / (total - shown)).min(rows - bar);
    for k in 0..bar {
        cv.put(x, y + (pos + k) as u16, "┃", st, x + 1);
    }
}

/// How old a time is, as a colour name: today green, this week teal, this month blue, this year purple, older dim.
pub fn age_colour(t: i64) -> &'static str {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    match now - t {
        d if d < 86_400 => "green",
        d if d < 7 * 86_400 => "teal",
        d if d < 31 * 86_400 => "blue",
        d if d < 365 * 86_400 => "purple",
        _ => "comment",
    }
}

/// The colour name of a human size (`12K`, `3M`, …): kilobytes green, megabytes yellow, gigabytes and more red, the rest dim.
pub fn size_colour(human: &str) -> &'static str {
    match human.chars().last() {
        Some('K') => "green",
        Some('M') => "yellow",
        Some('G') | Some('T') => "red",
        _ => "comment",
    }
}

/// The entries of a directory as a panel draws them — an icon, the name in its colour, the size and the date on
/// the right. `entries` without the `..` row.
pub fn draw_dir(cv: &mut Canvas, l: &Look, inner: Rect, entries: &[&Entry], offset: &mut usize) -> usize {
    if entries.is_empty() {
        cv.put(inner.x + 1, inner.y, "(empty)", l.counter, inner.x + inner.w);
        return 0;
    }
    let role = |name: &str, fallback: &str| -> Style {
        l.theme.roles.get(name).copied().unwrap_or(Style { fg: l.theme.color(fallback), ..Style::default() })
    };
    let (dir_st, exec_st, link_st, broken_st) =
        (role("dir", "blue"), role("exec", "green"), role("link", "cyan"), role("broken", "red"));
    let h = inner.h as usize;
    *offset = (*offset).min(entries.len().saturating_sub(h));
    let lim = inner.x + inner.w;
    for (row, e) in entries.iter().skip(*offset).take(h).enumerate() {
        let y = inner.y + row as u16;
        let base = l.rows[(*offset + row) % 2];
        cv.fill(Rect { x: inner.x, y, w: inner.w, h: 1 }, base);
        // the columns right of the name, each in its colour as in the panels: the kind or the size, then the date by its age
        let (size, size_col) = if e.is_dir() {
            ("DIR".to_string(), "blue")
        } else {
            let h = human_size(e.size);
            let c = size_colour(&h);
            (h, c)
        };
        let size_txt = format!(" {size:>6}");
        let date_txt = format!("  {} ", date_string(e.mtime));
        let rw = (str_width(&size_txt) + str_width(&date_txt)) as u16;
        let name_lim = lim.saturating_sub(rw).max(inner.x);
        let mut x = cv.put(inner.x, y, " ", base, name_lim);
        if let Some(id) = crate::iconimg::image_id(&e.name, e.is_dir()) {
            x = cv.put_image_icon(x, y, id, base.bg, name_lim);
        } else {
            let (glyph, accent) = crate::lvim::icon(&e.name, e.is_dir());
            let ifg = l.theme.color(accent).or(base.fg);
            x = cv.put(x, y, glyph, Style { fg: ifg, bg: base.bg, ..base }, name_lim);
        }
        x = cv.put(x, y, " ", base, name_lim);
        let kind = if e.broken {
            Some(broken_st)
        } else if matches!(e.kind, Kind::Link | Kind::LinkDir) {
            Some(link_st)
        } else if e.is_dir() {
            Some(dir_st)
        } else if e.is_exec() {
            Some(exec_st)
        } else {
            None
        };
        let ns = kind.map_or(base, |k| Style { fg: k.fg.or(base.fg), bg: base.bg, attrs: k.attrs, ..base });
        let mut name = e.name.clone();
        if e.is_dir() {
            name.push('/');
        }
        cv.put(x, y, &name, ns, name_lim);
        let sx = cv.put(lim - rw, y, &size_txt, tint(l, size_col, base, base.bg), lim);
        cv.put(sx, y, &date_txt, tint(l, age_colour(e.mtime), base, base.bg), lim);
    }
    entries.len()
}

/// One CSV line split at `sep`, with `"quoted, fields"` and `""` as a quote.
pub fn split_csv(line: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' if quoted && it.peek() == Some(&'"') => {
                cur.push('"');
                it.next();
            }
            '"' => quoted = !quoted,
            c if c == sep && !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// The first 500 lines of a CSV / TSV file as rows; the CSV separator is the one the header uses most
/// (`,` `;` tab `|`).
pub fn read_table(path: &std::path::Path, tsv: bool) -> Vec<Vec<String>> {
    use std::io::BufRead;
    let Ok(f) = std::fs::File::open(path) else { return Vec::new() };
    let lines: Vec<String> = std::io::BufReader::new(f).lines().take(500).map_while(Result::ok).collect();
    let sep = if tsv {
        '\t'
    } else {
        let h = lines.first().map(String::as_str).unwrap_or("");
        [',', ';', '\t', '|'].into_iter().max_by_key(|c| h.matches(*c).count()).unwrap_or(',')
    };
    lines.iter().map(|l| split_csv(l, sep)).collect()
}

/// A table: the header stays, the body scrolls; a colour a column, numbers to the right.
pub fn draw_table(cv: &mut Canvas, l: &Look, inner: Rect, rows: &[Vec<String>], offset: &mut usize) -> usize {
    let lim = inner.x + inner.w;
    let ncol = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..ncol)
        .map(|c| rows.iter().filter_map(|r| r.get(c)).map(|v| str_width(v)).max().unwrap_or(0).min(32))
        .collect();
    let colours = ["yellow", "cyan", "green", "purple", "orange", "teal", "magenta"];
    let dim = tint(l, "comment", l.panel, l.panel.bg);
    let h = inner.h as usize;
    let body = rows.len().saturating_sub(1);
    *offset = (*offset).min(body.saturating_sub(h.saturating_sub(2)));
    let draw_row = |cv: &mut Canvas, y: u16, r: &[String], base: Style, bold: bool| {
        cv.fill(Rect { x: inner.x, y, w: inner.w, h: 1 }, base);
        let mut x = inner.x + 1;
        for (c, w) in widths.iter().enumerate() {
            let v = r.get(c).map(String::as_str).unwrap_or("");
            let mut st = tint(l, colours[c % colours.len()], base, base.bg);
            if bold {
                st.attrs.set(Attribute::Bold);
            }
            let v = truncate(v, *w);
            let num = !v.is_empty() && v.trim().parse::<f64>().is_ok();
            let cell = if num { format!("{v:>w$}", w = *w) } else { format!("{v:<w$}", w = *w) };
            x = cv.put(x, y, &cell, st, lim);
            if c + 1 < widths.len() {
                x = cv.put(x, y, " │ ", Style { bg: base.bg, ..dim }, lim);
            }
        }
    };
    if let Some(head) = rows.first() {
        draw_row(cv, inner.y, head, l.panel, true);
        let rule: String = widths.iter().map(|w| "─".repeat(*w)).collect::<Vec<_>>().join("─┼─");
        cv.put(inner.x + 1, inner.y + 1, &rule, dim, lim);
    }
    for (i, r) in rows.iter().skip(1 + *offset).take(h.saturating_sub(2)).enumerate() {
        draw_row(cv, inner.y + 2 + i as u16, r, l.rows[(*offset + i) % 2], false);
    }
    body
}

/// An archive's tree as yazi shows it: a line a level, icons, names in their colours, the sizes on the right and a
/// line with the totals on top.
pub fn draw_archive(cv: &mut Canvas, l: &Look, inner: Rect, rows: &[Row], offset: &mut usize) -> usize {
    let lim = inner.x + inner.w;
    let dim = tint(l, "comment", l.panel, l.panel.bg);
    let files = rows.iter().filter(|r| !r.dir).count();
    let dirs = rows.len() - files;
    let total: u64 = rows.iter().map(|r| r.size).sum();
    let mut x = cv.put(inner.x + 1, inner.y, &files.to_string(), tint(l, "yellow", l.panel, l.panel.bg), lim);
    x = cv.put(x, inner.y, if files == 1 { " file  ·  " } else { " files  ·  " }, dim, lim);
    x = cv.put(x, inner.y, &dirs.to_string(), tint(l, "blue", l.panel, l.panel.bg), lim);
    x = cv.put(x, inner.y, if dirs == 1 { " directory  ·  " } else { " directories  ·  " }, dim, lim);
    cv.put(x, inner.y, &human_size(total), tint(l, "cyan", l.panel, l.panel.bg), lim);
    let body = Rect { x: inner.x, y: inner.y + 1, w: inner.w, h: inner.h.saturating_sub(1) };
    let h = body.h as usize;
    *offset = (*offset).min(rows.len().saturating_sub(h));
    let dir_st = l.theme.roles.get("dir").copied().unwrap_or(Style { fg: l.theme.color("blue"), ..Style::default() });
    for (i, r) in rows.iter().skip(*offset).take(h).enumerate() {
        let y = body.y + i as u16;
        let size = if r.dir { String::new() } else { human_size(r.size) };
        let sw = str_width(&size) as u16;
        let name_lim = lim.saturating_sub(sw + 2);
        let mut x = body.x + 1;
        for _ in 0..r.depth {
            x = cv.put(x, y, "│ ", dim, name_lim);
        }
        if let Some(id) = crate::iconimg::image_id(&r.name, r.dir) {
            x = cv.put_image_icon(x, y, id, l.panel.bg, name_lim);
        } else {
            let (glyph, accent) = crate::lvim::icon(&r.name, r.dir);
            x = cv.put(x, y, glyph, tint(l, accent, l.panel, l.panel.bg), name_lim);
        }
        x = cv.put(x, y, " ", l.panel, name_lim);
        let ns = if r.dir { Style { fg: dir_st.fg.or(l.panel.fg), bg: l.panel.bg, attrs: dir_st.attrs, ..l.panel } } else { l.panel };
        cv.put(x, y, &r.name, ns, name_lim);
        if !size.is_empty() {
            let colour = match size.chars().last() {
                Some('K') => "green",
                Some('M') => "yellow",
                Some('G') | Some('T') => "red",
                _ => "comment",
            };
            cv.put(lim.saturating_sub(sw + 1), y, &size, tint(l, colour, l.panel, l.panel.bg), lim);
        }
    }
    rows.len()
}

/// A binary file is a note about it — what it is and how big — never its bytes (the same table of facts a picture gets).
pub fn binary_note(path: &std::path::Path) -> Vec<(String, String)> {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    vec![("type".into(), "binary file".into()), ("file size".into(), human_size(size))]
}

/// A table of facts: the labels right-aligned in blue, the values in yellow.
pub fn draw_facts(cv: &mut Canvas, l: &Look, inner: Rect, info: &[(String, String)]) {
    let lim = inner.x + inner.w;
    let lw = info.iter().map(|(k, _)| str_width(k)).max().unwrap_or(0);
    let label = tint(l, "blue", l.panel, l.panel.bg);
    let value = tint(l, "yellow", l.panel, l.panel.bg);
    for (i, (k, v)) in info.iter().take(inner.h as usize).enumerate() {
        let y = inner.y + i as u16;
        let mut x = cv.put(inner.x + 1, y, &format!("{k:>lw$}"), label, lim);
        x = cv.put(x, y, "  ", l.panel, lim);
        cv.put(x, y, v, value, lim);
    }
}

/// Is `path` a binary file (a NUL in its first 8 KB)?
pub fn is_binary(path: &std::path::Path) -> bool {
    use std::io::Read;
    if !path.is_file() {
        return false;
    }
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut buf = [0u8; 8192];
    let n = f.read(&mut buf).unwrap_or(0);
    buf[..n].contains(&0)
}

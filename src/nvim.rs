//! An embedded Neovim (`nvim --embed`) used as the preview: we attach as a UI
//! client (ext_linegrid) and keep its grid, which the finder blits into the
//! preview area. One instance lives for the whole session; each item is
//! opened with `:edit`.

use std::collections::HashMap;
use std::io::{BufReader, BufWriter, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use crossterm::style::{Attribute, Attributes, Color};
use rmpv::Value;

use crate::theme::Style;

/// What a cell of the grid shows: an ASCII character (one byte, no allocation — nearly every cell), or anything wider (a multi-byte
/// character, or empty: the second half of a double-width one).
#[derive(Clone, Debug, PartialEq)]
pub enum CellText {
    Ascii(u8),
    Wide(Box<str>),
}

#[derive(Clone, Debug)]
pub struct Cell {
    pub text: CellText,
    pub hl: u64,
}

impl Default for Cell {
    fn default() -> Cell {
        Cell::blank()
    }
}

impl Cell {
    /// A space with no highlight.
    pub const fn blank() -> Cell {
        Cell { text: CellText::Ascii(b' '), hl: 0 }
    }

    /// The cell's text of Neovim (`""` for the right half of a double-width character).
    pub fn new(text: &str, hl: u64) -> Cell {
        let text = match text.as_bytes() {
            [b] if b.is_ascii() => CellText::Ascii(*b),
            _ => CellText::Wide(text.into()),
        };
        Cell { text, hl }
    }

    /// The text to draw.
    pub fn as_str(&self) -> &str {
        match &self.text {
            // a byte below 128 is one character
            CellText::Ascii(b) => std::str::from_utf8(std::slice::from_ref(b)).unwrap_or(" "),
            CellText::Wide(s) => s,
        }
    }
}

#[derive(Default)]
pub struct Grid {
    pub w: usize,
    pub h: usize,
    pub cells: Vec<Cell>,
    pub cursor: (usize, usize),
    /// What the preview shows of the file, told by Neovim (`lvfz_view`): the first and the last line in the window, and the lines the buffer has —
    /// for the scrollbar the finder draws. (0, 0, 0) until Neovim has said.
    pub view: (usize, usize, usize),
    pub hl: HashMap<u64, Style>,
    pub default_fg: Option<Color>,
    pub default_bg: Option<Color>,
    /// Bumped on every `flush`.
    pub frame: u64,
    /// Drawn: the first target has been opened and painted. Until then the
    /// finder keeps its "starting" note — the instance's own empty or start
    /// screen never flashes in the preview.
    pub ready: bool,
    /// The first open has been answered (its paint is the next flush).
    opened: bool,
    /// The target (`path`, or the text for a non-file) whose open was answered
    /// last — what the grid shows: until it is the finder's current item, the
    /// finder shows its own instant file preview instead.
    pub shown: Option<String>,
    pub exited: bool,
}

impl Grid {
    fn resize(&mut self, w: usize, h: usize) {
        self.w = w;
        self.h = h;
        self.cells = vec![Cell::blank(); w * h];
    }

    fn clear(&mut self) {
        self.cells.fill(Cell::blank());
    }

    /// The style of highlight `id` with the default colours filled in.
    pub fn style(&self, id: u64) -> Style {
        let s = self.hl.get(&id).copied().unwrap_or_default();
        Style { fg: s.fg.or(self.default_fg), bg: s.bg.or(self.default_bg), ..s }
    }
}

/// The Lua module of the lvim-fuzzy-nvim plugin that shows a file in the Neovim preview (opens the target, marks the query). The one place
/// its name is written: the preview code, the pool's warm-up and `doctor` all use it.
pub const PREVIEW_MODULE: &str = "lvim-fuzzy.preview";

/// Where requests go: the embedded Neovim's stdin, or a pooled instance's socket.
type Writer = Arc<Mutex<BufWriter<Box<dyn Write + Send>>>>;

pub struct Nvim {
    /// The embedded Neovim (`--embed`); None for an instance lent by the pool.
    child: Option<Child>,
    /// The pool's lease on the instance (given back when dropped), and the socket
    /// to shut down so the reader thread ends.
    lease: Option<(crate::pool::Lease, std::os::unix::net::UnixStream)>,
    writer: Writer,
    /// When the instance was started or taken (the finder shows "starting" only
    /// when it takes noticeably long).
    pub started: std::time::Instant,
    next_id: Arc<Mutex<u64>>,
    /// One preview open at a time: the request in flight, and only the
    /// latest one waiting (a key held down must not queue every file it
    /// passes; Neovim opens each with its filetype, autocmds and plugins).
    opens: Arc<Mutex<Opens>>,
    pub grid: Arc<Mutex<Grid>>,
    attached: (usize, usize),
}

#[derive(Default)]
struct Opens {
    in_flight: Option<u64>,
    waiting: Option<Value>,
    /// The targets of the open in flight and of the waiting one.
    in_flight_tag: Option<String>,
    waiting_tag: Option<String>,
}

/// Write one request (`[0, id, method, params]`); returns its id.
fn write_request(writer: &Mutex<BufWriter<Box<dyn Write + Send>>>, next_id: &Mutex<u64>, method: &str, params: Value) -> u64 {
    let id = {
        let mut n = next_id.lock().unwrap();
        *n += 1;
        *n
    };
    let msg = Value::Array(vec![Value::from(0), Value::from(id), s(method), params]);
    let mut w = writer.lock().unwrap();
    let _ = rmpv::encode::write_value(&mut *w, &msg);
    let _ = w.flush();
    id
}

fn rgb(v: &Value) -> Option<Color> {
    let n = v.as_i64()?;
    if n < 0 {
        return None;
    }
    Some(Color::Rgb { r: (n >> 16) as u8, g: (n >> 8) as u8, b: n as u8 })
}

/// The preview's Neovim is not the pane's Neovim: it draws in no terminal of its own. Inheriting the
/// multiplexer's variables made plugins that mark "Neovim runs in this pane" (lvim-winnav) label the
/// FINDER's pane as Neovim — the pane-nav keys were then sent into it instead of moving the pane, and
/// the mark stayed after the finder (which ends this Neovim without a quit) had gone.
pub fn no_pane_env(cmd: &mut Command) {
    for v in ["ZELLIJ", "ZELLIJ_PANE_ID", "ZELLIJ_SESSION_NAME", "TMUX", "TMUX_PANE"] {
        cmd.env_remove(v);
    }
}

fn s(v: &str) -> Value {
    Value::from(v)
}

impl Nvim {
    /// The preview's Neovim: a warm instance from the pool (`lvim-fuzzy-cli pool`) when
    /// `pool` is on and one is free, else `nvim --embed`. `program` may carry
    /// arguments ("nvim -u NONE").
    pub fn start(program: &str, appname: Option<&str>, pool: bool, wake: Sender<()>) -> std::io::Result<Nvim> {
        if pool
            && let Some(lease) = crate::pool::take(program, appname)
            && let Ok(stream) = std::os::unix::net::UnixStream::connect(&lease.socket)
            && let (Ok(reader), Ok(shut)) = (stream.try_clone(), stream.try_clone())
        {
            let mut n = Self::with_io(None, Box::new(stream), Box::new(reader), wake);
            // the instance started in the pool's directory: relative paths are this finder's
            if let Ok(cwd) = std::env::current_dir() {
                n.send("nvim_set_current_dir", vec![s(&cwd.to_string_lossy())]);
            }
            n.lease = Some((lease, shut));
            return Ok(n);
        }
        Self::embed(program, appname, wake)
    }

    /// Start `nvim --embed`.
    fn embed(program: &str, appname: Option<&str>, wake: Sender<()>) -> std::io::Result<Nvim> {
        let words = crate::util::shell_words(program).unwrap_or_else(|_| vec!["nvim".into()]);
        let (prog, args) = words.split_first().map(|(p, a)| (p.clone(), a.to_vec())).unwrap_or(("nvim".into(), vec![]));
        let mut cmd = Command::new(prog);
        // mark the instance so plugins can tell it is the finder's preview
        // (lvim-hud hides its tabline there: the finder shows the file name itself)
        cmd.args(args)
            .args(["--cmd", "let g:lvim_fuzzy_preview = 1", "--embed", "-n", "-i", "NONE"])
            .env("LVIM_FUZZY_PREVIEW", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(a) = appname {
            cmd.env("NVIM_APPNAME", a);
        }
        crate::nvim::no_pane_env(&mut cmd);
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        Ok(Self::with_io(Some(child), Box::new(stdin), Box::new(stdout), wake))
    }

    /// The client over any transport: requests to `w`, redraws and answers from `r`.
    fn with_io(child: Option<Child>, w: Box<dyn Write + Send>, r: Box<dyn std::io::Read + Send>, wake: Sender<()>) -> Nvim {
        let grid = Arc::new(Mutex::new(Grid::default()));
        let g = grid.clone();
        let writer: Writer = Arc::new(Mutex::new(BufWriter::new(w)));
        let next_id = Arc::new(Mutex::new(1u64));
        let opens: Arc<Mutex<Opens>> = Arc::new(Mutex::new(Opens::default()));
        let (w2, n2, o2) = (writer.clone(), next_id.clone(), opens.clone());
        thread::spawn(move || {
            let mut r = BufReader::new(r);
            while let Ok(msg) = rmpv::decode::read_value(&mut r) {
                // a response ([1, id, error, result]) to the open in flight:
                // the waiting one (the latest target) goes next
                if let Some(arr) = msg.as_array()
                    && arr.first().and_then(Value::as_u64) == Some(1)
                {
                    let id = arr.get(1).and_then(Value::as_u64);
                    let mut o = o2.lock().unwrap();
                    if id.is_some() && o.in_flight == id {
                        // the open paints before it answers (`redraw` in it): the grid shows it now
                        let mut gl = g.lock().unwrap();
                        gl.opened = true;
                        gl.shown = o.in_flight_tag.take();
                        drop(gl);
                        o.in_flight_tag = o.waiting_tag.take();
                    }
                    if id.is_some() && o.in_flight == id {
                        o.in_flight = o.waiting.take().map(|p| write_request(&w2, &n2, "nvim_exec_lua", p));
                    }
                    continue;
                }
                handle(&msg, &g, &wake);
            }
            g.lock().unwrap().exited = true;
            let _ = wake.send(());
        });
        Nvim { child, lease: None, writer, started: std::time::Instant::now(), next_id, opens, grid, attached: (0, 0) }
    }

    fn send(&self, method: &str, params: Vec<Value>) {
        write_request(&self.writer, &self.next_id, method, Value::Array(params));
    }

    /// Attach (first call) or resize the UI to `w` x `h`.
    pub fn size(&mut self, w: usize, h: usize) {
        if w == 0 || h == 0 || self.attached == (w, h) {
            return;
        }
        if self.attached == (0, 0) {
            let opts = Value::Map(vec![(s("rgb"), Value::from(true)), (s("ext_linegrid"), Value::from(true))]);
            self.send("nvim_ui_attach", vec![Value::from(w as u64), Value::from(h as u64), opts]);
            // Nothing is set in the instance from here: how the preview looks is up to the
            // user's config and plugins, which see g:lvim_fuzzy_preview.
        } else {
            self.send("nvim_ui_try_resize", vec![Value::from(w as u64), Value::from(h as u64)]);
        }
        self.attached = (w, h);
    }

    /// Show `path` at `line` (1-based, 0 = keep), or `text` in a scratch buffer
    /// when `path` is not a file.
    pub fn open(&self, path: &str, line: usize, col: usize, text: &str, query: &str) {
        // How the preview looks lives in Neovim: the lvim-fuzzy plugin's
        // `lvim-fuzzy.preview` (opens the target, marks the query). Without that
        // plugin only the target is opened.
        const CODE: &str = r#"
local path, line, col, text, query = ...
-- where the window is in the buffer, told to the finder (its scrollbar): now, and when the window scrolls
local function view()
  pcall(vim.rpcnotify, 0, "lvfz_view", vim.fn.line("w0"), vim.fn.line("w$"), vim.fn.line("$"))
end
vim.api.nvim_create_augroup("lvim_fuzzy_view", { clear = true })
vim.api.nvim_create_autocmd({ "WinScrolled", "CursorMoved", "BufWinEnter" }, { group = "lvim_fuzzy_view", callback = view })
local ok, pv = pcall(require, "@PREVIEW_MODULE@")
if ok and type(pv) == "table" and pv.show then
  pv.show({ path = path, line = line, col = col, text = text, query = query })
  -- paint now: Neovim redraws only when it is idle, and a held key sends the next
  -- open the moment this one is answered — it was never idle, the preview froze
  pcall(vim.cmd, "redraw")
  view()
  return
end
local prev = vim.api.nvim_get_current_buf()
local ok = false
if path ~= "" and vim.fn.filereadable(path) == 1 then
  ok = pcall(vim.cmd, "silent keepalt edit " .. vim.fn.fnameescape(path))
end
if not ok then
  local b = vim.api.nvim_create_buf(false, true)
  vim.bo[b].bufhidden = "wipe"
  vim.api.nvim_buf_set_lines(b, 0, -1, false, vim.split(text, "\n", { plain = true }))
  vim.api.nvim_set_current_buf(b)
end
local cur = vim.api.nvim_get_current_buf()
if prev ~= cur and vim.api.nvim_buf_is_valid(prev) and not vim.bo[prev].modified
   and vim.fn.bufwinnr(prev) == -1 and vim.b[prev].lvim_fuzzy_keep == nil then
  pcall(vim.api.nvim_buf_delete, prev, { force = false })
end
local n = vim.api.nvim_buf_line_count(cur)
if line > 0 then
  line = math.min(line, n)
  pcall(vim.api.nvim_win_set_cursor, 0, { line, math.max(col - 1, 0) })
  vim.cmd("normal! zz")
else
  pcall(vim.api.nvim_win_set_cursor, 0, { 1, 0 })
end
pcall(vim.cmd, "redraw")
view()
"#;
        let params = Value::Array(vec![
            s(&CODE.replace("@PREVIEW_MODULE@", PREVIEW_MODULE)),
            Value::Array(vec![s(path), Value::from(line as u64), Value::from(col as u64), s(text), s(query)]),
        ]);
        // one open at a time; meanwhile only the latest target waits
        let tag = if path.is_empty() { text.to_string() } else { path.to_string() };
        let mut o = self.opens.lock().unwrap();
        if o.in_flight.is_some() {
            o.waiting = Some(params);
            o.waiting_tag = Some(tag);
        } else {
            o.in_flight_tag = Some(tag);
            o.in_flight = Some(write_request(&self.writer, &self.next_id, "nvim_exec_lua", params));
        }
    }

    /// Send keys in Neovim's notation (`<C-d>`, `gg`, ...).
    pub fn input(&self, keys: &str) {
        self.send("nvim_input", vec![s(keys)]);
    }

    pub fn mouse(&self, button: &str, action: &str, modifier: &str, row: usize, col: usize) {
        self.send("nvim_input_mouse", vec![s(button), s(action), s(modifier), Value::from(0), Value::from(row as u64), Value::from(col as u64)]);
    }

    pub fn quit(mut self) {
        self.shutdown();
    }

    /// End the instance: the child is killed and reaped; a pooled one is detached and given back (the pool cleans or ends it).
    /// Done once, whichever of `quit` and `Drop` comes first.
    fn shutdown(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some((lease, sock)) = self.lease.take() {
            self.send("nvim_ui_detach", vec![]);
            let _ = sock.shutdown(std::net::Shutdown::Both);
            drop(lease);
        }
    }
}

impl Drop for Nvim {
    /// A finder that ends any other way (an error, a panic unwinding) leaves no Neovim behind.
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn handle(msg: &Value, grid: &Arc<Mutex<Grid>>, wake: &Sender<()>) {
    let Some(arr) = msg.as_array() else { return };
    // notification: [2, method, params]
    if arr.first().and_then(Value::as_u64) != Some(2) {
        return;
    }
    // Neovim's own notification: the window's place in the buffer (for the finder's scrollbar)
    if arr.get(1).and_then(Value::as_str) == Some("lvfz_view") {
        if let Some(p) = arr.get(2).and_then(Value::as_array) {
            let n = |i: usize| p.get(i).and_then(Value::as_u64).unwrap_or(0) as usize;
            grid.lock().unwrap().view = (n(0), n(1), n(2));
            let _ = wake.send(());
        }
        return;
    }
    if arr.get(1).and_then(Value::as_str) != Some("redraw") {
        return;
    }
    let Some(batches) = arr.get(2).and_then(Value::as_array) else {
        return;
    };
    let mut g = grid.lock().unwrap();
    let mut flushed = false;
    for ev in batches {
        let Some(ev) = ev.as_array() else { continue };
        let Some(name) = ev.first().and_then(Value::as_str) else {
            continue;
        };
        for args in &ev[1..] {
            let Some(a) = args.as_array() else { continue };
            match name {
                "grid_resize" => {
                    let w = a.get(1).and_then(Value::as_u64).unwrap_or(0) as usize;
                    let h = a.get(2).and_then(Value::as_u64).unwrap_or(0) as usize;
                    g.resize(w, h);
                }
                "default_colors_set" => {
                    g.default_fg = a.first().and_then(rgb);
                    g.default_bg = a.get(1).and_then(rgb);
                }
                "hl_attr_define" => {
                    let id = a.first().and_then(Value::as_u64).unwrap_or(0);
                    let mut st = Style::default();
                    let mut reverse = false;
                    if let Some(m) = a.get(1).and_then(Value::as_map) {
                        let mut attrs = Attributes::default();
                        for (k, v) in m {
                            match k.as_str().unwrap_or("") {
                                "foreground" => st.fg = rgb(v),
                                "background" => st.bg = rgb(v),
                                "bold" if v.as_bool() == Some(true) => attrs.set(Attribute::Bold),
                                "italic" if v.as_bool() == Some(true) => attrs.set(Attribute::Italic),
                                "underline" | "undercurl" if v.as_bool() == Some(true) => attrs.set(Attribute::Underlined),
                                "strikethrough" if v.as_bool() == Some(true) => attrs.set(Attribute::CrossedOut),
                                "reverse" if v.as_bool() == Some(true) => reverse = true,
                                _ => {}
                            }
                        }
                        st.attrs = attrs;
                    }
                    if reverse {
                        let fg = st.fg.or(g.default_fg);
                        st.fg = st.bg.or(g.default_bg);
                        st.bg = fg;
                    }
                    g.hl.insert(id, st);
                }
                "grid_clear" => g.clear(),
                "grid_cursor_goto" => {
                    let r = a.get(1).and_then(Value::as_u64).unwrap_or(0) as usize;
                    let c = a.get(2).and_then(Value::as_u64).unwrap_or(0) as usize;
                    g.cursor = (r, c);
                }
                "grid_line" => {
                    let row = a.get(1).and_then(Value::as_u64).unwrap_or(0) as usize;
                    let mut col = a.get(2).and_then(Value::as_u64).unwrap_or(0) as usize;
                    let Some(cells) = a.get(3).and_then(Value::as_array) else {
                        continue;
                    };
                    let mut hl = 0u64;
                    if row >= g.h {
                        continue;
                    }
                    let w = g.w;
                    for cell in cells {
                        let Some(c) = cell.as_array() else { continue };
                        let text = c.first().and_then(Value::as_str).unwrap_or(" ");
                        let hl_of = |hl| Cell::new(text, hl);
                        if let Some(h) = c.get(1).and_then(Value::as_u64) {
                            hl = h;
                        }
                        let rep = c.get(2).and_then(Value::as_u64).unwrap_or(1) as usize;
                        for _ in 0..rep {
                            if col < w {
                                g.cells[row * w + col] = hl_of(hl);
                            }
                            col += 1;
                        }
                    }
                }
                "grid_scroll" => {
                    let top = a.get(1).and_then(Value::as_u64).unwrap_or(0) as usize;
                    let bot = a.get(2).and_then(Value::as_u64).unwrap_or(0) as usize;
                    let left = a.get(3).and_then(Value::as_u64).unwrap_or(0) as usize;
                    let right = a.get(4).and_then(Value::as_u64).unwrap_or(0) as usize;
                    let rows = a.get(5).and_then(Value::as_i64).unwrap_or(0);
                    let w = g.w;
                    let bot = bot.min(g.h);
                    let right = right.min(w);
                    if rows > 0 {
                        let rows = rows as usize;
                        for r in top..bot.saturating_sub(rows) {
                            for c in left..right {
                                g.cells[r * w + c] = g.cells[(r + rows) * w + c].clone();
                            }
                        }
                    } else if rows < 0 {
                        let rows = (-rows) as usize;
                        for r in (top + rows..bot).rev() {
                            for c in left..right {
                                g.cells[r * w + c] = g.cells[(r - rows) * w + c].clone();
                            }
                        }
                    }
                }
                "flush" => flushed = true,
                _ => {}
            }
        }
    }
    if flushed {
        g.frame += 1;
        g.ready = g.opened;
        drop(g);
        let _ = wake.send(());
    }
}

/// Translate a terminal key into Neovim's key notation.
pub fn key_notation(e: &crossterm::event::KeyEvent) -> Option<String> {
    use crossterm::event::{KeyCode, KeyModifiers};
    let m = e.modifiers;
    let mut pre = String::new();
    if m.contains(KeyModifiers::CONTROL) {
        pre.push_str("C-");
    }
    if m.contains(KeyModifiers::ALT) {
        pre.push_str("M-");
    }
    let shift = m.contains(KeyModifiers::SHIFT);
    let named = |n: &str, pre: &str| -> String {
        let s = if shift { format!("S-{pre}") } else { pre.to_string() };
        format!("<{s}{n}>")
    };
    Some(match e.code {
        KeyCode::Char(c) => {
            if pre.is_empty() {
                if c == '<' { "<lt>".into() } else { c.to_string() }
            } else {
                let c = if c == '<' { "lt".to_string() } else { c.to_string() };
                format!("<{pre}{c}>")
            }
        }
        KeyCode::Enter => named("CR", &pre),
        KeyCode::Esc => named("Esc", &pre),
        KeyCode::Backspace => named("BS", &pre),
        KeyCode::Tab => named("Tab", &pre),
        KeyCode::BackTab => "<S-Tab>".into(),
        KeyCode::Delete => named("Del", &pre),
        KeyCode::Insert => named("Insert", &pre),
        KeyCode::Up => named("Up", &pre),
        KeyCode::Down => named("Down", &pre),
        KeyCode::Left => named("Left", &pre),
        KeyCode::Right => named("Right", &pre),
        KeyCode::Home => named("Home", &pre),
        KeyCode::End => named("End", &pre),
        KeyCode::PageUp => named("PageUp", &pre),
        KeyCode::PageDown => named("PageDown", &pre),
        KeyCode::F(n) => named(&format!("F{n}"), &pre),
        _ => return None,
    })
}

/// Split a preview target like `file:12:3:text` into (file, line, col).
pub fn parse_target(s: &str) -> (String, usize, usize) {
    let s = s.trim();
    if std::path::Path::new(s).exists() {
        return (s.to_string(), 0, 0);
    }
    let mut parts = s.splitn(4, ':');
    let file = parts.next().unwrap_or("").to_string();
    let line = parts.next().and_then(|p| p.trim().parse().ok()).unwrap_or(0);
    let col = parts.next().and_then(|p| p.trim().parse().ok()).unwrap_or(0);
    (file, line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts the allocations of the process (only the ignored test below looks at it).
    struct Counting;
    static ALLOCS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    // SAFETY: every call goes to the system allocator unchanged; only a counter is added.
    unsafe impl std::alloc::GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
            ALLOCS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            unsafe { std::alloc::System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
            unsafe { std::alloc::System.dealloc(p, l) }
        }
    }
    #[global_allocator]
    static COUNT: Counting = Counting;

    /// 1 000 frames of a 200 x 50 grid of ASCII cells allocate next to nothing (a `String` per cell was 10 000 per frame):
    /// `cargo test -- --ignored grid_frames`
    #[test]
    #[ignore]
    fn grid_frames_do_not_allocate_per_cell() {
        let mut g = Grid::default();
        g.resize(200, 50);
        let before = ALLOCS.load(std::sync::atomic::Ordering::Relaxed);
        for f in 0..1000usize {
            for (i, c) in g.cells.iter_mut().enumerate() {
                *c = Cell::new(if (i + f) % 7 == 0 { "x" } else { " " }, (i % 5) as u64);
            }
            g.clear();
        }
        let used = ALLOCS.load(std::sync::atomic::Ordering::Relaxed) - before;
        assert!(used < 50, "{used} allocations in 1000 frames");
    }

    /// An ASCII cell holds its byte (no allocation); anything else holds its text, and the right half of a wide character is empty.
    #[test]
    fn a_cell_is_a_byte_or_a_text() {
        assert_eq!(Cell::new("a", 3).text, CellText::Ascii(b'a'));
        assert_eq!(Cell::new("a", 3).as_str(), "a");
        assert!(matches!(Cell::new("é", 0).text, CellText::Wide(_)));
        assert_eq!(Cell::new("é", 0).as_str(), "é");
        assert_eq!(Cell::new("", 0).as_str(), "");
        assert_eq!(Cell::blank().as_str(), " ");
        // a grid is cleared and resized with blanks
        let mut g = Grid::default();
        g.resize(3, 2);
        assert!(g.cells.iter().all(|c| c.as_str() == " "));
        g.cells[0] = Cell::new("x", 1);
        g.clear();
        assert_eq!(g.cells[0].as_str(), " ");
    }

    /// Needs `nvim` on PATH: cargo test -- --ignored
    #[test]
    #[ignore]
    fn embedded_nvim_renders_a_file() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut n = Nvim::start("nvim -u NONE", None, false, tx).unwrap();
        n.size(60, 8);
        n.open("Cargo.toml", 2, 1, "", "");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let _ = rx.recv_timeout(std::time::Duration::from_millis(100));
            let g = n.grid.lock().unwrap();
            let text: String = g.cells.iter().map(|c| c.as_str()).collect();
            if text.contains("lvim-fuzzy-ui") {
                assert_eq!((g.w, g.h), (60, 8));
                break;
            }
            assert!(std::time::Instant::now() < deadline, "no file text in the grid: {text}");
        }
        n.quit();
    }

    #[test]
    fn targets() {
        assert_eq!(parse_target("src/x.rs:12:3:fn main"), ("src/x.rs".into(), 12, 3));
        assert_eq!(parse_target("a.lua:7"), ("a.lua".into(), 7, 0));
        assert_eq!(parse_target("plain"), ("plain".into(), 0, 0));
    }
}

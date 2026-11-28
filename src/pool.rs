//! `lvim-fuzzy-cli pool`: warm Neovim instances for the preview, so a finder does
//! not wait for Neovim to start.
//!
//! Starting the preview's Neovim with a full config costs a few hundred
//! milliseconds (the deferred plugins, the colorscheme, treesitter), and the
//! finder pays it every time it opens. The pool keeps one instance started and
//! warmed (`nvim --headless --listen SOCK`) and hands it to the next finder,
//! which attaches to it as a UI over the socket instead of starting its own.
//!
//! One instance serves one finder: two UIs on one Neovim share a screen. The
//! rules:
//! - there is always one FREE instance (warming or ready);
//! - a finder takes the free one, and the pool starts warming the next;
//! - when the finder closes (its control connection ends, even on a crash),
//!   its instance is killed. An instance is never given back: a used one
//!   carries the finder's buffers and state, the free one is always fresh.
//!
//! A finder that finds nothing ready starts its own `nvim --embed`, as without
//! the pool: the pool only ever makes the preview faster.
//!
//! Warming: Neovim loads its deferred plugins after the first UI attaches
//! (`UIEnter`), so the pool attaches a UI itself, waits until the instance stops
//! redrawing, and detaches.
//!
//! Control protocol (`<dir>/pool.sock`, one line per request):
//!   `take\t<nvim>\t<appname>` → `ok\t<socket>` | `none`; the connection then
//!                                stays open for the lease
//!   `status`                   → one `<id>\t<state>\t<socket>` line per instance
//!   `reload`                   → `ok` (the instances not in use are restarted)
//!   `stop`                     → `ok`, and the pool exits

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use rmpv::Value;

/// Where the pool's sockets live: `$LVIM_FUZZY_POOL_DIR`, else
/// `$XDG_RUNTIME_DIR/lvim-fuzzy`, else `/tmp/lvim-fuzzy-<uid>`.
pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("LVIM_FUZZY_POOL_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d).join("lvim-fuzzy");
    }
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    std::env::temp_dir().join(format!("lvim-fuzzy-{uid}"))
}

fn control_path(dir: &Path) -> PathBuf {
    dir.join("pool.sock")
}

/// What the pool's instances are: the same `nvim` program and `NVIM_APPNAME` a
/// finder would start. A finder with another one is not served.
fn key(nvim: &str, appname: Option<&str>) -> String {
    format!("{nvim}\t{}", appname.unwrap_or(""))
}

// ── the client side (the finder) ─────────────────────────────────────────────

/// A warm instance lent to this finder. Dropping it (or the finder exiting)
/// gives it back.
pub struct Lease {
    _ctl: UnixStream,
    pub socket: PathBuf,
}

/// Take the free instance, if a pool runs and has one ready for this `nvim` /
/// `appname`. Never blocks for long: no pool, no free instance or any error is
/// `None`, and the finder starts its own Neovim.
pub fn take(nvim: &str, appname: Option<&str>) -> Option<Lease> {
    let mut ctl = UnixStream::connect(control_path(&dir())).ok()?;
    ctl.set_read_timeout(Some(Duration::from_millis(500))).ok()?;
    ctl.write_all(format!("take\t{}\n", key(nvim, appname)).as_bytes()).ok()?;
    let mut line = String::new();
    BufReader::new(ctl.try_clone().ok()?).read_line(&mut line).ok()?;
    let socket = line.trim_end().strip_prefix("ok\t")?;
    // the lease lives as long as this connection; no timeout on it from here
    ctl.set_read_timeout(None).ok()?;
    Some(Lease { _ctl: ctl, socket: PathBuf::from(socket) })
}

/// Send one request to a running pool and read its whole answer; None when no
/// pool runs.
pub fn request(cmd: &str) -> Option<String> {
    let mut ctl = UnixStream::connect(control_path(&dir())).ok()?;
    ctl.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    ctl.write_all(format!("{cmd}\n").as_bytes()).ok()?;
    let mut out = String::new();
    ctl.read_to_string(&mut out).ok()?;
    Some(out)
}

// ── the pool ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Warming,
    Free,
    Taken,
}

impl State {
    fn name(self) -> &'static str {
        match self {
            State::Warming => "warming",
            State::Free => "free",
            State::Taken => "taken",
        }
    }
}

struct Inst {
    id: u64,
    child: Child,
    socket: PathBuf,
    state: State,
}

struct Shared {
    insts: Vec<Inst>,
    next: u64,
    /// Consecutive instances that died or failed to warm: after a few, the pool
    /// waits before trying again (a broken config must not spin).
    failures: u32,
    retry_at: Option<Instant>,
}

struct Pool {
    dir: PathBuf,
    nvim: String,
    appname: Option<String>,
    shared: Mutex<Shared>,
}

/// How long the instance must stay quiet (no redraw) to count as warm, and the
/// most the warm-up may take.
const QUIET: Duration = Duration::from_millis(400);
const WARM_MAX: Duration = Duration::from_secs(10);

/// Start `cmd` from one long-lived thread. The parent-death signal set in
/// `die_with_parent` belongs to the THREAD that forks, not the process: an
/// instance started from a finder's connection thread was killed when that
/// thread ended (the finder closing), so every instance starts here.
fn spawn_child(cmd: Command) -> std::io::Result<Child> {
    type Job = (Command, std::sync::mpsc::Sender<std::io::Result<Child>>);
    static SPAWNER: std::sync::OnceLock<Mutex<std::sync::mpsc::Sender<Job>>> = std::sync::OnceLock::new();
    let tx = SPAWNER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        thread::spawn(move || {
            for (mut cmd, reply) in rx {
                let _ = reply.send(cmd.spawn());
            }
        });
        Mutex::new(tx)
    });
    let (rtx, rrx) = std::sync::mpsc::channel();
    tx.lock().unwrap().send((cmd, rtx)).map_err(|_| std::io::Error::other("the spawner thread is gone"))?;
    rrx.recv().map_err(|_| std::io::Error::other("the spawner thread is gone"))?
}

/// Make the pool's process kill its instances when it dies (Linux), so a
/// killed pool leaves nothing behind.
fn die_with_parent(cmd: &mut Command) {
    #[cfg(target_os = "linux")]
    // SAFETY: prctl is async-signal-safe; nothing else runs between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
}

fn s(v: &str) -> Value {
    Value::from(v)
}

/// Write one msgpack-rpc request.
fn rpc(w: &mut impl Write, id: u64, method: &str, params: Vec<Value>) -> std::io::Result<()> {
    let msg = Value::Array(vec![Value::from(0), Value::from(id), s(method), Value::Array(params)]);
    rmpv::encode::write_value(w, &msg).map_err(std::io::Error::other)?;
    w.flush()
}

impl Pool {
    /// Start one more instance and warm it in the background.
    fn spawn(self: &Arc<Self>) {
        let mut sh = self.shared.lock().unwrap();
        if let Some(t) = sh.retry_at {
            if Instant::now() < t {
                return;
            }
            sh.retry_at = None;
        }
        sh.next += 1;
        let id = sh.next;
        let socket = self.dir.join(format!("nvim-{}-{id}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let words = crate::util::shell_words(&self.nvim).unwrap_or_else(|_| vec!["nvim".into()]);
        let (prog, args) = words.split_first().map(|(p, a)| (p.clone(), a.to_vec())).unwrap_or(("nvim".into(), vec![]));
        let mut cmd = Command::new(prog);
        cmd.args(args)
            .args(["--cmd", "let g:lvim_fuzzy_preview = 1", "-n", "-i", "NONE", "--headless", "--listen"])
            .arg(&socket)
            .env("LVIM_FUZZY_PREVIEW", "1")
            .current_dir(std::env::var_os("HOME").unwrap_or_else(|| "/".into()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(a) = &self.appname {
            cmd.env("NVIM_APPNAME", a);
        }
        crate::nvim::no_pane_env(&mut cmd);
        die_with_parent(&mut cmd);
        let child = match spawn_child(cmd) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("lvim-fuzzy-cli pool: cannot start {}: {e}", self.nvim);
                sh.failures += 1;
                sh.retry_at = Some(Instant::now() + Duration::from_secs(30));
                return;
            }
        };
        sh.insts.push(Inst { id, child, socket: socket.clone(), state: State::Warming });
        drop(sh);
        let pool = self.clone();
        thread::spawn(move || {
            let ok = pool.warm_unlocked(id, &socket);
            let mut sh = pool.shared.lock().unwrap();
            if ok {
                sh.failures = 0;
                if let Some(i) = sh.insts.iter_mut().find(|i| i.id == id && i.state == State::Warming) {
                    i.state = State::Free;
                }
            } else {
                sh.failures += 1;
                if sh.failures >= 3 {
                    eprintln!("lvim-fuzzy-cli pool: Neovim does not warm up; trying again in 30 s");
                    sh.retry_at = Some(Instant::now() + Duration::from_secs(30));
                }
                if let Some(pos) = sh.insts.iter().position(|i| i.id == id) {
                    let mut i = sh.insts.remove(pos);
                    let _ = i.child.kill();
                    let _ = i.child.wait();
                    let _ = std::fs::remove_file(&i.socket);
                }
            }
        });
    }

    /// The warm-up without holding the lock (it takes seconds): a throwaway
    /// handle on the process is enough to notice it dying.
    fn warm_unlocked(&self, id: u64, socket: &Path) -> bool {
        let pid = {
            let sh = self.shared.lock().unwrap();
            match sh.insts.iter().find(|i| i.id == id) {
                Some(i) => i.child.id(),
                None => return false,
            }
        };
        warm_with(socket, &ProcProbe(pid))
    }

    /// Keep exactly one free instance: start one when none is free or warming.
    fn ensure_free(self: &Arc<Self>) {
        let need = {
            let sh = self.shared.lock().unwrap();
            !sh.insts.iter().any(|i| i.state != State::Taken)
        };
        if need {
            self.spawn();
        }
    }

    /// Lend the free instance. Starts warming the next one right away.
    fn take(self: &Arc<Self>, key_wanted: &str) -> Option<(u64, PathBuf)> {
        if key_wanted != key(&self.nvim, self.appname.as_deref()) {
            return None;
        }
        let got = {
            let mut sh = self.shared.lock().unwrap();
            sh.insts.iter_mut().find(|i| i.state == State::Free).map(|i| {
                i.state = State::Taken;
                (i.id, i.socket.clone())
            })
        };
        self.ensure_free();
        got
    }

    /// A finder is done with instance `id`: it is killed (never given back), and
    /// the pool keeps its one free instance.
    fn release(self: &Arc<Self>, id: u64) {
        self.kill(id);
        self.ensure_free();
    }

    fn kill(&self, id: u64) {
        let mut sh = self.shared.lock().unwrap();
        if let Some(pos) = sh.insts.iter().position(|i| i.id == id) {
            let mut i = sh.insts.remove(pos);
            let _ = i.child.kill();
            let _ = i.child.wait();
            let _ = std::fs::remove_file(&i.socket);
        }
    }

    /// Restart every instance not in use (the config changed).
    fn reload(self: &Arc<Self>) {
        let ids: Vec<u64> = {
            let sh = self.shared.lock().unwrap();
            sh.insts.iter().filter(|i| i.state != State::Taken).map(|i| i.id).collect()
        };
        for id in ids {
            self.kill(id);
        }
        {
            let mut sh = self.shared.lock().unwrap();
            sh.failures = 0;
            sh.retry_at = None;
        }
        self.ensure_free();
    }

    fn status(&self) -> String {
        let sh = self.shared.lock().unwrap();
        sh.insts.iter().map(|i| format!("{}\t{}\t{}\n", i.id, i.state.name(), i.socket.display())).collect()
    }

    fn stop(&self) {
        let ids: Vec<u64> = self.shared.lock().unwrap().insts.iter().map(|i| i.id).collect();
        for id in ids {
            self.kill(id);
        }
        let _ = std::fs::remove_file(control_path(&self.dir));
    }

    /// Forget instances whose process ended (killed from outside, crashed).
    fn reap(self: &Arc<Self>) {
        let mut dead = false;
        {
            let mut sh = self.shared.lock().unwrap();
            sh.insts.retain_mut(|i| {
                if let Ok(Some(st)) = i.child.try_wait() {
                    eprintln!("lvim-fuzzy-cli pool: instance #{} ({}) ended: {st}", i.id, i.state.name());
                    let _ = std::fs::remove_file(&i.socket);
                    dead = true;
                    false
                } else {
                    true
                }
            });
        }
        if dead {
            let mut sh = self.shared.lock().unwrap();
            sh.failures += 1;
            if sh.failures >= 3 && sh.retry_at.is_none() {
                sh.retry_at = Some(Instant::now() + Duration::from_secs(30));
            }
        }
        self.ensure_free();
    }

    fn serve_conn(self: &Arc<Self>, conn: UnixStream) {
        let Ok(mut w) = conn.try_clone() else { return };
        // a client that connects and says nothing does not hold a thread of the pool for ever: the first line comes in 5 s
        let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
        let mut r = BufReader::new(conn);
        let mut line = String::new();
        if r.read_line(&mut line).is_err() {
            return;
        }
        // (a lease below waits for the finder to close the connection: no limit then)
        let _ = w.set_read_timeout(None);
        let line = line.trim_end_matches(['\n', '\r']);
        let (cmd, arg) = line.split_once('\t').unwrap_or((line, ""));
        match cmd {
            "take" => match self.take(arg) {
                Some((id, socket)) => {
                    if w.write_all(format!("ok\t{}\n", socket.display()).as_bytes()).is_err() {
                        self.release(id);
                        return;
                    }
                    // the lease: until the finder closes the connection (or dies)
                    let mut sink = [0u8; 64];
                    while matches!(r.read(&mut sink), Ok(n) if n > 0) {}
                    self.release(id);
                }
                None => {
                    let _ = w.write_all(b"none\n");
                }
            },
            "status" => {
                let _ = w.write_all(self.status().as_bytes());
            }
            "reload" => {
                self.reload();
                let _ = w.write_all(b"ok\n");
            }
            "stop" => {
                let _ = w.write_all(b"ok\n");
                self.stop();
                std::process::exit(0);
            }
            _ => {
                let _ = w.write_all(b"error\tunknown request\n");
            }
        }
    }
}

/// A process handle that only polls (the `Child` stays in the pool's table).
struct ProcProbe(u32);

impl ProcProbe {
    fn alive(&self) -> bool {
        // SAFETY: kill with signal 0 only checks that the process exists.
        unsafe { libc::kill(self.0 as libc::pid_t, 0) == 0 }
    }
}

/// Attach a UI, wait until the instance stops redrawing (its deferred plugins
/// loaded), detach; true when it got there. The process is only polled (see
/// `warm_unlocked`).
fn warm_with(socket: &Path, probe: &ProcProbe) -> bool {
    let deadline = Instant::now() + WARM_MAX;
    let conn = loop {
        if Instant::now() >= deadline || !probe.alive() {
            return false;
        }
        if let Ok(c) = UnixStream::connect(socket) {
            break c;
        }
        thread::sleep(Duration::from_millis(20));
    };
    warm_conn(conn, deadline)
}

/// Wait until the instance has stayed quiet for `QUIET` (after a first redraw,
/// with `need_redraw`); true when it got there before `deadline` (or redrew at
/// least once by then).
fn quiet(rx: &std::sync::mpsc::Receiver<()>, deadline: Instant, need_redraw: bool) -> bool {
    let mut seen = !need_redraw;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return seen; // still redrawing at the cap: warm enough
        }
        match rx.recv_timeout(QUIET.min(left)) {
            Ok(()) => seen = true,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if seen => return true,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return false,
        }
    }
}

/// Run in a warmed instance before it is offered: load what the first preview
/// would (the preview module; each common file type's plugins and treesitter
/// grammar, through a throwaway buffer), then leave one empty buffer, so the
/// finder's first frame is its file — not the start screen.
const PREPARE: &str = r#"
pcall(require, "@PREVIEW_MODULE@")
for _, ft in ipairs({ "lua", "markdown", "json", "toml", "yaml", "sh", "python", "rust", "javascript", "typescript" }) do
  local b = vim.api.nvim_create_buf(false, true)
  pcall(vim.api.nvim_set_option_value, "filetype", ft, { buf = b })
  pcall(vim.treesitter.start, b)
  pcall(vim.api.nvim_buf_delete, b, { force = true })
end
pcall(vim.cmd, "silent! enew")
local cur = vim.api.nvim_get_current_buf()
for _, b in ipairs(vim.api.nvim_list_bufs()) do
  if b ~= cur then pcall(vim.api.nvim_buf_delete, b, { force = true }) end
end
"#;

/// The warm-up on an open connection (see `warm_with`).
fn warm_conn(conn: UnixStream, deadline: Instant) -> bool {
    let Ok(mut w) = conn.try_clone() else {
        return false;
    };
    let opts = Value::Map(vec![(s("rgb"), Value::from(true)), (s("ext_linegrid"), Value::from(true))]);
    if rpc(&mut w, 1, "nvim_ui_attach", vec![Value::from(80), Value::from(24), opts]).is_err() {
        return false;
    }
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    thread::spawn(move || {
        let mut r = BufReader::new(conn);
        while let Ok(msg) = rmpv::decode::read_value(&mut r) {
            if let Some(arr) = msg.as_array()
                && arr.first().and_then(Value::as_u64) == Some(2)
                && arr.get(1).and_then(Value::as_str) == Some("redraw")
                && tx.send(()).is_err()
            {
                return;
            }
        }
    });
    // 1. the start: the deferred plugins load once a UI is attached
    if !quiet(&rx, deadline, true) {
        return false;
    }
    // 2. what the first preview would load (the preview module, the commonest file types), and
    // the start screen replaced by an empty buffer: a finder attaching must not see the dashboard
    // first (it flashed in the preview) nor wait for the dashboard to close
    if rpc(&mut w, 3, "nvim_exec_lua", vec![s(&PREPARE.replace("@PREVIEW_MODULE@", crate::nvim::PREVIEW_MODULE)), Value::Array(vec![])]).is_err() {
        return false;
    }
    let ok = quiet(&rx, Instant::now() + WARM_MAX, false);
    let _ = rpc(&mut w, 2, "nvim_ui_detach", vec![]);
    let _ = w.shutdown(std::net::Shutdown::Both);
    ok
}

/// The newest modification time under `root` (bounded depth, hidden entries
/// skipped): the pool restarts its idle instances when the Neovim config or the
/// finder's config changes.
fn newest(root: &Path, depth: usize, acc: &mut SystemTime) {
    let Ok(rd) = std::fs::read_dir(root) else {
        if let Ok(m) = std::fs::metadata(root).and_then(|m| m.modified()) {
            *acc = (*acc).max(m);
        }
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        // hidden entries are state, not config (`.git`, a plugin's own data such as the Control
        // Center's `.configs/`, written on every start — counting them restarted the pool in a loop);
        // `.snapshots` is kept: it changes when the plugins are updated
        if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.') && n != ".snapshots") {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if depth > 0 {
                newest(&p, depth - 1, acc);
            }
        } else if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
            *acc = (*acc).max(m);
        }
    }
}

fn fingerprint(roots: &[PathBuf]) -> SystemTime {
    let mut t = SystemTime::UNIX_EPOCH;
    for r in roots {
        newest(r, 8, &mut t);
    }
    t
}

/// The Neovim config directory the instances load (`NVIM_APPNAME` aware).
fn nvim_config_dir(appname: Option<&str>) -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join(appname.unwrap_or("nvim"))
}

/// The sockets of pools that are gone (`nvim-<pid>-<id>.sock`, the pid being the pool's): a killed pool leaves them behind.
fn remove_dead_sockets(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let pid = name.strip_prefix("nvim-").and_then(|r| r.split('-').next()).and_then(|p| p.parse::<i32>().ok());
        if let Some(pid) = pid
            && name.ends_with(".sock")
            // SAFETY: signal 0 only asks whether the process exists
            && unsafe { libc::kill(pid, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// `lvim-fuzzy-cli pool`: run the pool in the foreground until stopped.
pub fn serve(nvim: &str, appname: Option<&str>, config: Option<&Path>) -> Result<i32, String> {
    let dir = dir();
    crate::util::private_dir(&dir).map_err(|e| format!("cannot use {}: {e}", dir.display()))?;
    remove_dead_sockets(&dir);
    let ctl = control_path(&dir);
    if UnixStream::connect(&ctl).is_ok() {
        return Err(format!("a pool already runs ({})", ctl.display()));
    }
    let _ = std::fs::remove_file(&ctl);
    let listener = UnixListener::bind(&ctl).map_err(|e| format!("cannot listen on {}: {e}", ctl.display()))?;

    let pool = Arc::new(Pool {
        dir,
        nvim: nvim.to_string(),
        appname: appname.map(str::to_string),
        shared: Mutex::new(Shared { insts: Vec::new(), next: 0, failures: 0, retry_at: None }),
    });
    pool.ensure_free();

    // stop cleanly on SIGTERM / SIGINT: the instances go with the pool
    {
        static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        extern "C" fn on_signal(_: libc::c_int) {
            STOP.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        // SAFETY: the handler only stores to an atomic.
        unsafe {
            libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        }
        let p = pool.clone();
        let mut roots = vec![nvim_config_dir(appname)];
        if let Some(c) = config {
            roots.push(c.to_path_buf());
        }
        thread::spawn(move || {
            let mut print = fingerprint(&roots);
            let mut tick = 0u32;
            loop {
                thread::sleep(Duration::from_millis(500));
                if STOP.load(std::sync::atomic::Ordering::SeqCst) {
                    p.stop();
                    std::process::exit(0);
                }
                tick += 1;
                if tick.is_multiple_of(2) {
                    p.reap();
                }
                if tick.is_multiple_of(10) {
                    let now = fingerprint(&roots);
                    if now != print {
                        print = now;
                        eprintln!("lvim-fuzzy-cli pool: the config changed; restarting the idle instances");
                        p.reload();
                    }
                }
            }
        });
    }

    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        let p = pool.clone();
        thread::spawn(move || p.serve_conn(conn));
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_nvim() -> bool {
        Command::new("nvim").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
    }

    fn wait_for(mut f: impl FnMut() -> bool, secs: u64) -> bool {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            if f() {
                return true;
            }
            thread::sleep(Duration::from_millis(50));
        }
        false
    }

    fn states(p: &Pool) -> Vec<State> {
        p.shared.lock().unwrap().insts.iter().map(|i| i.state).collect()
    }

    /// One free instance; taking it warms the next; a used one is killed when
    /// given back, even when it is the only one alive.
    #[test]
    fn the_pool_keeps_one_free_instance() {
        if !has_nvim() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("lvf-pool-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pool = Arc::new(Pool {
            dir: dir.clone(),
            nvim: "nvim -u NONE".into(),
            appname: None,
            shared: Mutex::new(Shared { insts: Vec::new(), next: 0, failures: 0, retry_at: None }),
        });
        let k = key("nvim -u NONE", None);
        pool.ensure_free();
        assert!(wait_for(|| states(&pool) == vec![State::Free], 15), "{:?}", states(&pool));

        // another nvim / appname is not served
        assert!(pool.take(&key("vim", None)).is_none());

        let (a, _) = pool.take(&k).expect("the free one");
        assert!(wait_for(
            || {
                let st = states(&pool);
                st.len() == 2 && st.contains(&State::Taken) && st.contains(&State::Free)
            },
            15
        ));
        // given back: killed
        pool.release(a);
        assert_eq!(states(&pool), vec![State::Free]);

        // even the only one alive is killed, and a fresh free one is started
        let (b, _) = pool.take(&k).unwrap();
        pool.kill_others(b);
        pool.release(b);
        assert!(!pool.shared.lock().unwrap().insts.iter().any(|i| i.id == b));
        assert!(wait_for(|| states(&pool) == vec![State::Free], 15), "{:?}", states(&pool));
        pool.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    impl Pool {
        /// Test helper: kill every instance but `keep`.
        fn kill_others(&self, keep: u64) {
            let ids: Vec<u64> = self.shared.lock().unwrap().insts.iter().map(|i| i.id).filter(|&i| i != keep).collect();
            for id in ids {
                self.kill(id);
            }
        }
    }
}

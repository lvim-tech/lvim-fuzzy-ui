//! The terminal comes back whatever happens: a normal close, a panic, a signal.
//!
//! A tool registers the bytes that undo what it set up (`arm`: leave the alternate screen, show the cursor, stop the mouse and keyboard
//! protocols, …) together with the terminal it wrote them to, and takes them back after its own close (`disarm`). `install` (once, at the
//! start of `main`) puts the safety net in place:
//!
//! * a **panic hook** writes the bytes and leaves raw mode first, so the message of the panic lands on the normal screen, then runs the hook
//!   that was there before;
//! * **SIGTERM, SIGHUP, SIGINT and SIGQUIT** are caught by a handler that does nothing but record the number and write a byte to a pipe
//!   (async-signal-safe). The tool's loop asks `pending_signal()` (or polls `signal_fd()`), leaves by its normal way — its `Drop`s run: state
//!   saved, sockets and temp files removed, the terminal restored — and ends with `finish()`, which exits with `128 + signal`. A SECOND
//!   signal that comes MORE THAN TWO SECONDS after the first — the program is stuck where the loop does not look — puts the terminal back
//!   (the bytes, and the termios it had at the start) straight from the handler and exits at once. A second one sooner (the kernel's SIGHUP
//!   and the shell's, a TERM after a HUP) is only recorded: the loop is on its way out.

use std::fs::File;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Mutex, Once};

/// What is registered: the undo bytes and the terminal they go to.
struct Armed {
    restore: Vec<u8>,
    tty: File,
}

static ARMED: Mutex<Option<Armed>> = Mutex::new(None);
static INSTALLED: Once = Once::new();
/// The signal that came (0: none).
static SIGNAL: AtomicI32 = AtomicI32::new(0);
/// The read / write ends of the self-pipe (-1: none yet).
static PIPE_R: AtomicI32 = AtomicI32::new(-1);
static PIPE_W: AtomicI32 = AtomicI32::new(-1);
/// For the handler of a second signal, which may not lock: the bytes and the descriptor, set by `arm` (the bytes are leaked on purpose,
/// a few dozen of them, once per `arm`).
static RESTORE_PTR: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());
static RESTORE_LEN: AtomicUsize = AtomicUsize::new(0);
static TTY_FD: AtomicI32 = AtomicI32::new(-1);
static RAW_WANTED: AtomicBool = AtomicBool::new(false);
/// When the first signal came (seconds on the monotonic clock; 0: none) and the termios the terminal had at the start (leaked, never freed).
static FIRST_AT: AtomicI64 = AtomicI64::new(0);
static ORIG_TERMIOS: AtomicPtr<libc::termios> = AtomicPtr::new(std::ptr::null_mut());

/// Register what undoes the terminal set-up: `restore` goes to `tty` (a handle of the terminal the tool draws on) in `Drop`-less situations.
/// Call it right after the set-up, and again if the set-up changes.
pub fn arm(restore: Vec<u8>, tty: File) {
    let leaked: &'static mut [u8] = Box::leak(restore.clone().into_boxed_slice());
    // (a new pointer is never seen with an old length: the length is 0 while the pointer changes)
    RESTORE_LEN.store(0, Ordering::SeqCst);
    RESTORE_PTR.store(leaked.as_mut_ptr(), Ordering::SeqCst);
    RESTORE_LEN.store(leaked.len(), Ordering::SeqCst);
    TTY_FD.store(tty.as_raw_fd(), Ordering::SeqCst);
    RAW_WANTED.store(true, Ordering::SeqCst);
    if let Ok(mut g) = ARMED.lock() {
        *g = Some(Armed { restore, tty });
    }
}

/// The tool closed the terminal itself: nothing to undo any more.
pub fn disarm() {
    RAW_WANTED.store(false, Ordering::SeqCst);
    TTY_FD.store(-1, Ordering::SeqCst);
    RESTORE_LEN.store(0, Ordering::SeqCst);
    if let Ok(mut g) = ARMED.lock() {
        *g = None;
    }
}

/// Write the undo bytes and leave raw mode (what the panic hook does). Safe to call twice.
pub fn restore_now() {
    // `try_lock`: a panic may have come while the lock was held by this very thread
    if let Ok(mut g) = ARMED.try_lock()
        && let Some(a) = g.take()
    {
        let _ = (&a.tty).write_all(&a.restore);
        let _ = (&a.tty).flush();
    }
    if RAW_WANTED.swap(false, Ordering::SeqCst) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Which signal came, if one did (the loop leaves by its normal way, then calls `finish`).
pub fn pending_signal() -> Option<i32> {
    match SIGNAL.load(Ordering::SeqCst) {
        0 => None,
        n => Some(n),
    }
}

/// The read end of the self-pipe: readable when a signal came, for a loop that waits in `poll(2)` (-1 before `install`).
pub fn signal_fd() -> i32 {
    PIPE_R.load(Ordering::SeqCst)
}

/// Wake a loop that sleeps on a channel when a signal comes: a thread waits on the self-pipe and sends `()` once (the loop then asks
/// [`pending_signal`] and leaves by its normal way). Without it a loop that sleeps up to half a second sees the signal that late.
pub fn watch(wake: std::sync::mpsc::Sender<()>) {
    let fd = signal_fd();
    if fd < 0 {
        return;
    }
    std::thread::spawn(move || {
        let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        // SAFETY: poll(2) on one descriptor, waiting for ever
        while unsafe { libc::poll(&mut p, 1, -1) } < 0 {
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return;
            }
        }
        let _ = wake.send(());
    });
}

/// After the tool's own clean-up: when a signal ended it, exit with `128 + signal` as a shell expects.
pub fn finish() {
    if let Some(n) = pending_signal() {
        std::process::exit(128 + n);
    }
}

extern "C" fn on_signal(sig: libc::c_int) {
    // async-signal-safe only: atomics, clock_gettime(2), write(2), tcsetattr(3), _exit(2)
    if SIGNAL.swap(sig, Ordering::SeqCst) != 0 {
        // a second one soon after the first: only recorded (the loop is leaving). Later: the program is stuck where the loop does not look
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: clock_gettime fills a local struct
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        if ts.tv_sec - FIRST_AT.load(Ordering::SeqCst) < 2 {
            return;
        }
        let fd = TTY_FD.load(Ordering::SeqCst);
        let len = RESTORE_LEN.load(Ordering::SeqCst);
        let ptr = RESTORE_PTR.load(Ordering::SeqCst);
        if fd >= 0 && len > 0 && !ptr.is_null() {
            // SAFETY: the pointer and length come from a leaked, never freed slice
            unsafe {
                libc::write(fd, ptr as *const libc::c_void, len);
            }
        }
        // the terminal is cooked again as well (raw mode was set after the termios was taken)
        let t = ORIG_TERMIOS.load(Ordering::SeqCst);
        if fd >= 0 && !t.is_null() {
            // SAFETY: tcsetattr is async-signal-safe; the pointer is a leaked, never freed termios
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, t) };
        }
        // SAFETY: _exit is async-signal-safe
        unsafe { libc::_exit(128 + sig) };
    }
    {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: clock_gettime fills a local struct
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        FIRST_AT.store(ts.tv_sec, Ordering::SeqCst);
    }
    let w = PIPE_W.load(Ordering::SeqCst);
    if w >= 0 {
        let b = 1u8;
        // SAFETY: write(2) of one byte to an open descriptor
        unsafe {
            libc::write(w, &b as *const u8 as *const libc::c_void, 1);
        }
    }
}

/// Put the safety net in place, once, at the start of `main`.
pub fn install() {
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_now();
            previous(info);
            // a panic in a thread other than the main one would leave the main loop drawing on the normal screen: the program ends
            if std::thread::current().name() != Some("main") {
                std::process::exit(101);
            }
        }));
        // the terminal as it is NOW (cooked, before any raw mode): what a hurried exit gives back
        if let Ok(tty) = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty") {
            // SAFETY: tcgetattr fills a zeroed termios of ours
            unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(tty.as_raw_fd(), &mut t) == 0 {
                    ORIG_TERMIOS.store(Box::leak(Box::new(t)), Ordering::SeqCst);
                }
            }
        }
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: pipe2 fills two descriptors; both are made non-blocking and close-on-exec
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } == 0 {
            PIPE_R.store(fds[0], Ordering::SeqCst);
            PIPE_W.store(fds[1], Ordering::SeqCst);
        }
        for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT, libc::SIGQUIT] {
            // SAFETY: a plain handler with no flags that could surprise it; the structure is zeroed first
            unsafe {
                let mut sa: libc::sigaction = std::mem::zeroed();
                sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
                libc::sigemptyset(&mut sa.sa_mask);
                sa.sa_flags = 0;
                libc::sigaction(sig, &sa, std::ptr::null_mut());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::FromRawFd;

    /// A pipe instead of a terminal: (read end, write end as a File).
    fn pipe() -> (File, File) {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: two descriptors, both owned by the Files made from them
        unsafe {
            assert_eq!(libc::pipe(fds.as_mut_ptr()), 0);
            (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1]))
        }
    }

    fn read_available(f: &File) -> Vec<u8> {
        use std::io::Read;
        let fd = f.as_raw_fd();
        // SAFETY: setting O_NONBLOCK on our own descriptor
        unsafe {
            let fl = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
        let mut out = Vec::new();
        let _ = (&*f).read_to_end(&mut out);
        out
    }

    #[test]
    fn restore_writes_the_registered_bytes_once() {
        let (r, w) = pipe();
        arm(b"\x1b[?1049l\x1b[?25h".to_vec(), w);
        restore_now();
        assert_eq!(read_available(&r), b"\x1b[?1049l\x1b[?25h");
        restore_now(); // nothing left to write
        assert!(read_available(&r).is_empty());
    }

    #[test]
    fn a_disarmed_terminal_gets_nothing() {
        let (r, w) = pipe();
        arm(b"x".to_vec(), w);
        disarm();
        restore_now();
        assert!(read_available(&r).is_empty());
    }

    #[test]
    fn a_signal_is_recorded_and_wakes_the_pipe() {
        install();
        assert!(signal_fd() >= 0);
        SIGNAL.store(0, Ordering::SeqCst);
        // SAFETY: raising a signal this process has a handler for
        unsafe { libc::raise(libc::SIGHUP) };
        assert_eq!(pending_signal(), Some(libc::SIGHUP));
        let mut b = [0u8; 1];
        // SAFETY: reading one byte from the non-blocking self-pipe
        let n = unsafe { libc::read(signal_fd(), b.as_mut_ptr() as *mut libc::c_void, 1) };
        assert_eq!(n, 1);
        SIGNAL.store(0, Ordering::SeqCst);
    }
}

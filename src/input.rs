//! The terminal's input, read on ONE thread of its own that sleeps in `poll(2)` until there is something to read (a key, the mouse, a paste, a
//! resize) — no timeout, so an idle program is woken by nothing. Each event goes to the receiver `spawn` returns, then the `wake` channel is
//! signalled: the program sleeps on its own wake channel and takes the events with `try_recv` when it wakes.
//!
//! A program that hands the terminal to another (an editor, a shell command) calls [`pause`] first — it returns when the thread no longer
//! reads, so the other program gets every key — and [`resume`] after.

use std::os::fd::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use crossterm::event::Event;

struct Ctl {
    /// `pause` was asked for and `resume` has not come yet.
    want: bool,
    /// The thread has seen it and does not read.
    paused: bool,
}

static CTL: Mutex<Ctl> = Mutex::new(Ctl { want: false, paused: false });
static CV: Condvar = Condvar::new();
/// The write end of the pipe that wakes the thread out of `poll` for a `pause` (-1: no input thread).
static PAUSE_W: AtomicI32 = AtomicI32::new(-1);

/// The most events one read of the terminal hands over at once.
const BATCH: usize = 64;

/// Start the input thread. Events come on the returned receiver; `wake` gets a `()` after each batch.
pub fn spawn(wake: Sender<()>) -> Receiver<Event> {
    let (tx, rx) = channel();
    let (pr, pw) = pipe();
    PAUSE_W.store(pw, Ordering::SeqCst);
    // SIGWINCH is written into a socket that the thread polls with the tty
    let (winch_r, winch_w) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let _ = winch_r.set_nonblocking(true);
    let _ = winch_w.set_nonblocking(true);
    let _ = signal_hook::low_level::pipe::register(signal_hook::consts::SIGWINCH, winch_w);
    std::thread::spawn(move || {
        // (no terminal at all: the thread has nothing to read and ends — `pause` then finds nobody to wait for)
        let tty = std::fs::OpenOptions::new().read(true).open("/dev/tty").ok();
        if tty.is_none() {
            PAUSE_W.store(-1, Ordering::SeqCst);
        }
        let tty_fd = tty.as_ref().map_or(-1, |t| t.as_raw_fd());
        run(
            tty_fd,
            pr,
            winch_r.as_raw_fd(),
            &tx,
            &wake,
            &mut |out| {
                // everything crossterm has ready (it reads the bytes itself; with a zero time it does not, so 1 ms)
                // (at most BATCH at a time: a stream of events — a mouse dragged, a paste with no brackets — reaches the loop as it comes)
                let mut n = 0;
                while n < BATCH && crossterm::event::poll(Duration::from_millis(1)).unwrap_or(false) {
                    match crossterm::event::read() {
                        Ok(ev) => out.push(ev),
                        Err(_) => break,
                    }
                    n += 1;
                }
            },
            &mut |out| {
                if let Ok((w, h)) = crossterm::terminal::size() {
                    out.push(Event::Resize(w, h));
                }
            },
        );
        drop(winch_r);
    });
    rx
}

fn pipe() -> (RawFd, RawFd) {
    let mut fds = [0i32; 2];
    // SAFETY: pipe2 fills two descriptors
    unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) };
    (fds[0], fds[1])
}

fn drain(fd: RawFd) {
    let mut b = [0u8; 64];
    // SAFETY: reads into a local buffer
    while unsafe { libc::read(fd, b.as_mut_ptr().cast(), b.len()) } > 0 {}
}

/// The loop, with the descriptors and the two readers given (so a test can use pipes for a terminal).
fn run(
    tty: RawFd,
    pause_r: RawFd,
    winch_r: RawFd,
    tx: &Sender<Event>,
    wake: &Sender<()>,
    read_tty: &mut dyn FnMut(&mut Vec<Event>),
    read_winch: &mut dyn FnMut(&mut Vec<Event>),
) {
    loop {
        {
            let mut c = CTL.lock().unwrap();
            if c.want {
                c.paused = true;
                CV.notify_all();
                while c.want {
                    c = CV.wait(c).unwrap();
                }
                c.paused = false;
                drop(c);
                drain(pause_r);
                continue;
            }
        }
        let mut fds = [
            libc::pollfd { fd: tty, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: pause_r, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: winch_r, events: libc::POLLIN, revents: 0 },
        ];
        // SAFETY: poll(2) on three descriptors of ours, waiting for ever
        let n = unsafe { libc::poll(fds.as_mut_ptr(), if tty < 0 { 0 } else { 3 }, -1) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                std::thread::sleep(Duration::from_millis(20));
            }
            continue;
        }
        if tty < 0 {
            return;
        }
        if fds[1].revents != 0 {
            drain(pause_r);
            continue;
        }
        let mut out: Vec<Event> = Vec::new();
        if fds[2].revents & libc::POLLIN != 0 {
            drain(winch_r);
            read_winch(&mut out);
        }
        if fds[0].revents != 0 {
            let before = out.len();
            read_tty(&mut out);
            // a full batch: it goes now, and the reader is asked again (what crossterm has queued does not make the tty readable again)
            while out.len() - before >= BATCH {
                for ev in out.drain(..) {
                    if tx.send(ev).is_err() {
                        return;
                    }
                }
                if wake.send(()).is_err() {
                    return;
                }
                read_tty(&mut out);
                if out.is_empty() {
                    break;
                }
                // (`before` is 0 from here: everything in `out` is new)
                if out.len() < BATCH {
                    break;
                }
            }
            // readable but nothing came of it (a hang-up, a partial sequence): not a busy loop
            if out.len() == before && fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        if !out.is_empty() {
            for ev in out {
                if tx.send(ev).is_err() {
                    return;
                }
            }
            if wake.send(()).is_err() {
                return;
            }
        }
    }
}

/// Stop reading (for another program to have the terminal). Returns when the thread has stopped, or after 200 ms.
pub fn pause() {
    let w = PAUSE_W.load(Ordering::SeqCst);
    if w < 0 {
        return;
    }
    let mut c = CTL.lock().unwrap();
    c.want = true;
    // SAFETY: one byte into the pipe that wakes the thread
    unsafe { libc::write(w, b"x".as_ptr().cast(), 1) };
    let (g, _) = CV.wait_timeout_while(c, Duration::from_millis(200), |c| !c.paused).unwrap();
    drop(g);
}

/// Read again.
pub fn resume() {
    let mut c = CTL.lock().unwrap();
    c.want = false;
    CV.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// The pause state is one per process: the tests that use it run one at a time.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A pipe stands for the terminal: a byte written is an event within a few ms, `pause` returns at once, and while paused nothing is read.
    #[test]
    fn a_byte_is_an_event_and_pause_stops_the_reading() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (tty_r, tty_w) = pipe();
        let (pause_r, pause_w) = pipe();
        let (winch_r, _winch_w) = pipe();
        PAUSE_W.store(pause_w, Ordering::SeqCst);
        let (tx, rx) = channel();
        let (wake, wake_rx) = channel();
        std::thread::spawn(move || {
            run(
                tty_r,
                pause_r,
                winch_r,
                &tx,
                &wake,
                &mut |out| {
                    let mut b = [0u8; 16];
                    // SAFETY: reads into a local buffer
                    let n = unsafe { libc::read(tty_r, b.as_mut_ptr().cast(), b.len()) };
                    for c in &b[..n.max(0) as usize] {
                        out.push(Event::Key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char(*c as char))));
                    }
                },
                &mut |_| {},
            );
        });
        let put = |c: u8| unsafe { libc::write(tty_w, [c].as_ptr().cast(), 1) };
        let t = Instant::now();
        put(b'a');
        wake_rx.recv_timeout(Duration::from_secs(2)).expect("woken");
        assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
        assert!(matches!(rx.try_recv(), Ok(Event::Key(k)) if k.code == crossterm::event::KeyCode::Char('a')));
        // pause returns quickly and nothing is read while paused
        let t = Instant::now();
        pause();
        assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
        put(b'b');
        assert!(wake_rx.recv_timeout(Duration::from_millis(200)).is_err(), "read while paused");
        resume();
        wake_rx.recv_timeout(Duration::from_secs(2)).expect("read after resume");
        assert!(matches!(rx.try_recv(), Ok(Event::Key(k)) if k.code == crossterm::event::KeyCode::Char('b')));
    }

    /// The same on a pseudo-terminal: a byte typed on the master side is an event within a few ms (the terminal's own line discipline in
    /// between, in raw mode as the tools run it).
    #[test]
    fn a_byte_on_a_pty_is_an_event() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (mut master, mut slave) = (0i32, 0i32);
        // SAFETY: openpty fills two descriptors
        assert_eq!(unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), std::ptr::null()) }, 0);
        // raw mode: a byte reaches the reader at once, not at the end of a line
        // SAFETY: tcgetattr / cfmakeraw / tcsetattr on the descriptor just made
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            libc::tcgetattr(slave, &mut t);
            libc::cfmakeraw(&mut t);
            libc::tcsetattr(slave, libc::TCSANOW, &t);
        }
        let (pause_r, _pause_w) = pipe();
        let (winch_r, _winch_w) = pipe();
        let (tx, rx) = channel();
        let (wake, wake_rx) = channel();
        std::thread::spawn(move || {
            run(
                slave,
                pause_r,
                winch_r,
                &tx,
                &wake,
                &mut |out| {
                    let mut b = [0u8; 16];
                    // SAFETY: reads into a local buffer
                    let n = unsafe { libc::read(slave, b.as_mut_ptr().cast(), b.len()) };
                    for c in &b[..n.max(0) as usize] {
                        out.push(Event::Key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char(*c as char))));
                    }
                },
                &mut |_| {},
            );
        });
        let t = Instant::now();
        // SAFETY: one byte to the master
        unsafe { libc::write(master, b"q".as_ptr().cast(), 1) };
        wake_rx.recv_timeout(Duration::from_secs(2)).expect("woken by the pty");
        assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
        assert!(matches!(rx.try_recv(), Ok(Event::Key(k)) if k.code == crossterm::event::KeyCode::Char('q')));
        // the master closes (the terminal window went): the thread does not spin
        // SAFETY: closing a descriptor of ours
        unsafe { libc::close(master) };
        std::thread::sleep(Duration::from_millis(200));
    }
}

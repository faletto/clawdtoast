//! Cards from the daemon (one JSON line each on stdin), delivered to a poll loop. A thread reads
//! stdin and writes a byte to a pipe so the loop can wait on its display connection and the feed
//! with one `poll`.

use crate::core::Card;
use crate::log;
use std::io::BufRead;
use std::os::fd::RawFd;
use std::sync::mpsc;

pub struct Feed {
    /// `Some(card)` for each card, `None` once stdin has closed.
    pub rx: mpsc::Receiver<Option<Card>>,
    /// Readable whenever `rx` may have something.
    pub wake_fd: RawFd,
}

impl Feed {
    pub fn spawn() -> Option<Feed> {
        let mut fds = [0 as RawFd; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
            log::line("overlay: pipe failed");
            return None;
        }
        let (wake_rd, wake_wr) = (fds[0], fds[1]);
        let (tx, rx) = mpsc::channel::<Option<Card>>();
        std::thread::spawn(move || {
            let send = |msg: Option<Card>| {
                let _ = tx.send(msg);
                unsafe { libc::write(wake_wr, [0u8].as_ptr().cast(), 1) };
            };
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                match serde_json::from_str::<Card>(&line) {
                    Ok(card) => send(Some(card)),
                    Err(e) => log::line(&format!("overlay: bad card: {e}")),
                }
            }
            send(None);
        });
        Some(Feed { rx, wake_fd: wake_rd })
    }

    /// Empties the wake pipe; call after `poll` reports it readable.
    pub fn drain(&self) {
        let mut buf = [0u8; 64];
        while unsafe { libc::read(self.wake_fd, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
    }
}

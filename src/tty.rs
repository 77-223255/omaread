//! Getting an answer out of the terminal.
//!
//! A question travels as escape sequences on stdout and the answer arrives on
//! stdin as more escape sequences, so the two only mean anything in raw mode
//! with nothing else reading the input. The wait is bounded twice: a terminal
//! that answers does so in milliseconds, so a stretch of silence ends the wait
//! well before the total deadline — while a terminal that says nothing at all
//! costs only the deadline, never the session's patience.

use std::io::Write;
use std::time::{Duration, Instant};

/// How much of the input a single answer is read in: a reply is a handful of
/// short sequences, so anything left after one read is another program's
/// keystrokes rather than ours.
const CHUNK: usize = 1024;

/// Writes `ask` in one go and gathers what the terminal sends back, until the
/// input has been quiet for `quiet` or `timeout` runs out — whichever first.
///
/// Silence before the first byte waits out the whole `timeout`: the answer to
/// a question over a slow link is slow, not absent, and giving up early would
/// read a delayed terminal as a mute one.
pub fn gather(ask: &str, timeout: Duration, quiet: Duration) -> String {
    let mut out = std::io::stdout();
    if out.write_all(ask.as_bytes()).and_then(|()| out.flush()).is_err() {
        // No write, no question: an answer will not come, and waiting for one
        // would be waiting out the timeout for nothing.
        return String::new();
    }

    let mut reply = String::new();
    let deadline = Instant::now() + timeout;
    let mut last = Instant::now();
    let mut buffer = [0u8; CHUNK];

    loop {
        let now = Instant::now();
        if now >= deadline || (!reply.is_empty() && now.duration_since(last) >= quiet) {
            return reply;
        }
        // Poll until the earlier of the two boundaries, then look at the clock
        // again: the answer may have arrived between the read and the check.
        let boundary = if reply.is_empty() {
            deadline
        } else {
            (last + quiet).min(deadline)
        };
        match wait(boundary) {
            Wait::Data => {}
            Wait::Timeout => continue,
            // The far end is gone: whatever it had to say has been said.
            Wait::Closed => return reply,
        }
        // The file descriptor is read directly rather than through `stdin`:
        // the standard input keeps a buffer of its own, and bytes stuck in it
        // are invisible to the event reader the session goes back to — a
        // keystroke swallowed there would never reach the app.
        // Safety: a plain read on stdin, with the buffer this function owns.
        let read = unsafe {
            libc::read(
                libc::STDIN_FILENO,
                buffer.as_mut_ptr().cast::<libc::c_void>(),
                buffer.len(),
            )
        };
        if read <= 0 {
            let interrupted = read < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
            if interrupted {
                // A signal, not the answer: the clock above is the real
                // bound, so go round and consult it again.
                continue;
            }
            // An error, or the end of the input: nothing more will come.
            return reply;
        }
        let read = read as usize;
        reply.push_str(&String::from_utf8_lossy(&buffer[..read]));
        last = Instant::now();
    }
}

/// What waiting on the input found.
enum Wait {
    Data,
    Timeout,
    Closed,
}

/// Waits for input without spinning, using poll on the raw file descriptor.
fn wait(until: Instant) -> Wait {
    let remaining = until.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Wait::Timeout;
    }
    let mut fds = [libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    }];
    let millis = remaining.as_millis().min(i32::MAX as u128) as i32;
    // Safety: the descriptor is stdin and the array is valid for the call.
    let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, millis) };
    if ready <= 0 {
        return Wait::Timeout;
    }
    if fds[0].revents & libc::POLLIN != 0 {
        Wait::Data
    } else if fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        // Hangup without data: reading would say the same, and polling again
        // would report it at once — a spin until the deadline.
        Wait::Closed
    } else {
        Wait::Timeout
    }
}

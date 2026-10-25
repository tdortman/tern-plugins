//! The controlling terminal, opened directly: under `nh`, nom's stdin is a
//! pipe from nix, so the SDK's stdin/stdout `Tty` can't be used.

use std::{
    fs::{File, OpenOptions},
    io::{self, Read as _, Write as _},
    os::fd::AsRawFd as _,
    time::Duration,
};

use tern_sdk::term::Terminal;

/// `/dev/tty` in raw mode, but with signals kept so Ctrl+C still interrupts
/// the whole job (nix, nh and this process) as it does without Tern.
pub struct DevTty {
    /// The open terminal.
    file: File,
    /// The termios before raw mode.
    saved: libc::termios,
}

#[allow(unsafe_code, reason = "termios calls on the open /dev/tty")]
impl DevTty {
    /// Opens the controlling terminal and enters raw mode.
    pub fn open() -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
        let fd = file.as_raw_fd();
        let mut saved = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: fd is an open terminal and saved has room for a termios.
        if unsafe { libc::tcgetattr(fd, saved.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: tcgetattr filled it.
        let saved = unsafe { saved.assume_init() };
        let mut raw = saved;
        // SAFETY: raw is a valid termios.
        unsafe { libc::cfmakeraw(&raw mut raw) };
        raw.c_lflag |= libc::ISIG;
        raw.c_oflag |= libc::OPOST | libc::ONLCR;
        // SAFETY: fd is an open terminal, raw a valid termios.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw const raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { file, saved })
    }
}

#[allow(unsafe_code, reason = "poll and termios calls on the open /dev/tty")]
impl Terminal for DevTty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all(bytes)?;
        self.file.flush()
    }

    fn read(&mut self, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<Option<usize>> {
        let ms = timeout.map_or(-1, |t| i32::try_from(t.as_millis()).unwrap_or(i32::MAX));
        let mut pfd = libc::pollfd {
            fd: self.file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        match unsafe { libc::poll(&raw mut pfd, 1, ms) } {
            0 => Ok(None),
            n if n < 0 => {
                let err = io::Error::last_os_error();
                // A signal (Ctrl+C) interrupted the wait: the caller checks for it.
                if err.kind() == io::ErrorKind::Interrupted {
                    Ok(None)
                } else {
                    Err(err)
                }
            }
            _ => match self.file.read(buf) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => Ok(None),
                other => other.map(Some),
            },
        }
    }

    fn restore(&mut self) -> io::Result<()> {
        // SAFETY: the fd is still open; saved came from tcgetattr.
        if unsafe { libc::tcsetattr(self.file.as_raw_fd(), libc::TCSANOW, &raw const self.saved) }
            != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

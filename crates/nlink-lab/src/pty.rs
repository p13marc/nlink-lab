//! Pseudo-terminals (#267): a terminal for a command someone drives from
//! elsewhere — the daemon's `shell` and `exec` on a tty, the desktop
//! app's terminal.
//!
//! ```no_run
//! # fn demo(lab: &nlink_lab::RunningLab) -> std::io::Result<()> {
//! let pty = nlink_lab::pty::Pty::open(24, 80)?;
//! let mut prepared = lab
//!     .prepare_exec("host", "bash", &[], &Default::default(), Default::default())
//!     .unwrap();
//! pty.attach(prepared.command())?;
//! let child = prepared.spawn().unwrap();
//! let master = pty.into_master(); // read the output, write the input
//! # Ok(()) }
//! ```

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;

/// A pseudo-terminal: the master, which its user reads and writes, and
/// the slave, the command's terminal.
#[derive(Debug)]
pub struct Pty {
    master: OwnedFd,
    slave: OwnedFd,
}

fn winsize(rows: u16, cols: u16) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

impl Pty {
    /// A new terminal of `rows` × `cols`.
    pub fn open(rows: u16, cols: u16) -> std::io::Result<Self> {
        let (mut master, mut slave): (RawFd, RawFd) = (-1, -1);
        let size = winsize(rows, cols);
        // SAFETY: openpty writes two descriptors; no name buffer, no
        // termios.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openpty returned two descriptors this process owns.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        for fd in [&master, &slave] {
            // SAFETY: fcntl on a descriptor this process owns. Only the
            // copies a command's stdio gets cross an exec.
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        Ok(Self { master, slave })
    }

    /// Make `command` run on the terminal: its stdin, stdout and stderr,
    /// and its controlling terminal, in a session of its own (so a
    /// signal to its process group reaches what it runs).
    pub fn attach(&self, command: &mut std::process::Command) -> std::io::Result<()> {
        command
            .stdin(self.slave.try_clone()?)
            .stdout(self.slave.try_clone()?)
            .stderr(self.slave.try_clone()?);
        // SAFETY: only async-signal-safe syscalls; stdio is set up when
        // it runs.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }

    /// The master alone: once the command runs, the slave must be closed
    /// here, or the master never reads the end of the command's output.
    pub fn into_master(self) -> OwnedFd {
        self.master
    }
}

/// Set the size of the terminal whose master is `master`.
pub fn resize(master: &impl AsRawFd, rows: u16, cols: u16) -> std::io::Result<()> {
    let size = winsize(rows, cols);
    // SAFETY: TIOCSWINSZ reads a winsize from the pointer.
    if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// The size of the terminal whose master is `master`: `(rows, cols)`.
pub fn size(master: &impl AsRawFd) -> std::io::Result<(u16, u16)> {
    let mut size = winsize(0, 0);
    // SAFETY: TIOCGWINSZ writes a winsize to the pointer.
    if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCGWINSZ, &mut size) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((size.ws_row, size.ws_col))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn a_command_runs_on_the_terminal() {
        let pty = Pty::open(24, 80).unwrap();
        let mut cmd = std::process::Command::new("sh");
        cmd.args([
            "-c",
            "stty size; tty >/dev/null && echo on-a-tty; read line; echo got $line",
        ]);
        pty.attach(&mut cmd).unwrap();
        let mut child = cmd.spawn().unwrap();
        // the command keeps its stdio's descriptors, slave copies, until
        // it is dropped
        drop(cmd);
        let master = pty.into_master();
        let mut file = std::fs::File::from(master);
        file.write_all(b"hello\n").unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 1024];
        // reading the master ends with EIO once the command has exited
        while let Ok(n) = file.read(&mut buf) {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        assert!(child.wait().unwrap().success());
        let out = String::from_utf8_lossy(&out);
        assert!(out.contains("24 80"), "{out}");
        assert!(out.contains("on-a-tty"), "{out}");
        assert!(out.contains("got hello"), "{out}");
    }

    #[test]
    fn a_terminal_resizes() {
        let pty = Pty::open(10, 20).unwrap();
        let master = pty.into_master();
        assert_eq!(size(&master).unwrap(), (10, 20));
        resize(&master, 50, 132).unwrap();
        assert_eq!(size(&master).unwrap(), (50, 132));
    }
}

//! `nlink-lab-gui pty-bridge`: a shell on a lab's node, through the
//! daemon, on this process's terminal. The app's terminals run it: the
//! terminal widget (or emulator) gives it a pty, and it relays — keys to
//! the daemon's `shell`, the shell's output back, window sizes as they
//! change. Hidden: it is the app's, not a command for people.

use std::io::Write as _;
use std::process::ExitCode;

use clap::Parser;
use nlink_lab_client::{CallEvent, Client, Endpoint};
use nlink_lab_shared::api::{DataStream, Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Parser)]
#[command(name = "nlink-lab-gui pty-bridge", hide = true)]
struct Args {
    #[arg(long, value_parser = Endpoint::parse)]
    daemon: Endpoint,
    #[arg(short = 'L', long)]
    lab: String,
    node: String,
    /// The shell to run (the node's default otherwise).
    #[arg(long)]
    shell: Option<String>,
}

/// The terminal's size: `(rows, cols)`, `(24, 80)` when it cannot be read.
fn size() -> (u16, u16) {
    // SAFETY: TIOCGWINSZ fills the winsize it is given.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_row > 0 {
            (ws.ws_row, ws.ws_col)
        } else {
            (24, 80)
        }
    }
}

/// The terminal in raw mode until dropped: keys go to the remote shell as
/// they are typed, its pty echoes them.
struct Raw(Option<libc::termios>);

impl Raw {
    fn enter() -> Self {
        // SAFETY: plain termios calls on stdin; a failure (no tty) leaves
        // the mode as it was.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return Raw(None);
            }
            let saved = t;
            libc::cfmakeraw(&mut t);
            libc::tcsetattr(0, libc::TCSANOW, &t);
            Raw(Some(saved))
        }
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        if let Some(t) = self.0 {
            // SAFETY: puts back what `enter` read.
            unsafe { libc::tcsetattr(0, libc::TCSANOW, &t) };
        }
    }
}

pub fn main(args: impl IntoIterator<Item = String>) -> ExitCode {
    let args = match Args::try_parse_from(args) {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            return ExitCode::from(64);
        }
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("nlink-lab-gui: {e}");
            return ExitCode::FAILURE;
        }
    };
    let raw = Raw::enter();
    let code = rt.block_on(relay(args));
    // a read of stdin may still be blocked in tokio's pool, and dropping
    // the runtime would wait for it: until a key was pressed, after the
    // shell had ended
    rt.shutdown_background();
    match code {
        Ok(code) => {
            drop(raw);
            ExitCode::from(code)
        }
        Err(e) => {
            // said where the user looks, and kept until they have read it
            print!("\r\nnlink-lab-gui: {e}\r\n(press a key to close)\r\n");
            let _ = std::io::stdout().flush();
            let mut b = [0u8; 1];
            let _ = std::io::Read::read(&mut std::io::stdin(), &mut b);
            drop(raw);
            ExitCode::FAILURE
        }
    }
}

async fn relay(args: Args) -> Result<u8, String> {
    let winch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
        .map_err(|e| e.to_string())?;
    relay_io(
        &args.daemon,
        Shell {
            lab: args.lab,
            node: args.node,
            shell: args.shell,
        },
        size(),
        tokio::io::stdin(),
        tokio::io::stdout(),
        Some(winch),
    )
    .await
}

/// Where the shell is.
pub struct Shell {
    pub lab: String,
    pub node: String,
    pub shell: Option<String>,
}

/// The relay itself, between `input`/`output` and the daemon's `shell` of
/// `rows` × `cols`: the shell's exit code, or why there is none. `winch`
/// says when the size changed (read again from the terminal).
pub async fn relay_io<R, W>(
    endpoint: &Endpoint,
    target: Shell,
    (rows, cols): (u16, u16),
    mut input: R,
    mut output: W,
    mut winch: Option<tokio::signal::unix::Signal>,
) -> Result<u8, String>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let client = Client::connect(
        endpoint,
        concat!("nlink-lab-gui-pty/", env!("CARGO_PKG_VERSION")),
    )
    .await
    .map_err(|e| e.to_string())?;
    let mut call = client
        .start(Request::Shell {
            lab: target.lab,
            node: target.node,
            shell: target.shell,
            rows,
            cols,
        })
        .await
        .map_err(|e| e.to_string())?;
    let mut buf = [0u8; 4096];
    let mut input_open = true;
    loop {
        let resized = async {
            match winch.as_mut() {
                Some(w) => w.recv().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            n = input.read(&mut buf), if input_open => match n {
                Ok(0) | Err(_) => {
                    input_open = false;
                    let _ = call.eof();
                }
                Ok(n) => call.stdin(&buf[..n]).map_err(|e| e.to_string())?,
            },
            _ = resized => {
                let (rows, cols) = size();
                let _ = call.resize(rows, cols);
            }
            event = call.next() => match event {
                Some(CallEvent::Data(DataStream::Pty | DataStream::Stdout | DataStream::Stderr, bytes)) => {
                    output.write_all(&bytes).await.map_err(|e| e.to_string())?;
                    output.flush().await.map_err(|e| e.to_string())?;
                }
                Some(CallEvent::End(Ok(Response::Exited { code, signal, .. }))) => {
                    let code = code.unwrap_or_else(|| 128 + signal.unwrap_or(0));
                    return Ok(code.clamp(0, 255) as u8);
                }
                Some(CallEvent::End(Ok(_))) => return Ok(0),
                Some(CallEvent::End(Err(e))) => return Err(e.message),
                Some(_) => {}
                None => return Err("the connection to the daemon closed".into()),
            },
        }
    }
}

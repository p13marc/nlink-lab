//! `nlink-labd`: serve the nlink-lab API, or relay it over ssh.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use nlink_labd::config::DEFAULT_CONFIG;
use nlink_labd::{Config, listen};

#[derive(Parser)]
#[command(
    name = "nlink-labd",
    version,
    about = "The nlink-lab host daemon: every lab of the host behind one API"
)]
struct Cli {
    /// The configuration file (default: /etc/nlink-lab/daemon.toml, when
    /// it exists).
    #[arg(long, value_name = "PATH", global = true)]
    config: Option<PathBuf>,

    /// The socket, instead of the configuration's.
    #[arg(long, value_name = "PATH", global = true)]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the API (what it does without a command). Under systemd it
    /// takes the socket of `nlink-labd.socket`; otherwise it binds its own.
    Serve,
    /// Relay stdin and stdout to the daemon's socket: what a client runs
    /// over ssh (`ssh HOST nlink-labd dial-stdio`, the `ssh://HOST`
    /// endpoint) to reach a remote daemon as the ssh user.
    DialStdio,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let explicit = cli.config.is_some();
    let path = cli.config.unwrap_or_else(|| DEFAULT_CONFIG.into());
    let mut config = match Config::load(&path, explicit) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("nlink-labd: {e}");
            return ExitCode::from(1);
        }
    };
    if let Some(socket) = cli.socket {
        config.socket = socket;
    }
    match cli.command.unwrap_or(Command::Serve) {
        Command::DialStdio => {
            let socket = config.socket.clone();
            let result = runtime().block_on(dial_stdio(&socket));
            // stdin is read on a blocking thread the runtime would wait for
            match result {
                Ok(()) => std::process::exit(0),
                Err(e) => {
                    eprintln!("nlink-labd: {e}");
                    std::process::exit(1);
                }
            }
        }
        Command::Serve => match serve(config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("nlink-labd: {e}");
                ExitCode::from(1)
            }
        },
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime")
}

fn serve(config: Config) -> std::io::Result<()> {
    // before the runtime's threads: it changes the environment
    let (listener, bound) = match listen::activated() {
        Some(listener) => (listener, None),
        None => (listen::bind(&config.socket)?, Some(config.socket.clone())),
    };
    if let Err(e) = nlink_lab::state::ensure_state_root() {
        tracing::warn!("the state root: {e}");
    }
    let rt = runtime();
    let result = rt.block_on(async {
        let shutdown = nlink_lab::CancelToken::new();
        let signals = shutdown.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let (Ok(mut term), Ok(mut int)) = (
                signal(SignalKind::terminate()),
                signal(SignalKind::interrupt()),
            ) else {
                return;
            };
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
            listen::notify("STOPPING=1");
            tracing::info!("stopping");
            signals.cancel();
        });
        tracing::info!(
            "serving the nlink-lab API on {}",
            bound.as_deref().map_or_else(
                || "the socket systemd passed".into(),
                |p| p.display().to_string()
            )
        );
        listen::notify("READY=1");
        nlink_labd::serve(config, listener, shutdown).await
    });
    if let Some(path) = bound {
        let _ = std::fs::remove_file(path);
    }
    rt.shutdown_timeout(std::time::Duration::from_secs(2));
    result
}

/// Relay stdin to the socket and the socket to stdout until the daemon
/// closes the connection.
async fn dial_stdio(socket: &Path) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let stream = tokio::net::UnixStream::connect(socket).await.map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("cannot reach the daemon at {}: {e}", socket.display()),
        )
    })?;
    let (mut from_daemon, mut to_daemon) = stream.into_split();
    tokio::spawn(async move {
        let _ = tokio::io::copy(&mut tokio::io::stdin(), &mut to_daemon).await;
        // the client is done: the daemon ends its calls and closes
        let _ = to_daemon.shutdown().await;
    });
    let mut stdout = tokio::io::stdout();
    tokio::io::copy(&mut from_daemon, &mut stdout).await?;
    stdout.flush().await
}

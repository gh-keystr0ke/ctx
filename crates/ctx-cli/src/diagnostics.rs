use std::{
    fs::{self, File, OpenOptions},
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use chrono::Utc;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};

pub(super) struct Diagnostics {
    debug_path: Option<PathBuf>,
}

impl Diagnostics {
    pub(super) fn debug_path(&self) -> Option<&Path> {
        self.debug_path.as_deref()
    }
}

pub(super) fn init(verbosity: u8, debug: bool, root: &Path) -> io::Result<Diagnostics> {
    let terminal_level = match verbosity {
        0 => LevelFilter::WARN,
        1 => LevelFilter::INFO,
        2 => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    };
    let terminal = tracing_subscriber::fmt::layer()
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .with_target(false)
        .with_filter(terminal_level);

    let (file_layer, debug_path) = if debug {
        let path = debug_path(root);
        let file = open_debug_file(&path)?;
        let writer = SharedWriter(Arc::new(Mutex::new(file)));
        let layer = tracing_subscriber::fmt::layer()
            .json()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .with_filter(LevelFilter::TRACE);
        (Some(layer), Some(path))
    } else {
        (None, None)
    };

    tracing_subscriber::registry()
        .with(terminal)
        .with(file_layer)
        .try_init()
        .map_err(io::Error::other)?;
    Ok(Diagnostics { debug_path })
}

fn debug_path(root: &Path) -> PathBuf {
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    root.join(".ctx")
        .join("logs")
        .join(format!("ctx-{timestamp}-{}.jsonl", std::process::id()))
}

fn open_debug_file(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[derive(Clone)]
struct SharedWriter(Arc<Mutex<File>>);

impl Write for SharedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("debug log writer lock poisoned"))?
            .write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("debug log writer lock poisoned"))?
            .flush()
    }
}

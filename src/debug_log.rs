use std::{
    collections::VecDeque,
    env,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_LINES: usize = 500;
static LOGGER: OnceLock<Mutex<Logger>> = OnceLock::new();

struct Logger {
    lines: VecDeque<String>,
    file: File,
}

pub fn init() -> io::Result<PathBuf> {
    let path = log_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "=== session start: {} ===", timestamp())?;
    file.flush()?;
    let _ = LOGGER.set(Mutex::new(Logger {
        lines: VecDeque::with_capacity(MAX_LINES),
        file,
    }));
    log(format!("debug log file: {}", path.display()));
    Ok(path)
}

pub fn log(message: impl AsRef<str>) {
    let Some(logger) = LOGGER.get() else {
        return;
    };
    let line = format!("[{}] {}", timestamp(), message.as_ref());
    let mut logger = logger
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if logger.lines.len() == MAX_LINES {
        logger.lines.pop_front();
    }
    logger.lines.push_back(line.clone());
    let _ = writeln!(logger.file, "{line}");
    let _ = logger.file.flush();
}

pub fn lines() -> Vec<String> {
    LOGGER.get().map_or_else(Vec::new, |logger| {
        logger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lines
            .iter()
            .cloned()
            .collect()
    })
}

fn log_path() -> PathBuf {
    let state = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")));
    state
        .unwrap_or_else(|| PathBuf::from("."))
        .join("com.columbiafoundry.ZuneTUI/zunetui.log")
}

fn timestamp() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", elapsed.as_secs(), elapsed.subsec_millis())
}

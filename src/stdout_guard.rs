use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    os::fd::{AsRawFd, RawFd},
    sync::{Mutex, MutexGuard},
};

static STDOUT_LOCK: Mutex<()> = Mutex::new(());

pub fn synchronized<R>(operation: impl FnOnce() -> R) -> R {
    let _lock = lock_stdout();
    operation()
}

pub fn silenced<R>(operation: impl FnOnce() -> R) -> R {
    let _lock = lock_stdout();
    let _redirect = StdoutRedirect::new().ok();
    operation()
}

fn lock_stdout() -> MutexGuard<'static, ()> {
    STDOUT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct StdoutRedirect {
    saved_stdout: RawFd,
    _null: File,
}

impl StdoutRedirect {
    fn new() -> io::Result<Self> {
        io::stdout().flush()?;
        let null = OpenOptions::new().write(true).open("/dev/null")?;

        // SAFETY: STDOUT_FILENO is an open process fd. `dup` returns a new fd
        // owned by this guard, which is closed in Drop.
        let saved_stdout = unsafe { libc::dup(libc::STDOUT_FILENO) };
        if saved_stdout == -1 {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: both fds are valid here. `dup2` atomically replaces fd 1
        // while preserving the saved duplicate for restoration.
        if unsafe { libc::dup2(null.as_raw_fd(), libc::STDOUT_FILENO) } == -1 {
            // SAFETY: `saved_stdout` was created successfully above and is
            // owned by this function until the guard is constructed.
            unsafe { libc::close(saved_stdout) };
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            saved_stdout,
            _null: null,
        })
    }
}

impl Drop for StdoutRedirect {
    fn drop(&mut self) {
        // SAFETY: flushing all C streams ensures libmtp's stdio buffer is
        // drained to /dev/null before stdout is restored.
        unsafe { libc::fflush(std::ptr::null_mut()) };
        // SAFETY: `saved_stdout` remains owned and open for the guard's whole
        // lifetime. Restore fd 1, then close the duplicate exactly once.
        unsafe {
            libc::dup2(self.saved_stdout, libc::STDOUT_FILENO);
            libc::close(self.saved_stdout);
        }
    }
}

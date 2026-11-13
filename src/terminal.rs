//! Platform termios/ioctl bindings and scoped cleanup for the details view.
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

static CANCELLED: LazyLock<Arc<AtomicBool>> = LazyLock::new(|| Arc::new(AtomicBool::new(false)));

pub fn cancelled() -> bool {
    CANCELLED.load(Ordering::Relaxed)
}

pub fn cancel() {
    CANCELLED.store(true, Ordering::Relaxed);
}

pub struct Terminal {
    original: Option<libc::termios>,
    signals: Vec<signal_hook::SigId>,
}

impl Terminal {
    pub fn enter(raw: bool) -> io::Result<Self> {
        CANCELLED.store(false, Ordering::Relaxed);
        let mut guard = Self {
            original: None,
            signals: Vec::new(),
        };
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
            guard
                .signals
                .push(signal_hook::flag::register(signal, Arc::clone(&CANCELLED))?);
        }
        // SAFETY: isatty only inspects the process's standard input descriptor.
        if raw && unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 {
            // Zeroed, not uninit: Linux fills only the kernel's 19 control
            // characters of libc's larger c_cc array; the rest must not be
            // read as uninitialized memory.
            let mut original = std::mem::MaybeUninit::<libc::termios>::zeroed();
            // SAFETY: tcgetattr fills the fields it supports on success; every
            // other byte is already zero.
            if unsafe { libc::tcgetattr(libc::STDIN_FILENO, original.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let original = unsafe { original.assume_init() };
            guard.original = Some(original);
            let mut changed = original;
            changed.c_lflag &= !(libc::ICANON | libc::ECHO);
            changed.c_cc[libc::VMIN] = 1;
            changed.c_cc[libc::VTIME] = 0;
            // SAFETY: changed is a termios obtained from this descriptor.
            if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &changed) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(guard)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if let Some(original) = &self.original {
            // SAFETY: restore the exact attributes read on entry, including
            // VMIN/VTIME and flags that may already have been disabled.
            unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, original) };
            // Darwin marks PENDIN when transitioning back to canonical mode.
            // FIONREAD performs the kernel's pending-input reprocessing
            // without consuming bytes, clearing this transient flag. A second
            // tcsetattr would retain it (XNU tty.c, ttnread and TIOCSETA).
            #[cfg(target_os = "macos")]
            if original.c_lflag & libc::PENDIN == 0 {
                let mut pending: libc::c_int = 0;
                // SAFETY: FIONREAD writes one c_int for this descriptor.
                unsafe { libc::ioctl(libc::STDIN_FILENO, libc::FIONREAD, &mut pending) };
            }
        }
        for id in self.signals.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

pub const RESTORE: &str = "\x1b[?1006l\x1b[?1000l\x1b[?25h";

pub struct Screen<'a> {
    pub output: &'a mut dyn Write,
    restored: bool,
}
impl<'a> Screen<'a> {
    pub fn new(output: &'a mut dyn Write) -> Self {
        Self {
            output,
            restored: false,
        }
    }
    pub fn start(&mut self) -> io::Result<()> {
        write!(self.output, "\x1b[?25l\x1b[?1000h\x1b[?1006h")
    }
    pub fn finish(&mut self) -> io::Result<()> {
        self.output.write_all(RESTORE.as_bytes())?;
        self.output.flush()?;
        self.restored = true;
        Ok(())
    }
}
impl Drop for Screen<'_> {
    fn drop(&mut self) {
        if !self.restored {
            let _ = self.finish();
        }
    }
}

#[cfg(test)]
thread_local! { static TEST_SIZE: std::cell::Cell<Option<(usize, usize)>> = const { std::cell::Cell::new(None) }; }
#[cfg(test)]
pub fn test_size(size: (usize, usize)) {
    TEST_SIZE.set(Some(size));
}

pub fn size() -> Option<(usize, usize)> {
    #[cfg(test)]
    if let Some(size) = TEST_SIZE.get() {
        return Some(size);
    }
    for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO] {
        let mut size = std::mem::MaybeUninit::<libc::winsize>::uninit();
        // SAFETY: TIOCGWINSZ initializes this platform's winsize on success.
        if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, size.as_mut_ptr()) } == 0 {
            let size = unsafe { size.assume_init() };
            if size.ws_row > 0 && size.ws_col > 0 {
                return Some((usize::from(size.ws_row), usize::from(size.ws_col)));
            }
        }
    }
    None
}

pub fn wait_for_close(closes: impl Fn(&[u8]) -> bool) -> io::Result<()> {
    while !cancelled() {
        let mut poll = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll points to one initialized descriptor.
        let result = unsafe { libc::poll(&mut poll, 1, 50) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if result > 0 {
            let mut input = [0; 256];
            match io::stdin().read(&mut input) {
                Ok(0) => return Ok(()),
                Ok(count) if closes(&input[..count]) => return Ok(()),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Interrupted,
        "details interrupted",
    ))
}

/// External labels cannot inject C0/C1 terminal commands, line breaks or
/// bidirectional formatting; bound their display length as well.
pub fn safe_text(text: &str) -> String {
    text.chars()
        .filter(|character| {
            !character.is_control()
                && !matches!(*character, '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(80)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_retries_cleanup_on_write_error_and_reports_final_flush_error() {
        struct FailOnce {
            bytes: Vec<u8>,
            fail: bool,
        }
        impl Write for FailOnce {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.fail {
                    self.fail = false;
                    return Err(io::Error::other("fixture write failure"));
                }
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut writer = FailOnce {
            bytes: vec![],
            fail: true,
        };
        {
            let mut screen = Screen::new(&mut writer);
            assert!(screen.start().is_err());
        }
        assert_eq!(writer.bytes, RESTORE.as_bytes());
        struct BadFlush;
        impl Write for BadFlush {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::other("fixture flush failure"))
            }
        }
        assert!(Screen::new(&mut BadFlush).finish().is_err());
    }

    #[test]
    fn external_labels_cannot_inject_terminal_or_bidi_controls() {
        let status = crate::codex::Status {
            windows: vec![],
            plan_type: Some("pro\x1b[2J\x1b]52;c;test\x07\n\u{202e}x".into()),
            credits: Some(crate::codex::Credits {
                has_credits: true,
                unlimited: false,
                balance: Some("10\x1b[?25l\r\u{009b}m".into()),
            }),
            observed_at: None,
        };
        let screen = crate::popup_theme::PopupTheme::default().codex(
            Some(&status),
            Some(&crate::resets::SavedResets::Unavailable),
            0,
        );
        assert!(
            !screen.contains("\x1b[2J")
                && !screen.contains("\x1b]52;")
                && !screen.contains("\x1b[?25l")
        );
        assert!(
            !screen.contains('\u{202e}')
                && !screen.contains('\u{009b}')
                && !screen.contains('\x07')
        );
        assert_eq!(safe_text(&"x".repeat(1000)).len(), 80);
    }
}

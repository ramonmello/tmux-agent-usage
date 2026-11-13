//! Bounded stdio helpers. No reader threads can outlive a child; every exit
//! path kills (if still running) and reaps the process owned by this guard.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const MAX_OUTPUT: usize = 1024 * 1024;

pub struct Process {
    child: Child,
    input: Option<ChildStdin>,
    output: ChildStdout,
    pending: Vec<u8>,
    received: usize,
    deadline: Instant,
}

impl Process {
    pub fn spawn(command: &mut Command, timeout: Duration) -> Result<Self, String> {
        if crate::terminal::cancelled() {
            return Err("query interrupted".into());
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "cannot start helper process")?;
        let input = child.stdin.take().expect("piped stdin");
        let output = child.stdout.take().expect("piped stdout");
        let process = Self {
            child,
            input: Some(input),
            output,
            pending: Vec::new(),
            received: 0,
            deadline: Instant::now() + timeout,
        };
        nonblocking(process.input.as_ref().unwrap().as_raw_fd())?;
        nonblocking(process.output.as_raw_fd())?;
        Ok(process)
    }

    pub fn write(&mut self, mut bytes: &[u8]) -> Result<(), String> {
        let input = self.input.as_mut().ok_or("helper stdin is closed")?;
        while !bytes.is_empty() {
            ready(input.as_raw_fd(), libc::POLLOUT, self.deadline)?;
            match input.write(bytes) {
                Ok(0) => return Err("helper closed stdin".into()),
                Ok(count) => bytes = &bytes[count..],
                Err(error) if retry(&error) => continue,
                Err(_) => return Err("cannot write to helper".into()),
            }
        }
        Ok(())
    }

    pub fn close_input(&mut self) {
        self.input.take();
    }

    fn read_more(&mut self) -> Result<bool, String> {
        let mut bytes = [0; 8192];
        loop {
            ready(self.output.as_raw_fd(), libc::POLLIN, self.deadline)?;
            match self.output.read(&mut bytes) {
                Ok(0) => return Ok(false),
                Ok(count) => {
                    self.received += count;
                    if self.received > MAX_OUTPUT {
                        return Err("helper response is too large".into());
                    }
                    self.pending.extend_from_slice(&bytes[..count]);
                    return Ok(true);
                }
                Err(error) if retry(&error) => continue,
                Err(_) => return Err("cannot read helper response".into()),
            }
        }
    }

    pub fn line(&mut self) -> Result<Option<Vec<u8>>, String> {
        let mut scanned = 0;
        loop {
            check(self.deadline)?;
            if let Some(end) = self.pending[scanned..]
                .iter()
                .position(|byte| *byte == b'\n')
            {
                return Ok(Some(self.pending.drain(..=scanned + end).collect()));
            }
            scanned = self.pending.len();
            if !self.read_more()? {
                return Ok((!self.pending.is_empty()).then(|| std::mem::take(&mut self.pending)));
            }
        }
    }

    pub fn finish(mut self) -> Result<(ExitStatus, Vec<u8>), String> {
        self.close_input();
        while self.read_more()? {}
        let status = loop {
            check(self.deadline)?;
            if let Some(status) = self.child.try_wait().map_err(|_| "cannot reap helper")? {
                break status;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        Ok((status, std::mem::take(&mut self.pending)))
    }

    #[cfg(test)]
    pub fn id(&self) -> libc::pid_t {
        self.child.id() as libc::pid_t
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.close_input();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn output(command: &mut Command, input: &[u8], timeout: Duration) -> Result<Vec<u8>, String> {
    let mut process = Process::spawn(command, timeout)?;
    process.write(input)?;
    let (status, output) = process.finish()?;
    if status.success() {
        Ok(output)
    } else {
        Err("helper process failed".into())
    }
}

fn retry(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
    )
}

fn nonblocking(fd: libc::c_int) -> Result<(), String> {
    // SAFETY: fd belongs to a live pipe. fcntl changes only its status flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err("cannot configure helper pipe".into());
    }
    Ok(())
}

fn check(deadline: Instant) -> Result<(), String> {
    if crate::terminal::cancelled() {
        Err("query interrupted".into())
    } else if Instant::now() >= deadline {
        Err("helper took too long".into())
    } else {
        Ok(())
    }
}

fn ready(fd: libc::c_int, events: libc::c_short, deadline: Instant) -> Result<(), String> {
    loop {
        check(deadline)?;
        let mut poll = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let timeout = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(50) as i32;
        // SAFETY: poll points to one initialized pollfd for this call.
        let result = unsafe { libc::poll(&mut poll, 1, timeout) };
        if result > 0 {
            return Ok(());
        }
        if result < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return Err("cannot poll helper pipe".into());
        }
    }
}

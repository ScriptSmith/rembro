//! The parts of rembro's side that differ between Linux, macOS and Windows: signals,
//! processes and file modes. The agent runs only on Linux, so it uses libc directly.

use std::fs;
use std::path::Path;
use std::process::{Command, ExitStatus};

pub use imp::*;

// Linux's numbers, which the agent takes in `Call::Kill`. macOS uses the same ones.
pub const SIGKILL: i32 = 9;
pub const SIGTERM: i32 = 15;

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    use std::os::unix::process::{CommandExt, ExitStatusExt};

    /// Calls `f` with each SIGINT, SIGTERM or SIGHUP. Run this before any other thread
    /// starts: it blocks those signals so that one thread can wait for them, and threads
    /// inherit the mask.
    pub fn on_signals(f: impl Fn(i32) + Send + Sync + 'static) {
        let set = unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            for s in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                libc::sigaddset(&mut set, s);
            }
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
            set
        };
        std::thread::spawn(move || {
            loop {
                let mut sig = 0;
                if unsafe { libc::sigwait(&set, &mut sig) } == 0 {
                    f(sig);
                }
            }
        });
    }

    /// Makes `cmd` start with no signals blocked. rembro blocks the signals it waits for, and
    /// children would otherwise inherit that, leaving them deaf to SIGTERM and Ctrl-C.
    pub fn unblock_signals(cmd: &mut Command) {
        unsafe {
            cmd.pre_exec(|| {
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::pthread_sigmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
                Ok(())
            });
        }
    }

    /// Starts `cmd` in a process group of its own, so that Ctrl-C in the terminal reaches only rembro.
    pub fn own_group(cmd: &mut Command) {
        cmd.process_group(0);
    }

    /// Sends `sig` to the process group that `own_group` started.
    pub fn signal_group(pid: u32, sig: i32) {
        unsafe { libc::kill(-(pid as i32), sig) };
    }

    pub fn exit_signal(status: &ExitStatus) -> Option<i32> {
        status.signal()
    }

    pub fn pid_alive(pid: u32) -> bool {
        let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
        alive || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    /// Whether another process holds an fcntl lock on `path`, as Firefox does on its profile's .parentlock.
    pub fn fcntl_locked(path: &Path) -> bool {
        use std::os::fd::AsRawFd;
        let Ok(f) = fs::OpenOptions::new().read(true).write(true).open(path) else { return false };
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = libc::F_WRLCK as _;
        lock.l_whence = libc::SEEK_SET as _;
        let ok = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_GETLK, &mut lock) } == 0;
        // F_UNLCK is an int on Linux and a short on macOS.
        #[allow(clippy::unnecessary_cast)]
        let unlocked = libc::F_UNLCK as i32;
        ok && lock.l_type as i32 != unlocked
    }

    /// Enough of a running process to tell whether it is rembro: its command line on Linux,
    /// and its executable's path on macOS.
    #[cfg(not(target_os = "macos"))]
    pub fn process_name(pid: u32) -> Option<String> {
        fs::read(format!("/proc/{pid}/cmdline")).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    #[cfg(target_os = "macos")]
    pub fn process_name(pid: u32) -> Option<String> {
        let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let n = unsafe { libc::proc_pidpath(pid as i32, buf.as_mut_ptr().cast(), buf.len() as u32) };
        (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
    }

    pub fn hostname() -> String {
        let mut buf = [0u8; 256];
        if unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) } != 0 {
            return String::new();
        }
        let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }

    #[cfg(target_os = "linux")]
    pub fn uid() -> u32 {
        unsafe { libc::getuid() }
    }

    pub fn set_mode(path: &Path, mode: u32) {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
    }

    pub fn is_executable(m: &fs::Metadata) -> bool {
        m.is_file() && m.permissions().mode() & 0o111 != 0
    }

    pub fn is_socket(path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket())
    }

    pub fn writable(path: &Path) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let Ok(p) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
        unsafe { libc::access(p.as_ptr(), libc::W_OK) == 0 }
    }

    /// Creates a directory that only this user can open, unless it exists.
    pub fn create_private_dir(path: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
            r => r,
        }
    }

    /// Whether `path` is a directory, not a link to one, that this user owns and only this
    /// user can open.
    pub fn is_private_dir(path: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        fs::symlink_metadata(path)
            .is_ok_and(|m| m.is_dir() && m.uid() == unsafe { libc::getuid() } && m.mode() & 0o077 == 0)
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    use std::sync::OnceLock;
    use std::time::Duration;

    type Handle = *mut std::ffi::c_void;

    const SIGHUP: i32 = 1;
    const SIGINT: i32 = 2;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn QueryFullProcessImageNameW(process: Handle, flags: u32, name: *mut u16, size: *mut u32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    static HANDLER: OnceLock<Box<dyn Fn(i32) + Send + Sync>> = OnceLock::new();

    unsafe extern "system" fn on_ctrl(kind: u32) -> i32 {
        // CTRL_C_EVENT and CTRL_BREAK_EVENT are 0 and 1. The others mean that the console
        // window is closing or the user is logging off.
        let sig = if kind <= 1 { SIGINT } else { SIGHUP };
        if let Some(f) = HANDLER.get() {
            f(sig);
        }
        if kind > 1 {
            // Windows ends the process when this returns, so wait while rembro closes the browser.
            std::thread::sleep(Duration::from_secs(30));
        }
        1
    }

    /// Calls `f` when the console sends Ctrl-C or Ctrl-Break (as SIGINT) or closes (as SIGHUP).
    pub fn on_signals(f: impl Fn(i32) + Send + Sync + 'static) {
        if HANDLER.set(Box::new(f)).is_ok() {
            unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
        }
    }

    pub fn unblock_signals(_cmd: &mut Command) {}

    /// Starts `cmd` in a process group of its own, so that Ctrl-C in the console reaches only rembro.
    pub fn own_group(cmd: &mut Command) {
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }

    /// Asks the process tree at `pid` to close its windows, or with SIGKILL ends it.
    pub fn signal_group(pid: u32, sig: i32) {
        let mut cmd = Command::new("taskkill");
        if sig == SIGKILL {
            cmd.arg("/F");
        }
        let _ = cmd.args(["/T", "/PID", &pid.to_string()]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }

    pub fn exit_signal(_status: &ExitStatus) -> Option<i32> {
        None
    }

    /// The path of a running process's executable.
    pub fn process_name(pid: u32) -> Option<String> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            let mut buf = vec![0u16; 32768];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) != 0;
            CloseHandle(h);
            ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
        }
    }

    pub fn hostname() -> String {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }

    pub fn set_mode(_path: &Path, _mode: u32) {}

    pub fn create_private_dir(path: &Path) -> std::io::Result<()> {
        match fs::create_dir(path) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
            r => r,
        }
    }

    /// Whether `path` is a directory. The system's temporary directory is the user's own.
    pub fn is_private_dir(path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
    }

    pub fn is_executable(m: &fs::Metadata) -> bool {
        m.is_file()
    }

    pub fn is_socket(_path: &Path) -> bool {
        false
    }
}

//! Shell lookup and inspection of processes running in a pty.

use std::ffi::CStr;
use std::os::fd::RawFd;
use std::path::PathBuf;

/// The user's shell: `$SHELL`, else the passwd entry, else `/bin/sh`.
pub fn user_shell() -> String {
    pick_shell(std::env::var("SHELL").ok(), passwd_shell())
}

/// The argv for a new shell: the `shell` setting (an argv list) when it is
/// non-empty, else the user's login shell.
pub fn shell_argv(setting: Vec<String>) -> Vec<String> {
    if setting.first().is_some_and(|s| !s.trim().is_empty()) {
        setting
    } else {
        vec![user_shell()]
    }
}

fn pick_shell(env: Option<String>, passwd: Option<String>) -> String {
    [env, passwd]
        .into_iter()
        .flatten()
        .find(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/bin/sh".into())
}

fn passwd_shell() -> Option<String> {
    // SAFETY: getpwuid returns a pointer to static storage or NULL; we copy
    // the string out immediately. The app is single-threaded on this path.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_shell.is_null() {
            return None;
        }
        Some(
            CStr::from_ptr((*pw).pw_shell)
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// The foreground process group of the terminal behind pty master `fd`.
pub fn foreground_pgrp(fd: RawFd) -> Option<i32> {
    // SAFETY: tcgetpgrp only reads from the descriptor.
    let pg = unsafe { libc::tcgetpgrp(fd) };
    (pg > 0).then_some(pg)
}

/// Whether something other than the spawned child (the shell) owns the
/// foreground of the pty, i.e. the user is running a program in it.
pub fn is_busy(fd: RawFd, child_pid: i32) -> bool {
    matches!(foreground_pgrp(fd), Some(pg) if pg != child_pid)
}

/// Short command name of `pid` (`/proc/PID/comm`).
pub fn command_name(pid: i32) -> Option<String> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(s.trim_end().to_string()).filter(|s| !s.is_empty())
}

/// Effective uid of `pid` from `/proc/PID/status`.
pub fn effective_uid(pid: i32) -> Option<u32> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    parse_euid(&s)
}

fn parse_euid(status: &str) -> Option<u32> {
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(2)?.parse().ok()
}

/// Current directory of `pid` (`/proc/PID/cwd`).
pub fn process_cwd(pid: i32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// What the foreground of a terminal looks like, for the header tint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tint {
    #[default]
    None,
    Root,
    Remote,
}

pub fn classify(command: Option<&str>, euid: Option<u32>, self_is_root: bool) -> Tint {
    if self_is_root || euid == Some(0) {
        return Tint::Root;
    }
    match command {
        Some("ssh" | "mosh" | "mosh-client" | "et" | "telnet") => Tint::Remote,
        _ => Tint::None,
    }
}

/// Arguments of `pid` (`/proc/PID/cmdline`).
pub fn command_line(pid: i32) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| {
            b.split(|&c| c == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// The host a remote-login command line (`ssh`, `mosh`, `telnet`, `et`)
/// connects to, without the user name: `ssh -p 22 me@box ls` → `box`.
pub fn remote_host(argv: &[String]) -> Option<String> {
    let program = argv.first()?.rsplit('/').next()?;
    // Options that take a separate value, per program.
    let with_value: &[char] = match program {
        "ssh" => &[
            'B', 'b', 'c', 'D', 'E', 'e', 'F', 'I', 'i', 'J', 'L', 'l', 'm', 'O', 'o', 'p', 'Q',
            'R', 'S', 'W', 'w',
        ],
        "telnet" => &['b', 'e', 'l', 'n', 'X'],
        "mosh" | "et" => &[],
        _ => return None,
    };
    let mut args = argv[1..].iter();
    while let Some(a) = args.next() {
        if a == "--" {
            return args.next().map(|h| strip_user(h));
        }
        if let Some(opts) = a.strip_prefix('-') {
            if opts.starts_with('-') {
                // Long option (mosh, et): `--opt=value` or a flag; `--port`
                // style values are rare enough to ignore.
                continue;
            }
            // Bundled short flags: the value follows the first option that
            // takes one, in the same word or the next.
            if let Some(i) = opts.find(|c| with_value.contains(&c)) {
                if i + 1 == opts.len() {
                    args.next();
                }
            }
            continue;
        }
        return Some(strip_user(a));
    }
    None
}

fn strip_user(dest: &str) -> String {
    let dest = dest.strip_prefix("ssh://").unwrap_or(dest);
    let host = dest.rsplit('@').next().unwrap_or(dest);
    // ssh://host:port
    match host.rsplit_once(':') {
        Some((h, port)) if !h.contains(':') && port.chars().all(|c| c.is_ascii_digit()) => {
            h.to_string()
        }
        _ => host.to_string(),
    }
}

pub fn tint_for_pgrp(pgrp: Option<i32>) -> Tint {
    // SAFETY: geteuid has no preconditions.
    let self_root = unsafe { libc::geteuid() } == 0;
    match pgrp {
        Some(pg) => classify(command_name(pg).as_deref(), effective_uid(pg), self_root),
        None => classify(None, None, self_root),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    #[test]
    fn remote_hosts() {
        let v = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(remote_host(&v("ssh box")).as_deref(), Some("box"));
        assert_eq!(
            remote_host(&v("/usr/bin/ssh -p 2222 -A me@box.lan uptime")).as_deref(),
            Some("box.lan")
        );
        assert_eq!(
            remote_host(&v("ssh -tt -oBatchMode=yes -i key root@10.0.0.1")).as_deref(),
            Some("10.0.0.1")
        );
        assert_eq!(remote_host(&v("ssh -lme host")).as_deref(), Some("host"));
        assert_eq!(
            remote_host(&v("ssh ssh://me@host:2222")).as_deref(),
            Some("host")
        );
        assert_eq!(
            remote_host(&v("mosh --ssh=ssh me@host")).as_deref(),
            Some("host")
        );
        assert_eq!(
            remote_host(&v("telnet -l me host 23")).as_deref(),
            Some("host")
        );
        assert_eq!(remote_host(&v("ssh -v")), None);
        assert_eq!(remote_host(&v("bash -c ssh")), None);
    }

    #[test]
    fn shell_choice() {
        assert_eq!(
            pick_shell(Some("/bin/zsh".into()), Some("/bin/bash".into())),
            "/bin/zsh"
        );
        assert_eq!(
            pick_shell(Some(" ".into()), Some("/bin/bash".into())),
            "/bin/bash"
        );
        assert_eq!(pick_shell(None, None), "/bin/sh");
        assert!(!user_shell().is_empty());
    }

    #[test]
    fn shell_setting() {
        assert_eq!(
            shell_argv(vec!["/bin/fish".into(), "-l".into()]),
            ["/bin/fish", "-l"]
        );
        assert_eq!(shell_argv(vec![]), [user_shell()]);
        assert_eq!(shell_argv(vec!["".into()]), [user_shell()]);
    }

    #[test]
    fn euid_parsing() {
        let s = "Name:\tsudo\nUid:\t1000\t0\t0\t0\nGid:\t1\n";
        assert_eq!(parse_euid(s), Some(0));
        assert_eq!(parse_euid("Name: x\n"), None);
        let me = unsafe { libc::geteuid() };
        assert_eq!(effective_uid(std::process::id() as i32), Some(me));
    }

    #[test]
    fn classification() {
        assert_eq!(classify(Some("bash"), Some(1000), false), Tint::None);
        assert_eq!(classify(Some("bash"), Some(0), false), Tint::Root);
        assert_eq!(classify(Some("ssh"), Some(1000), false), Tint::Remote);
        assert_eq!(classify(Some("ssh"), Some(1000), true), Tint::Root);
    }

    #[test]
    fn proc_helpers() {
        let me = std::process::id() as i32;
        assert!(command_name(me).is_some());
        assert_eq!(process_cwd(me), std::env::current_dir().ok());
        assert_eq!(command_name(-5), None);
    }

    /// Opens a pty, runs `script` with `sh` as session leader on it and
    /// returns (master fd, child).
    fn spawn_on_pty(script: &str) -> (RawFd, std::process::Child) {
        let mut master = -1;
        let mut slave = -1;
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert_eq!(rc, 0);
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(script)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            cmd.pre_exec(move || {
                if libc::setsid() < 0 || libc::ioctl(slave, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::dup2(slave, 0);
                Ok(())
            });
        }
        let child = cmd.spawn().unwrap();
        unsafe { libc::close(slave) };
        (master, child)
    }

    /// Whether `f` comes true within a while (generous: CI runners can be
    /// slow to fork and exec).
    fn wait_for(mut f: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(20);
        while Instant::now() < end {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn idle_shell_is_not_busy() {
        let (fd, mut child) = spawn_on_pty("read x");
        let pid = child.id() as i32;
        assert!(wait_for(|| foreground_pgrp(fd) == Some(pid)));
        assert!(!is_busy(fd, pid));
        child.kill().ok();
        child.wait().ok();
        unsafe { libc::close(fd) };
    }

    #[test]
    fn foreground_job_is_busy() {
        // With job control on, the shell puts `sleep` in its own foreground
        // process group, like an interactive shell running a program.
        let (fd, mut child) = spawn_on_pty("set -m; sleep 60; true");
        let pid = child.id() as i32;
        assert!(wait_for(|| is_busy(fd, pid)), "foreground never changed");
        // The new group's leader is the shell's fork until it has run
        // `exec sleep`: wait for that rather than look once.
        let mut pg = None;
        let mut last = None;
        let ran = wait_for(|| {
            pg = foreground_pgrp(fd).filter(|&g| g != pid);
            last = pg.and_then(command_name);
            last.as_deref() == Some("sleep")
        });
        assert!(ran, "foreground {pg:?} is {last:?}, not sleep");
        assert!(is_busy(fd, pid));
        let pg = pg.unwrap();
        unsafe { libc::kill(-pg, libc::SIGKILL) };
        child.kill().ok();
        child.wait().ok();
        unsafe { libc::close(fd) };
    }
}

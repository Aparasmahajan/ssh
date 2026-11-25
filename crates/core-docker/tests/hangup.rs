//! `EXEC_HANGUP` against real processes on this machine.
//!
//! The script only reads `/proc` and sends signals, so the host is as good a
//! test bed as a container, and it runs here without Docker. Each available
//! POSIX shell (dash, busybox ash) is tested, since images ship one or the
//! other.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use opspilot_core_docker::{session_token, shell_command, EXEC_HANGUP};

fn shells() -> Vec<Vec<&'static str>> {
    let mut out = Vec::new();
    for (bin, argv) in [("dash", vec!["dash"]), ("busybox", vec!["busybox", "sh"])] {
        let found = Command::new("sh")
            .args(["-c", &format!("command -v {bin}")])
            .stdout(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if found {
            out.push(argv);
        }
    }
    out
}

/// Alive and not a zombie.
fn alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(s) => !matches!(s.rsplit_once(") ").map(|(_, r)| r.as_bytes()[0]), Some(b'Z')),
        Err(_) => false,
    }
}

fn wait_for_pids(file: &Path, n: usize) -> Vec<(String, u32)> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let got: Vec<(String, u32)> = std::fs::read_to_string(file)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (k, v) = l.split_once(' ')?;
                Some((k.to_string(), v.parse().ok()?))
            })
            .collect();
        if got.len() >= n {
            return got;
        }
        assert!(Instant::now() < deadline, "setup never wrote its pids: {got:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("opspilot-hangup-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A stand-in for one exec session: `setsid` gives the wrapper its own session,
/// as a TTY exec does, then it runs the real wrapper line up to the point
/// where it would exec the shell.
fn session(token: &str, body: &str) -> Child {
    // `shell_command` ends by exec'ing bash/sh; replace that tail with the
    // test's body but keep the exact export the product uses.
    let export = shell_command(token);
    let export = export.split("; ").next().unwrap().to_string();
    Command::new("setsid")
        .args(["sh", "-c", &format!("{export}; {body}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("setsid")
}

#[test]
fn hangs_up_the_closed_session_and_nothing_else() {
    let shells = shells();
    assert!(!shells.is_empty(), "no POSIX shell to test with");
    for argv in shells {
        let dir = tmp(argv[0]);
        let pids = dir.join("pids");
        let p = pids.display();
        let closed = session_token();
        let other = session_token();

        // The closed tab: a foreground job, a background job, a job that
        // scrubbed its environment (what sudo does), and a daemon that left
        // the session with setsid.
        let mut a = session(
            &closed,
            &format!(
                "echo \"leader $$\" >> {p}; \
                 sleep 600 & echo \"bg $!\" >> {p}; \
                 env -i sleep 600 & echo \"scrubbed $!\" >> {p}; \
                 setsid sleep 600 & echo \"daemon $!\" >> {p}; \
                 exec sleep 600"
            ),
        );
        // Another tab, still open.
        let mut b = session(&other, &format!("sleep 600 & echo \"other $!\" >> {p}; wait"));
        // Unrelated to OpsPilot entirely.
        let mut c = Command::new("setsid").args(["sleep", "600"]).spawn().unwrap();
        let got = wait_for_pids(&pids, 5);

        let out = Command::new(argv[0])
            .args(&argv[1..])
            .args(["-c", EXEC_HANGUP, "sh", &closed])
            .output()
            .unwrap();
        assert!(out.status.success(), "{argv:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stderr.is_empty(), "{argv:?} wrote errors: {}", String::from_utf8_lossy(&out.stderr));
        let _ = a.wait(); // reap the leader so it does not read as a zombie
        std::thread::sleep(Duration::from_millis(200));

        let pid = |k: &str| got.iter().find(|(n, _)| n == k).map(|(_, p)| *p).unwrap();
        for gone in ["leader", "bg", "scrubbed"] {
            assert!(!alive(pid(gone)), "{argv:?}: `{gone}` survived the hangup");
        }
        let survivors = [("daemon", pid("daemon")), ("other tab", pid("other")), ("unrelated", c.id())];
        for (what, p) in survivors {
            assert!(alive(p), "{argv:?}: `{what}` was hung up");
        }

        for (_, p) in &got {
            let _ = Command::new("kill").arg(p.to_string()).stderr(Stdio::null()).status();
        }
        let _ = c.kill();
        let _ = c.wait();
        let _ = b.kill();
        let _ = b.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn an_unknown_token_signals_nothing() {
    let out = Command::new("sh")
        .args(["-c", EXEC_HANGUP, "sh", &session_token()])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "0");
    assert!(out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_missing_token_is_refused() {
    let out = Command::new("sh").args(["-c", EXEC_HANGUP, "sh"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

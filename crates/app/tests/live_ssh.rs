//! Predictive echo and agent auth against real OpenSSH, over a slow link.
//!
//! ```text
//! cargo test -p opspilot-app --test live_ssh -- --ignored --nocapture
//! ```
//!
//! Self-contained and unprivileged. It starts, in a temp dir:
//! - `sshd` as the current user on a free loopback port, with its own host key,
//!   `authorized_keys` and config (no PAM, no password auth);
//! - a private `ssh-agent` holding a throwaway key;
//! - a TCP proxy that delays every byte by `ONE_WAY` in each direction, which
//!   is §4's "200 ms RTT WAN" row without needing root for `tc netem`.
//!
//! It then connects through the app's own path, `remote::connect` (auth comes
//! from the agent alone), opens a terminal session through `registry()` like the
//! UI does, and types into real shells. Nothing it starts outlives the test, and
//! nothing touches the developer's own agent, keys or `known_hosts`.
//!
//! What it measures, per keystroke:
//! - **visible**: write → the typed character is in the session's snapshot;
//! - **echo**: write → the server's echo has arrived, which is when a terminal
//!   without prediction could first show the character.
//!
//! "Visible" is the backend half of keystroke → glyph. The webview adds one IPC
//! round trip and a canvas paint, which this test cannot see.

use std::sync::Arc;
use std::time::{Duration, Instant};

use opspilot_app::remote::{self, AuthSpec, ConnectRequest};
use opspilot_app::session::{registry, Session, SessionId};
use opspilot_test_sshd::{delay_proxy, wait_for, which, Sshd};

/// Each direction. 100 ms each way is a 200 ms round trip.
const ONE_WAY: Duration = Duration::from_millis(100);

struct Fixture {
    sshd: Sshd,
    proxy_port: u16,
    _rt: tokio::runtime::Runtime,
}

impl Fixture {
    fn dir(&self) -> &std::path::Path {
        &self.sshd.dir
    }
}

/// Real sshd and a private agent (`opspilot-test-sshd`), behind a proxy that
/// adds `ONE_WAY` each way. `SSH_AUTH_SOCK` points at the private agent and
/// `OPSPILOT_CONFIG_DIR` at the temp dir, so accept-new records the host key
/// there and not in `~/.ssh/known_hosts`. Process-wide, and this binary holds
/// only this test.
fn fixture() -> Option<Fixture> {
    let mut sshd = Sshd::start("live")?;
    let Some(sock) = sshd.agent() else {
        eprintln!("SKIPPED: needs ssh-agent and ssh-add");
        return None;
    };
    std::env::set_var("SSH_AUTH_SOCK", &sock);
    std::env::set_var("OPSPILOT_CONFIG_DIR", &sshd.dir);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let proxy_port = delay_proxy(&rt, sshd.port, ONE_WAY);
    Some(Fixture { sshd, proxy_port, _rt: rt })
}

/* ---- Driving a session ------------------------------------------------------- */

/// The cursor's row up to the cursor. The predicted cursor wins in a
/// snapshot, so this is what the user sees left of the caret, typed spaces
/// included.
fn cursor_line(s: &Session) -> String {
    let snap = s.snapshot(0).unwrap().unwrap();
    snap.lines
        .iter()
        .find(|l| l.row == snap.cursor_row)
        .map(|l| l.text.chars().take(snap.cursor_col).collect())
        .unwrap_or_default()
}

fn screen_text(s: &Session) -> String {
    let snap = s.snapshot(0).unwrap().unwrap();
    snap.lines.iter().map(|l| l.text.trim_end()).collect::<Vec<_>>().join("\n")
}

/// Wait until no output has arrived for `quiet`.
fn settle(s: &Session, quiet: Duration) {
    let mut last = s.traffic().0;
    let mut since = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(20);
    while since.elapsed() < quiet {
        assert!(Instant::now() < deadline, "output never settled");
        std::thread::sleep(Duration::from_millis(10));
        let now = s.traffic().0;
        if now != last {
            last = now;
            since = Instant::now();
        }
    }
}

fn run(s: &Session, line: &str) {
    s.write(format!("{line}\r").as_bytes()).unwrap();
    settle(s, Duration::from_millis(600));
}

#[derive(Default)]
struct Timings {
    visible: Vec<Duration>,
    echo: Vec<Duration>,
}

fn pct(v: &mut [Duration], p: f64) -> Duration {
    v.sort();
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

/// Type `text` one key at a time, waiting for each echo before the next key so
/// the two latencies of every keystroke are measured cleanly.
fn type_spaced(s: &Session, text: &str, t: &mut Timings) {
    // Only this call's text: the line before it ends in a prompt whose
    // trailing space the trimmed snapshot text does not keep.
    let mut typed = String::new();
    for c in text.chars() {
        typed.push(c);
        let bytes_in = s.traffic().0;
        let t0 = Instant::now();
        s.write(c.to_string().as_bytes()).unwrap();

        let mut vis = None;
        let deadline = t0 + Duration::from_secs(5);
        loop {
            if vis.is_none() && cursor_line(s).ends_with(&typed) {
                vis = Some(t0.elapsed());
            }
            if s.traffic().0 > bytes_in {
                t.echo.push(t0.elapsed());
                break;
            }
            assert!(Instant::now() < deadline, "no echo for {c:?}");
            std::thread::sleep(Duration::from_micros(200));
        }
        // Not visible before the echo means it was not predicted: then it is
        // visible when the echo lands.
        let vis = vis.unwrap_or_else(|| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !cursor_line(s).ends_with(&typed) {
                assert!(
                    Instant::now() < deadline,
                    "typed {typed:?} never appeared; cursor line is {:?}",
                    cursor_line(s)
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            t0.elapsed()
        });
        t.visible.push(vis);
        // Let the echo finish arriving and reconcile before the next key.
        settle(s, Duration::from_millis(30));
    }
}

/// Type like a fast typist: a key every `gap`, not waiting for echoes, so up
/// to RTT/gap predictions are in flight at once.
fn type_burst(s: &Session, text: &str, gap: Duration) {
    for c in text.chars() {
        s.write(c.to_string().as_bytes()).unwrap();
        std::thread::sleep(gap);
    }
}

fn prediction(id: SessionId) -> opspilot_app::session::PredictionStat {
    registry()
        .stats()
        .into_iter()
        .find(|s| s.id == id)
        .and_then(|s| s.prediction)
        .expect("remote session has prediction stats")
}

fn report(name: &str, t: &mut Timings) {
    let (v50, v99) = (pct(&mut t.visible, 0.5), pct(&mut t.visible, 0.99));
    let (e50, e99) = (pct(&mut t.echo, 0.5), pct(&mut t.echo, 0.99));
    println!(
        "{name:<28} n={:<3} visible P50 {:>8.2?} P99 {:>8.2?} | echo P50 {:>8.2?} P99 {:>8.2?}",
        t.visible.len(),
        v50,
        v99,
        e50,
        e99
    );
}

#[test]
#[ignore = "starts sshd and ssh-agent; run with --ignored"]
fn predictive_echo_over_a_200ms_link_with_agent_auth() {
    let Some(fx) = fixture() else { return };
    let user = fx.sshd.user.clone();

    // 1. Agent auth, through the app's own connect path. Nothing but the agent
    //    is offered: no key path, no password.
    let started = Instant::now();
    let info = remote::connect(ConnectRequest {
        host: "127.0.0.1".into(),
        port: fx.proxy_port,
        user: user.clone(),
        identity: "live".into(),
        auth: AuthSpec {
            password: None,
            key_path: None,
            passphrase: None,
            password_ref: None,
            passphrase_ref: None,
            use_agent: None,
        },
        host_key_policy: Some("accept-new".into()),
    })
    .unwrap_or_else(|e| {
        let log = fx.sshd.log();
        panic!("agent auth through remote::connect failed: {e}\nsshd log:\n{log}")
    });
    println!("connect + agent auth over 200 ms RTT: {:?}", started.elapsed());
    assert!(fx.dir().join("known_hosts").exists(), "host key went somewhere other than the test dir");

    let transport = remote::get(&info.key).unwrap();
    let id = registry().open_ssh(24, 100, Arc::clone(&transport)).unwrap();
    let s = registry().get(id).unwrap();
    settle(&s, Duration::from_millis(1500));

    // 2. A clean readline shell: no rc files, a fixed prompt.
    run(&s, "exec bash --norc --noprofile");
    run(&s, "PS1='$ '");
    run(&s, "clear");
    let base = prediction(id);

    let phase = |name: &str, from: &opspilot_app::session::PredictionStat| {
        let now = prediction(id);
        println!(
            "  {name:<22} +{} confirmed, +{} missed, +{} timed out, +{} held",
            now.confirmed - from.confirmed,
            now.missed - from.missed,
            now.timed_out - from.timed_out,
            now.withheld - from.withheld
        );
        now
    };
    let mut bash = Timings::default();
    type_spaced(&s, "echo the quick brown fox", &mut bash);
    let p1 = phase("spaced typing", &base);
    // Backspace is predicted too.
    s.write(b"\x7f\x7f\x7f").unwrap();
    settle(&s, Duration::from_millis(400));
    let p2 = phase("3 x backspace", &p1);
    type_spaced(&s, "cat", &mut bash);
    run(&s, "");
    let p3 = phase("more typing + enter", &p2);
    assert!(screen_text(&s).contains("the quick brown cat"), "{}", screen_text(&s));
    report("bash, spaced keys", &mut bash);

    // 3. Fast typing: several predictions in flight at once.
    type_burst(&s, "echo jumps over the lazy dog and keeps typing", Duration::from_millis(45));
    settle(&s, Duration::from_millis(600));
    let p4 = phase("burst, 45 ms/key", &p3);
    run(&s, "");
    phase("enter", &p4);
    assert!(screen_text(&s).contains("jumps over the lazy dog and keeps typing"));
    let after_bash = prediction(id);
    let (c, m) = (after_bash.confirmed - base.confirmed, after_bash.missed - base.missed);
    println!(
        "bash hit rate: {c} confirmed, {m} missed, {} timed out, {} flushes -> {:.1}% (disabled: {})",
        after_bash.timed_out - base.timed_out,
        after_bash.flushes - base.flushes,
        100.0 * c as f64 / (c + m).max(1) as f64,
        after_bash.disabled
    );

    // 4. A password prompt: echo is off. Count the secret's characters that
    //    were ever on screen.
    run(&s, "clear");
    s.write(b"read -s -p 'pw: ' x; echo; echo got ${#x}\r").unwrap();
    wait_for("the password prompt", 5, || cursor_line(&s).ends_with("pw: "));
    settle(&s, Duration::from_millis(300));
    let secret = "hunter2zz";
    let mut shown = 0usize;
    for c in secret.chars() {
        s.write(c.to_string().as_bytes()).unwrap();
        let t0 = Instant::now();
        let mut seen = false;
        while t0.elapsed() < Duration::from_millis(150) {
            if cursor_line(&s).contains(c) {
                seen = true;
            }
            std::thread::sleep(Duration::from_micros(500));
        }
        shown += seen as usize;
    }
    s.write(b"\r").unwrap();
    settle(&s, Duration::from_millis(600));
    assert!(screen_text(&s).contains("got 9"), "{}", screen_text(&s));
    assert!(!screen_text(&s).contains("hunter2"), "the secret is still on screen");
    let pw = prediction(id);
    println!(
        "password prompt: {shown}/{} secret characters ever visible; withheld so far: {}",
        secret.len(),
        pw.withheld
    );
    // §5.5: never predict onto a prompt with echo off. Not briefly, not once.
    assert_eq!(shown, 0, "secret characters were drawn at a no-echo prompt");

    // 5. Back to a normal prompt: prediction resumes.
    run(&s, "clear");
    let mut again = Timings::default();
    type_spaced(&s, "echo back to normal", &mut again);
    run(&s, "");
    report("bash, after password", &mut again);

    // 6. zsh with no rc files, if installed: a different line editor.
    if which("zsh").is_some() {
        run(&s, "exec zsh -f");
        run(&s, "PS1='%% '");
        run(&s, "clear");
        let z0 = prediction(id);
        let mut zsh = Timings::default();
        type_spaced(&s, "echo zsh line editor", &mut zsh);
        run(&s, "");
        report("zsh -f, spaced keys", &mut zsh);
        let z = prediction(id);
        let (c, m) = (z.confirmed - z0.confirmed, z.missed - z0.missed);
        println!("zsh hit rate: {c} confirmed, {m} missed -> {:.1}%", 100.0 * c as f64 / (c + m).max(1) as f64);
    }

    let fin = prediction(id);
    println!(
        "session total: hit rate {:.1}% over {} samples, disabled: {}",
        100.0 * fin.hit_rate,
        fin.samples,
        fin.disabled
    );

    // The claim under test, stated as assertions.
    let v50 = pct(&mut bash.visible, 0.5);
    let e50 = pct(&mut bash.echo, 0.5);
    assert!(e50 >= Duration::from_millis(190), "the link is not 200 ms RTT: echo P50 {e50:?}");
    assert!(v50 < Duration::from_millis(8), "§4: keystroke → glyph < 8 ms over WAN; got {v50:?}");
    assert!(!fin.disabled, "prediction disabled itself on a plain shell");

    // 7. Informational: the user's own interactive zsh, rc files and all.
    //    Syntax highlighting and autosuggestion plugins redraw the line on
    //    every key, which is the hardest case for prediction. Reported, not
    //    asserted: it depends on the machine's dotfiles.
    if which("zsh").is_some() {
        run(&s, "exec zsh -i");
        settle(&s, Duration::from_millis(1500));
        run(&s, "clear");
        let r0 = prediction(id);
        let mut real = Timings::default();
        type_spaced(&s, "echo highlighted and suggested", &mut real);
        type_burst(&s, " plus a fast burst of typing", Duration::from_millis(45));
        settle(&s, Duration::from_millis(600));
        let r = prediction(id);
        s.write(b"\x03").unwrap(); // abandon the line
        let (c, m, t) = (r.confirmed - r0.confirmed, r.missed - r0.missed, r.timed_out - r0.timed_out);
        report("your zsh (rc files), spaced", &mut real);
        println!(
            "your zsh hit rate: {c} confirmed, {m} missed, {t} timed out, {} held -> {:.1}% (disabled: {})",
            r.withheld - r0.withheld,
            100.0 * c as f64 / (c + m + t).max(1) as f64,
            r.disabled
        );
    }

    registry().close(id).unwrap();
    remote::disconnect(&info.key).unwrap();
}

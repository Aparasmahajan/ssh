//! The transfer engine against real OpenSSH (`opspilot-test-sshd`).
//!
//! The server runs as the current user, so "remote" paths are paths on this
//! machine — which is what lets every test compare bytes on both sides.
//!
//! Correctness tests run in the normal suite and skip where `sshd` is missing.
//! The benchmark at the bottom is `#[ignore]`:
//!
//! ```text
//! cargo test -p opspilot-core-sftp --release --test sshd -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use opspilot_core_sftp::{run, Cancel, Error, Method, Options, Progress, Request, TarPolicy};
use opspilot_core_ssh::{AuthMethod, Endpoint, HostKeyPolicy, Transport};
use opspilot_test_sshd::{delay_proxy, Sshd};

struct Env {
    sshd: Sshd,
    rt: tokio::runtime::Runtime,
}

fn env(tag: &str) -> Option<Env> {
    let sshd = Sshd::start(tag)?;
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap();
    Some(Env { sshd, rt })
}

impl Env {
    fn connect(&self, port: u16) -> Arc<Transport> {
        let t = self
            .rt
            .block_on(Transport::connect(
                Endpoint {
                    host: "127.0.0.1".into(),
                    port,
                    user: self.sshd.user.clone(),
                    identity: "sftp-test".into(),
                },
                &[AuthMethod::PrivateKey { path: self.sshd.key.clone(), passphrase: None }],
                HostKeyPolicy::AcceptNew,
                Some(self.sshd.dir.join("known_hosts")),
            ))
            .unwrap_or_else(|e| panic!("connect: {e}\n{}", self.sshd.log()));
        Arc::new(t)
    }

    fn dir(&self, name: &str) -> PathBuf {
        let d = self.sshd.dir.join(name);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn go(&self, t: &Arc<Transport>, req: Request, opts: Options) -> (Result<opspilot_core_sftp::Summary, Error>, Arc<Progress>) {
        let p = Arc::new(Progress::default());
        let r = self.rt.block_on(run(Arc::clone(t), req, opts, Arc::clone(&p), Arc::new(Cancel::default())));
        (r, p)
    }
}

/// Deterministic bytes that do not compress, so no layer can cheat.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Every file and directory under `root`, with contents, sorted.
fn snapshot(root: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, Option<Vec<u8>>)>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            let ft = e.file_type().unwrap();
            if ft.is_dir() {
                out.push((r.clone(), None));
                walk(&e.path(), &r, out);
            } else if ft.is_file() {
                out.push((r, Some(std::fs::read(e.path()).unwrap())));
            }
        }
    }
    let mut v = Vec::new();
    walk(root, "", &mut v);
    v.sort();
    v
}

fn no_part_files(root: &Path) {
    for (name, _) in snapshot(root) {
        assert!(!name.contains(".opspilot-part"), "part file left behind: {name}");
    }
}

fn make_tree(root: &Path, files: usize, size: usize) {
    std::fs::create_dir_all(root.join("empty")).unwrap();
    for i in 0..files {
        let sub = root.join(format!("d{}", i % 7)).join(format!("e{}", i % 3));
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(format!("f{i}.bin")), noise(size + i, i as u64)).unwrap();
    }
    std::fs::write(root.join("it's a \"name\" with $(spaces).txt"), b"quoted").unwrap();
}

#[test]
fn a_file_round_trips_byte_for_byte_with_its_mode() {
    let Some(e) = env("rt") else { return };
    let t = e.connect(e.sshd.port);
    let (src, remote, back) = (e.dir("src"), e.dir("remote"), e.dir("back"));
    // Not a multiple of any request size: exercises the last partial chunk.
    let data = noise(3 * 1024 * 1024 + 4321, 1);
    std::fs::write(src.join("big.bin"), &data).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(src.join("big.bin"), std::fs::Permissions::from_mode(0o640)).unwrap();
    }

    let (r, p) = e.go(&t, Request::Upload { local: src.join("big.bin"), remote_dir: s(&remote) }, Options::default());
    let sum = r.unwrap();
    assert_eq!(sum.method, Method::Pipelined);
    assert_eq!(sum.bytes, data.len() as u64);
    assert_eq!(p.bytes_done.load(Ordering::Relaxed), data.len() as u64);
    assert_eq!(std::fs::read(remote.join("big.bin")).unwrap(), data);

    let (r, _) = e.go(&t, Request::Download { remote: s(&remote.join("big.bin")), local_dir: back.clone() }, Options::default());
    r.unwrap();
    assert_eq!(std::fs::read(back.join("big.bin")).unwrap(), data);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&remote.join("big.bin")), 0o640, "upload lost the mode");
        assert_eq!(mode(&back.join("big.bin")), 0o640, "download lost the mode");
    }
    no_part_files(&remote);
    no_part_files(&back);

    // Striped over 4 channels: every channel writes interleaved ranges of the
    // same part file. Forced here, below the size where it switches on, so
    // the chunk boundaries fall unevenly across stripes.
    let opts = Options { overwrite: true, stripes: Some(4), io_len: Some(64 * 1024), ..Default::default() };
    let (r, _) = e.go(&t, Request::Upload { local: src.join("big.bin"), remote_dir: s(&remote) }, opts);
    assert_eq!(r.unwrap().method, Method::Striped { channels: 4 });
    assert_eq!(std::fs::read(remote.join("big.bin")).unwrap(), data, "striped upload differs");
    no_part_files(&remote);
}

/// Both directory methods, both directions, on the same tree.
#[test]
fn a_directory_round_trips_by_parallel_sftp_and_by_tar() {
    let Some(e) = env("dir") else { return };
    let t = e.connect(e.sshd.port);
    let src = e.dir("src").join("tree");
    make_tree(&src, 40, 3000);
    #[cfg(unix)]
    std::os::unix::fs::symlink("empty", src.join("link")).unwrap();
    let want = snapshot(&src);

    for (label, tar, method) in [
        ("sftp", TarPolicy::Never, Method::Parallel { channels: 8 }),
        ("tar", TarPolicy::Always, Method::TarPipe),
    ] {
        let remote = e.dir(&format!("remote-{label}"));
        let back = e.dir(&format!("back-{label}"));
        let opts = Options { tar, ..Default::default() };

        let (r, p) = e.go(&t, Request::Upload { local: src.clone(), remote_dir: s(&remote) }, opts.clone());
        let sum = r.unwrap_or_else(|err| panic!("{label} upload: {err}"));
        assert_eq!(sum.method, method, "{label}");
        assert_eq!(snapshot(&remote.join("tree")), want, "{label}: upload differs");
        assert_eq!(p.files_done.load(Ordering::Relaxed), 41, "{label}");

        let (r, _) = e.go(&t, Request::Download { remote: s(&remote.join("tree")), local_dir: back.clone() }, opts);
        r.unwrap_or_else(|err| panic!("{label} download: {err}"));
        assert_eq!(snapshot(&back.join("tree")), want, "{label}: download differs");
        no_part_files(&remote);
        no_part_files(&back);

        // SFTP skips the symlink (counted); tar carries it.
        #[cfg(unix)]
        {
            let link = back.join("tree/link");
            match label {
                "tar" => assert_eq!(std::fs::read_link(&link).unwrap(), PathBuf::from("empty")),
                _ => {
                    assert!(std::fs::symlink_metadata(&link).is_err(), "sftp path created a link");
                    assert_eq!(sum.skipped, 1);
                }
            }
        }
    }
}

#[test]
fn an_existing_target_is_refused_unless_overwrite_is_asked_for() {
    let Some(e) = env("ow") else { return };
    let t = e.connect(e.sshd.port);
    let (src, remote) = (e.dir("src"), e.dir("remote"));
    std::fs::write(src.join("a.txt"), b"new").unwrap();
    std::fs::write(remote.join("a.txt"), b"old").unwrap();

    let up = || Request::Upload { local: src.join("a.txt"), remote_dir: s(&remote) };
    let (r, _) = e.go(&t, up(), Options::default());
    assert!(matches!(r, Err(Error::Exists(_))), "{r:?}");
    assert_eq!(std::fs::read(remote.join("a.txt")).unwrap(), b"old");

    let (r, _) = e.go(&t, up(), Options { overwrite: true, ..Default::default() });
    r.unwrap();
    assert_eq!(std::fs::read(remote.join("a.txt")).unwrap(), b"new");
    no_part_files(&remote);
}

/// Cancel mid-file over a slow link: an error, and nothing under the real name.
#[test]
fn a_cancelled_download_leaves_nothing_behind() {
    let Some(e) = env("cx") else { return };
    let port = delay_proxy(&e.rt, e.sshd.port, Duration::from_millis(20));
    let t = e.connect(port);
    let (remote, back) = (e.dir("remote"), e.dir("back"));
    std::fs::write(remote.join("huge.bin"), noise(64 * 1024 * 1024, 9)).unwrap();

    let p = Arc::new(Progress::default());
    let cancel = Arc::new(Cancel::default());
    let job = e.rt.spawn(run(
        Arc::clone(&t),
        Request::Download { remote: s(&remote.join("huge.bin")), local_dir: back.clone() },
        Options { window: Some((2, 2, 2)), io_len: Some(32 * 1024), ..Default::default() },
        Arc::clone(&p),
        Arc::clone(&cancel),
    ));
    let deadline = Instant::now() + Duration::from_secs(20);
    while p.bytes_done.load(Ordering::Relaxed) < 256 * 1024 {
        assert!(Instant::now() < deadline, "no progress");
        std::thread::sleep(Duration::from_millis(5));
    }
    let at = Instant::now();
    cancel.cancel();
    let r = e.rt.block_on(job).unwrap();
    assert_eq!(r.unwrap_err(), Error::Cancelled);
    assert!(at.elapsed() < Duration::from_secs(2), "cancel took {:?}", at.elapsed());
    assert!(!back.join("huge.bin").exists(), "a partial file appeared under the real name");
    no_part_files(&back);
}

/* ---- Benchmark ---------------------------------------------------------------- */

fn mbps(bytes: u64, d: Duration) -> f64 {
    bytes as f64 / d.as_secs_f64() / 1_000_000.0
}

/// §4's rows for SFTP, measured through a 40 ms RTT proxy (§16), against the
/// behaviours CLAUDE.md §2 attributes to WinSCP. No packet loss is simulated.
#[test]
#[ignore = "benchmark: starts sshd; run with --ignored --nocapture, ideally --release"]
fn bench_against_winscp_style_transfers_at_40ms_rtt() {
    let Some(e) = env("bench") else { return };
    let port = delay_proxy(&e.rt, e.sshd.port, Duration::from_millis(20));
    let t = e.connect(port);
    let remote = e.dir("remote");
    println!("\nlink: 40 ms RTT (20 ms each way), loopback bandwidth, no loss\n");

    // 1. One large file.
    let size = 32 * 1024 * 1024;
    std::fs::write(remote.join("big.bin"), noise(size, 3)).unwrap();
    let cases: [(&str, Options); 3] = [
        ("1 request in flight, 32 KiB (WinSCP)", Options { window: Some((1, 1, 1)), io_len: Some(32 * 1024), ..Default::default() }),
        ("1 request in flight, 255 KiB", Options { window: Some((1, 1, 1)), ..Default::default() }),
        ("pipelined AIMD (§6.1)", Options::default()),
    ];
    let mut base = None;
    for (label, opts) in cases {
        let back = e.dir(&format!("big-{}", label.len()));
        // WinSCP-style at 32 KiB is ~0.8 MB/s: time a smaller slice of it.
        let file = if opts.io_len.is_some() { "slice.bin" } else { "big.bin" };
        if file == "slice.bin" {
            std::fs::write(remote.join("slice.bin"), noise(4 * 1024 * 1024, 4)).unwrap();
        }
        let n = std::fs::metadata(remote.join(file)).unwrap().len();
        let at = Instant::now();
        let (r, p) = e.go(&t, Request::Download { remote: s(&remote.join(file)), local_dir: back }, opts);
        r.unwrap();
        let d = at.elapsed();
        let rate = mbps(n, d);
        let x = base.map(|b: f64| format!("{:>6.1}×", rate / b)).unwrap_or_else(|| "  base".into());
        base.get_or_insert(rate);
        println!(
            "  download {:>3} MiB  {label:<40} {:>8.2?}  {rate:>7.1} MB/s  {x}  (peak window {})",
            n >> 20,
            d,
            p.window.load(Ordering::Relaxed)
        );
    }

    // 1b. Steady state: a larger file (setup is ~6 RTTs, 240 ms, of any
    //     transfer), and the same with no added latency for the local ceiling.
    std::fs::write(remote.join("bigger.bin"), noise(128 * 1024 * 1024, 5)).unwrap();
    let direct = e.connect(e.sshd.port);
    for (label, conn, window) in [
        ("pipelined AIMD, 40 ms RTT", &t, None),
        ("fixed 128-request window, 40 ms RTT", &t, Some((128, 128, 128))),
        ("pipelined AIMD, no added latency", &direct, None),
    ] {
        let back = e.dir(&format!("bigger-{}", label.len()));
        let at = Instant::now();
        let (r, p) = e.go(conn, Request::Download { remote: s(&remote.join("bigger.bin")), local_dir: back.clone() }, Options { window, ..Default::default() });
        r.unwrap();
        let d = at.elapsed();
        println!(
            "  download 128 MiB  {label:<40} {:>8.2?}  {:>7.1} MB/s          (peak window {})",
            d,
            mbps(128 * 1024 * 1024, d),
            p.window.load(Ordering::Relaxed)
        );
        std::fs::remove_dir_all(back).unwrap();
    }
    let up_src = e.dir("upsrc");
    std::fs::copy(remote.join("bigger.bin"), up_src.join("bigger.bin")).unwrap();
    for (label, conn, stripes) in [
        ("1 channel, 40 ms RTT", &t, Some(1)),
        ("striped over 4 channels, 40 ms RTT", &t, Some(4)),
        ("striped over 8 channels (default), 40 ms", &t, None),
        ("striped over 8 channels, no latency", &direct, None),
    ] {
        let dst = e.dir(&format!("updst-{}", label.len()));
        let at = Instant::now();
        let (r, p) = e.go(conn, Request::Upload { local: up_src.join("bigger.bin"), remote_dir: s(&dst) }, Options { stripes, ..Default::default() });
        r.unwrap();
        let d = at.elapsed();
        println!(
            "  upload   128 MiB  {label:<40} {:>8.2?}  {:>7.1} MB/s          (peak window {})",
            d,
            mbps(128 * 1024 * 1024, d),
            p.window.load(Ordering::Relaxed)
        );
        std::fs::remove_dir_all(dst).unwrap();
    }

    // 2. Many small files.
    let src = e.dir("small").join("tree");
    make_tree(&src, 2000, 4096);
    let files = 2001u64;
    // Sequential per-file SFTP is ~3 RTTs a file: time 150 files and scale.
    let few = e.dir("few").join("tree");
    make_tree(&few, 150, 4096);
    let cases: [(&str, &PathBuf, u64, Options); 3] = [
        ("1 channel, 1 request (WinSCP)", &few, 151, Options { tar: TarPolicy::Never, channels: Some(1), window: Some((1, 1, 1)), ..Default::default() }),
        ("parallel SFTP, 8 channels", &src, files, Options { tar: TarPolicy::Never, ..Default::default() }),
        ("tar-pipe (§6.2)", &src, files, Options::default()),
    ];
    let mut base = None;
    for (i, (label, from, n, opts)) in cases.into_iter().enumerate() {
        let dst = e.dir(&format!("up-{i}"));
        let at = Instant::now();
        let (r, _) = e.go(&t, Request::Upload { local: from.clone(), remote_dir: s(&dst) }, opts);
        let sum = r.unwrap();
        let d = at.elapsed();
        let per_s = n as f64 / d.as_secs_f64();
        let x = base.map(|b: f64| format!("{:>6.1}×", per_s / b)).unwrap_or_else(|| "  base".into());
        base.get_or_insert(per_s);
        println!(
            "  upload {n:>5} × 4 KiB  {label:<32} {:>8.2?}  {per_s:>7.0} files/s  {x}  [{}]",
            d,
            sum.method.label()
        );
        println!(
            "      → 10,000 files would take {:.1} s",
            10_000.0 / per_s
        );
    }
}


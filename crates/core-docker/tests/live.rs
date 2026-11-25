//! Against a real Docker daemon. Ignored by default — run with
//! `cargo test -p opspilot-core-docker -- --ignored` while Docker is running.
//!
//! Touches only a container it creates itself (labelled `opspilot.test`, from
//! `alpine:3.20`), and removes it at the end even when an assertion fails.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bollard::models::ContainerCreateBody;
use bollard::query_parameters::{CreateContainerOptions, RemoveContainerOptions};
use opspilot_core_docker::{act, connect, endpoint, exec, logs, Action, Watch};
use tokio::sync::mpsc;

const IMAGE: &str = "alpine:3.20";

async fn wait_for(w: &Watch, what: &str, mut f: impl FnMut(&opspilot_core_docker::Snapshot) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(s) = w.snapshot(0) {
            if f(&s) {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for: {what}");
}

/// Removes the test container however the test exits.
struct Cleanup(opspilot_core_docker::Api, String);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let (api, name) = (self.0.clone(), self.1.clone());
        let _ = std::thread::spawn(move || {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let _ = api
                    .remove_container(&name, Some(RemoveContainerOptions { force: true, ..Default::default() }))
                    .await;
            })
        })
        .join();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a running Docker daemon"]
async fn live_container_lifecycle() {
    let ep = endpoint::resolve().expect("resolve endpoint");
    eprintln!("endpoint: {} ({})", ep.host, ep.source);
    let api = connect(&ep).expect("connect");

    let name = format!("opspilot-test-{}", std::process::id());
    let mut labels = HashMap::new();
    labels.insert("opspilot.test".to_string(), "1".to_string());
    api.create_container(
        Some(CreateContainerOptions { name: Some(name.clone()), ..Default::default() }),
        ContainerCreateBody {
            image: Some(IMAGE.into()),
            // Prints a line a second, so logs have something to follow.
            cmd: Some(vec!["sh".into(), "-c".into(), "while true; do echo tick $(date +%s); sleep 1; done".into()]),
            labels: Some(labels),
            ..Default::default()
        },
    )
    .await
    .expect("create test container");
    let _cleanup = Cleanup(api.clone(), name.clone());

    // 1. The watch sees it appear (created) — through the events stream.
    let w = Watch::start(api.clone(), &tokio::runtime::Handle::current());
    wait_for(&w, "connected + listed", |s| s.connected && s.containers.iter().any(|c| c.name == name)).await;

    // 2. Start it; the watch reports running, then stats arrive.
    act(&api, &name, Action::Start).await.expect("start");
    wait_for(&w, "running", |s| s.containers.iter().any(|c| c.name == name && c.state == "running")).await;
    wait_for(&w, "stats", |s| {
        s.containers.iter().any(|c| c.name == name && c.mem_bytes.is_some() && c.cpu_pct.is_some())
    })
    .await;

    // 3. Exec: alpine has no bash, so the fallback to sh must kick in.
    let (out_tx, mut out_rx) = mpsc::channel(64);
    let (in_tx, in_rx) = mpsc::channel(64);
    let (_rs_tx, rs_rx) = mpsc::channel(4);
    exec(&api, &name, 24, 80, out_tx, in_rx, rs_rx).await.expect("exec");
    in_tx.send(b"echo exec_$((40+2))\r".to_vec()).await.unwrap();
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !String::from_utf8_lossy(&seen).contains("exec_42") {
        if let Ok(Some(c)) = tokio::time::timeout(Duration::from_millis(500), out_rx.recv()).await {
            seen.extend(c);
        }
    }
    assert!(String::from_utf8_lossy(&seen).contains("exec_42"), "exec output: {:?}", String::from_utf8_lossy(&seen));

    // 3b. A large paste with echo on. One loop that both wrote stdin and read
    // stdout deadlocked here: it sat in write_all while the remote, blocked
    // writing its echo, stopped draining stdin. ~1 MB of 100-byte lines.
    let drain = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Ok(Some(c)) = tokio::time::timeout(Duration::from_secs(30), out_rx.recv()).await {
            seen.extend(c);
            if String::from_utf8_lossy(&seen[seen.len().saturating_sub(4096)..]).contains("paste_ok") {
                return true;
            }
        }
        false
    });
    in_tx.send(b"head -c 992000 > /dev/null; echo paste_ok\n".to_vec()).await.unwrap();
    let line = {
        let mut l = vec![b'a'; 99];
        l.push(b'\n');
        l
    };
    let chunk: Vec<u8> = line.iter().copied().cycle().take(16 * 1000).collect();
    for _ in 0..(1_000_000 / chunk.len()) {
        tokio::time::timeout(Duration::from_secs(20), in_tx.send(chunk.clone()))
            .await
            .expect("input stalled: exec pump deadlocked")
            .unwrap();
    }
    assert!(drain.await.unwrap(), "paste never completed through the exec");
    drop(in_tx);

    // 4. Logs: the tail arrives, then new lines keep coming.
    let (log_tx, mut log_rx) = mpsc::channel(64);
    logs(&api, &name, 5, log_tx).expect("logs");
    let first = tokio::time::timeout(Duration::from_secs(5), log_rx.recv()).await.expect("log line").unwrap();
    assert!(String::from_utf8_lossy(&first).contains("tick"), "{:?}", String::from_utf8_lossy(&first));
    drop(log_rx);

    // 5. Stop -> exited; remove -> gone.
    act(&api, &name, Action::Stop).await.expect("stop");
    wait_for(&w, "exited", |s| s.containers.iter().any(|c| c.name == name && c.state == "exited")).await;
    act(&api, &name, Action::Remove { force: false }).await.expect("remove");
    wait_for(&w, "removed", |s| !s.containers.iter().any(|c| c.name == name)).await;
}

#[tokio::test]
#[ignore = "needs a running Docker daemon"]
async fn an_invalid_id_never_reaches_the_daemon() {
    let api = connect(&endpoint::resolve().unwrap()).unwrap();
    let err = act(&api, "../../images/alpine", Action::Remove { force: true }).await.unwrap_err();
    assert!(err.contains("invalid container id"), "{err}");
}

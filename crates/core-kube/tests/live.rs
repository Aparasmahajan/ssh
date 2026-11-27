//! Against a real cluster. Ignored by default. With a kind cluster:
//!
//! ```text
//! kind create cluster --name opspilot-test --kubeconfig /tmp/kc
//! docker save --platform linux/amd64 alpine:3.20 -o /tmp/a.tar && kind load image-archive /tmp/a.tar --name opspilot-test
//! OPSPILOT_TEST_KUBECONFIG=/tmp/kc cargo test -p opspilot-core-kube --test live -- --ignored --nocapture
//! ```
//!
//! Works in its own namespace (`opspilot-test`) and deletes it at the end.

use std::time::{Duration, Instant};

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Namespace;
use kube::api::{Api, DeleteParams, PostParams};
use opspilot_core_kube::{self as k, Informer, Kind};
use tokio::sync::mpsc;

const NS: &str = "opspilot-test";

async fn until<T>(what: &str, secs: u64, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn read_until(rx: &mut mpsc::Receiver<Vec<u8>>, needle: &str, secs: u64) -> String {
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if let Ok(Some(c)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            seen.extend(c);
            if String::from_utf8_lossy(&seen).contains(needle) {
                break;
            }
        }
    }
    String::from_utf8_lossy(&seen).into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a Kubernetes cluster"]
async fn live_cluster() {
    // Single test, so setting the process-global env var here is safe.
    if let Ok(kc) = std::env::var("OPSPILOT_TEST_KUBECONFIG") {
        std::env::set_var("KUBECONFIG", kc);
    }
    let ctxs = k::contexts().expect("kubeconfig");
    let ctx = ctxs.current.clone().expect("a current context");
    eprintln!("context {ctx} from {}", ctxs.source);
    let client = k::client(&ctx).await.expect("client");

    // Setup: namespace + a deployment printing a line a second.
    let nss: Api<Namespace> = Api::all(client.clone());
    let _ = nss.delete(NS, &DeleteParams::default()).await;
    until("old namespace gone", 90, || None::<()>.or(Some(()))).await;
    let ns: Namespace = serde_json::from_value(serde_json::json!({"metadata": {"name": NS}})).unwrap();
    for _ in 0..60 {
        if nss.create(&PostParams::default(), &ns).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await; // still terminating
    }
    let dep: Deployment = serde_json::from_value(serde_json::json!({
        "metadata": {"name": "ticker"},
        "spec": {
            "replicas": 1,
            "selector": {"matchLabels": {"app": "ticker"}},
            "template": {
                "metadata": {"labels": {"app": "ticker"}},
                "spec": {"containers": [{
                    "name": "main", "image": "alpine:3.20", "imagePullPolicy": "IfNotPresent",
                    "command": ["sh", "-c", "while true; do echo tick $(date +%s); sleep 1; done"]
                }]}
            }
        }
    }))
    .unwrap();
    Api::<Deployment>::namespaced(client.clone(), NS)
        .create(&PostParams::default(), &dep)
        .await
        .expect("create deployment");

    let rt = tokio::runtime::Handle::current();
    let pods = Informer::start(client.clone(), Kind::Pods, NS, &rt);
    let deps = Informer::start(client.clone(), Kind::Deployments, NS, &rt);

    // 1. The informer sees the pod reach Running, without polling.
    let pod = until("pod running", 120, || {
        pods.snapshot(0)?.rows.into_iter().find(|r| r.cells[1] == "Running" && r.tone == "live")
    })
    .await;
    let snap = pods.snapshot(0).unwrap();
    eprintln!("pods informer mode: {} · {} row(s)", snap.mode, snap.rows.len());
    assert_eq!(pod.containers, ["main"]);

    // 2. Logs follow.
    let (tx, mut rx) = mpsc::channel(64);
    k::logs(client.clone(), NS, &pod.name, None, 5, tx).await.expect("logs");
    let got = read_until(&mut rx, "tick", 15).await;
    assert!(got.contains("tick"), "logs: {got:?}");
    drop(rx);

    // 3. Exec: alpine has no bash; the sh fallback runs.
    let (out_tx, mut out_rx) = mpsc::channel(64);
    let (in_tx, in_rx) = mpsc::channel(64);
    let (rs_tx, rs_rx) = mpsc::channel(4);
    k::exec(client.clone(), NS, &pod.name, Some("main".into()), 24, 80, out_tx, in_rx, rs_rx)
        .await
        .expect("exec");
    in_tx.send(b"echo exec_$((40+2)) $TERM\n".to_vec()).await.unwrap();
    let got = read_until(&mut out_rx, "exec_42 xterm-256color", 15).await;
    assert!(got.contains("exec_42 xterm-256color"), "exec: {got:?}");
    rs_tx.send((30, 100)).await.unwrap();
    drop(in_tx);

    // 4. Scale to 2; the deployment informer reports 2/2.
    k::scale(client.clone(), NS, "ticker", 2).await.expect("scale");
    until("2/2 ready", 120, || deps.snapshot(0)?.rows.into_iter().find(|r| r.cells[0] == "2/2")).await;

    // 5. Rollout restart and YAML.
    k::rollout_restart(client.clone(), NS, "ticker").await.expect("restart");
    let y = k::yaml(client.clone(), Kind::Deployments, NS, "ticker").await.expect("yaml");
    assert!(y.contains("kind: Deployment"), "{y}");
    assert!(y.contains("restartedAt"), "restart annotation missing");
    assert!(!y.contains("managedFields"));

    // 6. Delete a pod: the informer drops that UID.
    let victim = pods.snapshot(0).unwrap().rows[0].clone();
    k::delete_pod(client.clone(), NS, &victim.name).await.expect("delete pod");
    until("deleted pod gone from store", 90, || {
        (!pods.snapshot(0)?.rows.iter().any(|r| r.uid == victim.uid)).then_some(())
    })
    .await;

    // 7. Bad names never reach the API server.
    assert!(k::delete_pod(client.clone(), NS, "../etc").await.unwrap_err().contains("invalid"));

    let _ = nss.delete(NS, &DeleteParams::default()).await;
}

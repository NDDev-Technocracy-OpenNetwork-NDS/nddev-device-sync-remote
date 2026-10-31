use crate::adapter::{OpenView, RemoteScreen, ScreenFrame, ScreenView};
use ed25519_dalek::SigningKey;
use nddev_device_sync_domain::screen::{ModuleError, ScreenError, ViewLease};
use rds_agent::{Agent, AgentPolicy};
use rds_client::local::{Client, Prepared, Server};
use rds_core::{
    ServiceKind,
    grant::{Grant, GrantConstraints},
    local::{Command, Reply, SessionId},
};
use rds_net::{Backend, Endpoint, EndpointConfig, Ticket, Transports, bind_endpoint};
use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use x11rb::{
    connection::Connection,
    protocol::xproto::{ChangeWindowAttributesAux, ConnectionExt, CreateWindowAux, WindowClass},
    rust_connection::RustConnection,
};
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

struct Marker {
    connection: RustConnection,
    window: u32,
}
impl Marker {
    fn new() -> Self {
        assert_eq!(
            std::env::var("NDS_OWNED_XVFB").as_deref(),
            Ok("1"),
            "requires the owned Xvfb runner"
        );
        let auth = std::env::var_os("XAUTHORITY").expect("owned X authority required");
        assert!(std::path::Path::new(&auth).is_file());
        let (connection, _) = RustConnection::connect(None).unwrap();
        let root = connection.setup().roots[0].root;
        let window = connection.generate_id().unwrap();
        connection
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                root,
                0,
                0,
                320,
                240,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new().background_pixel(0xff0000u32),
            )
            .unwrap()
            .check()
            .unwrap();
        connection.map_window(window).unwrap().check().unwrap();
        connection.flush().unwrap();
        Self { connection, window }
    }
    fn color(&self, pixel: u32) {
        self.connection
            .change_window_attributes(
                self.window,
                &ChangeWindowAttributesAux::new().background_pixel(pixel),
            )
            .unwrap()
            .check()
            .unwrap();
        self.connection
            .clear_area(false, self.window, 0, 0, 0, 0)
            .unwrap()
            .check()
            .unwrap();
        self.connection.flush().unwrap();
    }
}
impl Drop for Marker {
    fn drop(&mut self) {
        let _ = self.connection.destroy_window(self.window);
        let _ = self.connection.flush();
    }
}
struct Fixture {
    root: tempfile::TempDir,
    local: Endpoint,
    remote: Endpoint,
    manager: Server,
    agent: Arc<Agent>,
    task: JoinHandle<()>,
    issuer: SigningKey,
    policy: AgentPolicy,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new(backend: Backend) -> Self {
        Self::with_desktop(backend, true).await
    }
    async fn with_desktop(backend: Backend, desktop: bool) -> Self {
        let cfg = || EndpointConfig {
            backend,
            discovery: false,
            transports: Transports::DirectOnly,
            bind_addrs: vec!["127.0.0.1:0".parse().unwrap()],
            ..Default::default()
        };
        let local = bind_endpoint(cfg()).await.unwrap();
        let remote = bind_endpoint(cfg()).await.unwrap();
        let issuer = SigningKey::from_bytes(&rand::random());
        let mut policy = AgentPolicy::ssh_only(("127.0.0.1".into(), 9));
        policy.allow.insert(local.id());
        policy.issuers.insert(issuer.verifying_key().to_bytes());
        policy.services = Some(if desktop {
            BTreeSet::from([ServiceKind::Desktop])
        } else {
            BTreeSet::new()
        });
        policy.use_local_revocations();
        let agent = Arc::new(Agent::new(remote.clone(), policy.clone()));
        let serving = agent.clone();
        let task = tokio::spawn(async move {
            let _ = serving.run().await;
        });
        let root = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let prepared = Prepared::bind(root.path().join("control")).await.unwrap();
        let manager = Server::start(Some(prepared), local.clone(), None);
        Self {
            root,
            local,
            remote,
            manager,
            agent,
            task,
            issuer,
            policy,
        }
    }
    fn adapter(&self) -> RemoteScreen {
        RemoteScreen::new(self.root.path().join("control")).unwrap()
    }
    fn client(&self) -> Client {
        Client::new(self.root.path().join("control"))
    }
    async fn info_session(&self) -> SessionId {
        let grant = Grant::issue(
            &self.issuer,
            *self.local.id().as_bytes(),
            *self.remote.id().as_bytes(),
            rand::random(),
            vec![ServiceKind::Info],
            Duration::from_secs(30),
            GrantConstraints::default(),
        );
        match self
            .client()
            .request(Command::Connect {
                target: Ticket::of(&self.remote).to_string(),
                grant: Some(Box::new(grant)),
            })
            .await
            .unwrap()
        {
            Reply::Connected(session) => session,
            _ => panic!("native owner did not connect its Info-scoped session"),
        }
    }
    fn request(&self, lifetime_ms: u64, scopes: Vec<ServiceKind>) -> OpenView {
        let grant = Grant::issue(
            &self.issuer,
            *self.local.id().as_bytes(),
            *self.remote.id().as_bytes(),
            rand::random(),
            scopes,
            Duration::from_secs(30),
            GrantConstraints {
                displays: Some(vec![0]),
                max_bps: Some(8_000_000),
                ..Default::default()
            },
        );
        OpenView {
            target: Ticket::of(&self.remote).to_string(),
            peer: *self.remote.id().as_bytes(),
            issuer: self.issuer.verifying_key().to_bytes(),
            grant,
            lease: ViewLease::new(0, 30, 480, now() + lifetime_ms, now()).unwrap(),
        }
    }
    async fn close(&mut self) {
        self.manager.close().await.unwrap();
        self.local.close().await;
        self.remote.close().await;
        self.task.abort();
    }
}
async fn color(view: &mut ScreenView, red: bool) -> ScreenFrame {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let frame = view.next_frame().await.unwrap();
            let (w, h, stride) = frame.dimensions();
            assert_eq!((w, h), (640, 480));
            let bytes = frame.pixels().unwrap();
            let p = &bytes[(100 * stride + 100 * 4) as usize..][..4];
            if (red && p[2] > 180 && p[0] < 70) || (!red && p[0] > 180 && p[2] < 70) {
                return frame;
            }
        }
    })
    .await
    .expect("actual X11 marker pixels never reached decoder")
}
async fn ended(view: &ScreenView) -> ScreenError {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if let Some(error) = view.status() {
                return error;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("view lifetime did not terminate")
}

#[tokio::test]
async fn cancelled_frames_are_inaccessible_before_the_worker_runs_again() {
    let _marker = Marker::new();
    let mut fixture = Fixture::new(Backend::Iroh).await;
    let cancellation = CancellationToken::new();
    let mut view = fixture
        .adapter()
        .open(
            fixture.request(10_000, vec![ServiceKind::DesktopView]),
            cancellation.clone(),
        )
        .await
        .unwrap();
    let frame = color(&mut view, true).await;
    cancellation.cancel();
    // This current-thread runtime cannot schedule the supervisor between these
    // synchronous calls. Access must honor the cancellation token directly.
    assert!(frame.pixels().is_none());
    assert!(matches!(
        view.next_frame().await,
        Err(ScreenError::Module(ModuleError::Cancelled))
    ));
    view.close().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_x11_pixels_cross_managed_quic_and_cancel_under_retention_pressure() {
    let marker = Marker::new();
    for backend in [Backend::Iroh, Backend::Noq] {
        marker.color(0xff0000);
        let mut fixture = Fixture::new(backend).await;
        let adapter = fixture.adapter();
        let token = CancellationToken::new();
        let request = fixture.request(20_000, vec![ServiceKind::DesktopView]);
        let mut view = adapter.open(request, token.clone()).await.unwrap();
        let first = color(&mut view, true).await;
        let before = fixture.client().snapshot().await.unwrap();
        let another = fixture.request(10_000, vec![ServiceKind::DesktopView]);
        assert!(matches!(
            adapter.open(another, CancellationToken::new()).await,
            Err(ScreenError::Module(ModuleError::Busy))
        ));
        marker.color(0x0000ff);
        let second = color(&mut view, false).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), view.next_frame())
                .await
                .is_err(),
            "retained frame limit was bypassed"
        );
        token.cancel();
        assert_eq!(
            ended(&view).await,
            ScreenError::Module(ModuleError::Cancelled)
        );
        assert!(first.pixels().is_none() && second.pixels().is_none());
        assert!(matches!(
            view.next_frame().await,
            Err(ScreenError::Module(ModuleError::Cancelled))
        ));
        view.close().await;
        let after = fixture.client().snapshot().await.unwrap();
        assert_eq!(before.generation, after.generation);
        assert_eq!(before.selected, after.selected);
        assert_eq!(
            after.sessions.len(),
            1,
            "closing one view destroyed the native manager connection"
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            while fixture.agent.active_streams() != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grant_scope_rejection_and_native_revocation_end_real_video() {
    let _marker = Marker::new();
    let mut fixture = Fixture::new(Backend::Iroh).await;
    let adapter = fixture.adapter();
    for scopes in [
        vec![ServiceKind::Desktop],
        vec![ServiceKind::DesktopView, ServiceKind::DesktopControl],
        vec![ServiceKind::DesktopControl],
    ] {
        assert!(matches!(
            adapter
                .open(fixture.request(10_000, scopes), CancellationToken::new())
                .await,
            Err(ScreenError::Denied)
        ));
    }
    assert!(
        fixture
            .client()
            .snapshot()
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    let request = fixture.request(20_000, vec![ServiceKind::DesktopView]);
    let grant_id = request.grant.id().unwrap();
    let mut view = adapter
        .open(request, CancellationToken::new())
        .await
        .unwrap();
    let first = color(&mut view, true).await;
    fixture.policy.revoke(grant_id);
    assert!(matches!(
        ended(&view).await,
        ScreenError::Closed | ScreenError::Module(ModuleError::Unavailable)
    ));
    assert!(first.pixels().is_none());
    view.close().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expiry_closes_without_a_polling_consumer() {
    let _marker = Marker::new();
    let mut fixture = Fixture::new(Backend::Iroh).await;
    let adapter = fixture.adapter();
    let mut view = adapter
        .open(
            fixture.request(2500, vec![ServiceKind::DesktopView]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let first = color(&mut view, true).await;
    assert_eq!(ended(&view).await, ScreenError::Expired);
    assert!(first.pixels().is_none());
    view.close().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_owner_rejects_untrusted_issuer_and_wrong_audience() {
    let _marker = Marker::new();
    let mut fixture = Fixture::new(Backend::Iroh).await;
    let adapter = fixture.adapter();
    let mut wrong = fixture.request(10_000, vec![ServiceKind::DesktopView]);
    wrong.peer = rand::random();
    assert!(matches!(
        adapter.open(wrong, CancellationToken::new()).await,
        Err(ScreenError::Denied)
    ));
    let other = SigningKey::from_bytes(&rand::random());
    let mut request = fixture.request(10_000, vec![ServiceKind::DesktopView]);
    request.issuer = other.verifying_key().to_bytes();
    request.grant = Grant::issue(
        &other,
        *fixture.local.id().as_bytes(),
        *fixture.remote.id().as_bytes(),
        rand::random(),
        vec![ServiceKind::DesktopView],
        Duration::from_secs(30),
        GrantConstraints::default(),
    );
    assert!(matches!(
        adapter.open(request, CancellationToken::new()).await,
        Err(ScreenError::Denied)
    ));
    assert!(
        fixture
            .client()
            .snapshot()
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_pinned_legacy_manager_refuses_the_v5_client() {
    use std::process::{Command, Stdio};
    struct Process(std::process::Child);
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let Some(binary) = std::env::var_os("NDS_SCREEN_LEGACY_AGENT") else {
        panic!("verified legacy binary required");
    };
    let root = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let control = root.path().join("control");
    let mut process = Process(
        Command::new(binary)
            .args([
                "--backend",
                "noq",
                "--no-relay",
                "--bind-address",
                "127.0.0.1:0",
                "--max-connections",
                "4",
                "--max-streams",
                "4",
            ])
            .arg("--key-file")
            .arg(root.path().join("endpoint.key"))
            .arg("--control-dir")
            .arg(&control)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !control.join("control.sock").exists() {
            assert!(process.0.try_wait().unwrap().is_none());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        Client::new(&control).snapshot().await,
        Err(rds_client::local::Error::Rejected(
            rds_core::local::ErrorCode::Version
        ))
    ));
    assert!(matches!(
        RemoteScreen::new(control)
            .unwrap()
            .capabilities(SessionId([0; 16]), [0; 32], CancellationToken::new())
            .await,
        Err(ModuleError::InvalidContract)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn discovery_reports_actual_inventory_and_disabled_service_without_claiming_watching() {
    use nddev_device_sync_domain::{
        modules::{ModuleObservation, ModuleOutcome, query_outcome},
        screen::{ScreenPhase, ScreenSupport, ViewProgress},
        state::Reachability,
    };
    let _marker = Marker::new();
    for enabled in [true, false] {
        let mut fixture = Fixture::with_desktop(Backend::Iroh, enabled).await;
        let session = fixture.info_session().await;
        let adapter = fixture.adapter();
        let before = fixture.client().snapshot().await.unwrap();
        let result = adapter
            .capabilities(
                session,
                *fixture.remote.id().as_bytes(),
                CancellationToken::new(),
            )
            .await;
        assert_eq!(query_outcome(&result), ModuleOutcome::Completed);
        let mut observation = ModuleObservation {
            id: "remote".into(),
            version: "0.0.1-alpha.1".into(),
            configured: true,
            reachability: Reachability::NotChecked,
            observed_at_ms: None,
            error: None,
        };
        observation.observe(&result, Some(now()));
        assert!(observation.reachability == Reachability::Reachable);
        let caps = result.unwrap();
        if enabled {
            assert!(caps.support == ScreenSupport::Supported);
            assert!(
                caps.displays
                    .iter()
                    .any(|d| d.id == 0 && d.width == 640 && d.height == 480)
            );
        } else {
            assert!(caps.support == ScreenSupport::ServiceDisabled);
            assert!(caps.displays.is_empty());
        }
        let progress = ViewProgress::new(1, 0).unwrap();
        assert_eq!(progress.phase(0), ScreenPhase::AwaitingFrame);
        assert_eq!(
            before.generation,
            fixture.client().snapshot().await.unwrap().generation
        );
        assert!(matches!(
            adapter
                .capabilities(session, [0; 32], CancellationToken::new())
                .await,
            Err(ModuleError::InvalidInput)
        ));
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_a_view_releases_only_its_owned_streams_and_admission() {
    use nddev_device_sync_domain::screen::{ScreenPhase, ViewProgress};
    let _marker = Marker::new();
    let mut fixture = Fixture::new(Backend::Iroh).await;
    let adapter = fixture.adapter();
    let request = fixture.request(20_000, vec![ServiceKind::DesktopView]);
    let retry = OpenView {
        target: request.target.clone(),
        peer: request.peer,
        issuer: request.issuer,
        grant: request.grant.clone(),
        lease: request.lease,
    };
    let mut view = adapter
        .open(request, CancellationToken::new())
        .await
        .unwrap();
    let frame = color(&mut view, true).await;
    let mut progress = ViewProgress::new(1, 0).unwrap();
    progress.received(1, frame.sequence(), 1).unwrap();
    assert_eq!(progress.phase(1), ScreenPhase::FrameAvailable);
    let before = fixture.client().snapshot().await.unwrap();
    drop(view);
    assert!(frame.pixels().is_none());
    tokio::time::timeout(Duration::from_secs(3), async {
        while fixture.agent.active_streams() != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let after = fixture.client().snapshot().await.unwrap();
    assert_eq!(before.selected, after.selected);
    assert_eq!(before.generation, after.generation);
    let mut restarted = adapter.open(retry, CancellationToken::new()).await.unwrap();
    let _new_frame = color(&mut restarted, true).await;
    restarted.close().await;
    fixture.close().await;
}

struct PausedSource(u32);
impl PausedSource {
    fn new() -> Self {
        let pid: u32 = std::env::var("NDS_OWNED_XVFB_PID")
            .expect("owned X server PID required")
            .parse()
            .unwrap();
        let command = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
        let arguments: Vec<_> = command.split(|byte| *byte == 0).collect();
        assert!(
            std::path::Path::new(std::str::from_utf8(arguments[0]).unwrap())
                .file_name()
                .unwrap()
                == "Xvfb"
        );
        assert!(
            arguments
                .iter()
                .any(|a| *a == std::env::var("XAUTHORITY").unwrap().as_bytes())
        );
        assert!(
            std::process::Command::new("kill")
                .args(["-STOP", "--", &pid.to_string()])
                .status()
                .unwrap()
                .success()
        );
        Self(pid)
    }
}
impl Drop for PausedSource {
    fn drop(&mut self) {
        let _ = std::process::Command::new("kill")
            .args(["-CONT", "--", &self.0.to_string()])
            .status();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_real_stalled_x_server_invalidates_pixels_and_terminates_the_view() {
    let _marker = Marker::new();
    let mut fixture = Fixture::new(Backend::Iroh).await;
    let mut view = fixture
        .adapter()
        .open(
            fixture.request(20_000, vec![ServiceKind::DesktopView]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let frame = color(&mut view, true).await;
    let paused = PausedSource::new();
    assert_eq!(
        ended(&view).await,
        ScreenError::Module(ModuleError::Timeout)
    );
    assert!(frame.pixels().is_none());
    drop(paused);
    view.close().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unchanging_x11_image_remains_live_across_multiple_stale_deadlines() {
    let _marker = Marker::new();
    let mut fixture = Fixture::new(Backend::Iroh).await;
    let mut view = fixture
        .adapter()
        .open(
            fixture.request(20_000, vec![ServiceKind::DesktopView]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let session = fixture.client().snapshot().await.unwrap().sessions[0].id;
    // A second actual native channel independently proves the existing control
    // heartbeat semantics. It adds no protocol and owns only its own stream.
    let mut observer = fixture
        .client()
        .desktop_profile(
            Some(session),
            rds_core::DesktopHello {
                display: 0,
                max_fps: 30,
                codec: rds_core::Codec::H264,
                input_acks: false,
            },
            Some(480),
        )
        .await
        .unwrap();
    let probe = observer.heartbeat().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(rds_client::local::ManagedMessage::Event(
                rds_core::DesktopEvent::Heartbeat { seq, .. },
            )) = observer.recv().await.unwrap()
                && seq == probe
            {
                break;
            }
        }
    })
    .await
    .expect("native heartbeat was not echoed on the unchanged desktop");
    observer.finish().await.unwrap();
    let started = tokio::time::Instant::now();
    let mut count = 0;
    let mut previous = None;
    while started.elapsed() < Duration::from_secs(8) {
        let frame = color(&mut view, true).await;
        if let Some(seq) = previous {
            assert!(frame.sequence() > seq);
        }
        previous = Some(frame.sequence());
        count += 1;
        assert!(
            view.status().is_none(),
            "unchanged pixels were treated as a stalled source"
        );
    }
    assert!(
        count >= 4,
        "the native source did not refresh the unchanged image"
    );
    view.close().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the explicit owned 5120x1440 X server lane"]
async fn a_real_ultrawide_source_fits_the_existing_frame_budget() {
    assert_eq!(std::env::var("NDS_ULTRAWIDE_FIXTURE").as_deref(), Ok("1"));
    let _marker = Marker::new();
    let mut fixture = Fixture::new(Backend::Iroh).await;
    let mut request = fixture.request(20_000, vec![ServiceKind::DesktopView]);
    request.lease = ViewLease::new(0, 30, 1080, now() + 20_000, now()).unwrap();
    let mut view = fixture
        .adapter()
        .open(request, CancellationToken::new())
        .await
        .unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(8), view.next_frame())
        .await
        .unwrap()
        .unwrap();
    let (width, height, stride) = frame.dimensions();
    assert_eq!((width, height), (1920, 540));
    let pixel = &frame.pixels().unwrap()[(30 * stride + 40 * 4) as usize..][..4];
    assert!(pixel[2] > 180 && pixel[0] < 70);
    view.close().await;
    fixture.close().await;
}

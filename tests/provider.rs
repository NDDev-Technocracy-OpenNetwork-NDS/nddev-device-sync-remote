use nddev_device_sync_adapter_io::{CancellationToken, NativeIo, ProcessRequest};
use nddev_device_sync_remote::{
    ReadRemote, Remote,
    view::{RemoteSnapshot, SessionState},
};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct OwnedAgent(Child);
impl Drop for OwnedAgent {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn(agent: &Path, root: &Path, name: &str, allow: Option<&str>) -> (OwnedAgent, PathBuf) {
    let control = root.join(name);
    let mut command = Command::new(agent);
    command
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
        .arg(root.join(format!("{name}.key")))
        .arg("--control-dir")
        .arg(&control);
    if let Some(peer) = allow {
        command.arg("--allow").arg(peer);
    }
    (
        OwnedAgent(
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        ),
        control,
    )
}

async fn ready(agent: &mut OwnedAgent, provider: &Remote) -> RemoteSnapshot {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(snapshot) = provider.sessions(CancellationToken::new()).await {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "native manager startup timed out"
        );
        assert!(
            agent.0.try_wait().unwrap().is_none(),
            "native manager exited"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn native(cli: &Path, control: &Path, args: &[&str]) -> String {
    let mut arguments = vec!["--control-dir".into(), control.as_os_str().into()];
    arguments.extend(args.iter().map(Into::into));
    let request = ProcessRequest {
        executable: cli.into(),
        arguments,
        directory: None,
        timeout: Duration::from_secs(8),
        max_output_bytes: 8192,
    };
    let output = NativeIo::new(1)
        .unwrap()
        .process_handle()
        .process(request, CancellationToken::new())
        .await
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[tokio::test]
#[ignore = "requires verified NDS_TEST_RDS and NDS_TEST_RDS_AGENT native executables"]
async fn reads_actual_loopback_connection_and_preserves_native_selection() {
    let cli: PathBuf = std::env::var_os("NDS_TEST_RDS")
        .expect("native RDS CLI required")
        .into();
    let agent: PathBuf = std::env::var_os("NDS_TEST_RDS_AGENT")
        .expect("native RDS agent required")
        .into();
    assert!(cli.is_absolute() && agent.is_absolute());
    let fixture = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(fixture.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let io = NativeIo::new(2).unwrap();
    let (mut first_agent, first_control) = spawn(&agent, fixture.path(), "first", None);
    let first_provider = Remote::new(
        io.process_handle(),
        cli.clone(),
        Some(first_control.clone()),
    )
    .unwrap();
    let first = ready(&mut first_agent, &first_provider).await;
    assert!(first.sessions.is_empty());
    let (mut second_agent, second_control) =
        spawn(&agent, fixture.path(), "second", Some(&first.endpoint));
    let second_provider = Remote::new(
        io.process_handle(),
        cli.clone(),
        Some(second_control.clone()),
    )
    .unwrap();
    let second = ready(&mut second_agent, &second_provider).await;
    // The real native owner creates the connection and authorizes only this
    // generated peer. There is no public relay, SSH service or real user key.
    let ticket = native(&cli, &second_control, &["ticket"]).await;
    let session = native(&cli, &first_control, &["session", "connect", &ticket]).await;
    let snapshot = first_provider
        .sessions(CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(snapshot.instance, first.instance);
    assert_eq!(snapshot.sessions.len(), 1);
    assert_eq!(snapshot.sessions[0].id, session);
    assert_eq!(snapshot.sessions[0].peer, second.endpoint);
    assert!(matches!(
        snapshot.sessions[0].status,
        SessionState::Connected
    ));
    assert_eq!(snapshot.selected.as_deref(), Some(session.as_str()));
    let again = first_provider
        .sessions(CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(snapshot.generation, again.generation);
    assert_eq!(snapshot.selected, again.selected);
}

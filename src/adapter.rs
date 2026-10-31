use crate::{Error, IoError, ReadRemote, view::*};
use nddev_device_sync_adapter_io::{CancellationToken, ProcessIo, ProcessRequest};

use std::{path::PathBuf, time::Duration};

#[derive(Clone)]
pub struct Remote {
    io: ProcessIo,
    executable: PathBuf,
    control_directory: Option<PathBuf>,
}

impl Remote {
    pub fn new(
        io: ProcessIo,
        executable: PathBuf,
        control_directory: Option<PathBuf>,
    ) -> Result<Self, Error> {
        if !executable.is_absolute()
            || control_directory
                .as_ref()
                .is_some_and(|path| !path.is_absolute())
        {
            return Err(IoError::InvalidConfiguration.into());
        }
        Ok(Self {
            io,
            executable,
            control_directory,
        })
    }
}

impl ReadRemote for Remote {
    async fn sessions(&self, cancellation: CancellationToken) -> Result<RemoteSnapshot, Error> {
        let mut arguments = vec!["session".into(), "list".into(), "--json".into()];
        if let Some(directory) = &self.control_directory {
            arguments.extend(["--control-dir".into(), directory.as_os_str().into()]);
        }
        let request = ProcessRequest {
            executable: self.executable.clone(),
            arguments,
            directory: None,
            timeout: Duration::from_secs(5),
            max_output_bytes: 65_536,
        };
        let (snapshot, _): (RemoteSnapshot, _) = self.io.json(request, &[0], cancellation).await?;
        if !consistent(&snapshot) {
            return Err(Error::Protocol);
        }
        Ok(snapshot)
    }
}

fn hex(value: &str, bytes: usize) -> bool {
    value.len() == bytes && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn consistent(snapshot: &RemoteSnapshot) -> bool {
    if !hex(&snapshot.instance, 32)
        || !hex(&snapshot.endpoint, 64)
        || snapshot.sessions.len() > 32
        || snapshot
            .selected
            .as_ref()
            .is_some_and(|id| !snapshot.sessions.iter().any(|session| &session.id == id))
        || snapshot
            .sessions
            .iter()
            .any(|session| !hex(&session.id, 32) || !hex(&session.peer, 64))
    {
        return false;
    }
    let mut ids = std::collections::BTreeSet::new();
    snapshot
        .sessions
        .iter()
        .all(|session| ids.insert(&session.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nddev_device_sync_adapter_io::NativeIo;

    fn session(id: &str) -> RemoteSession {
        RemoteSession {
            id: id.into(),
            peer: "f".repeat(64),
            status: SessionState::Connected,
        }
    }

    fn snapshot() -> RemoteSnapshot {
        RemoteSnapshot {
            instance: "a".repeat(32),
            generation: 1,
            endpoint: "e".repeat(64),
            selected: None,
            sessions: vec![session(&"1".repeat(32))],
        }
    }

    #[test]
    fn hex_accepts_only_exact_ascii_hex_lengths() {
        assert!(hex(&"ab".repeat(16), 32));
        assert!(!hex(&"ab".repeat(16), 31));
        assert!(!hex("zz", 2));
        assert!(!hex("aB\n", 3));
    }

    #[test]
    fn consistent_snapshot_requires_hex_identity_bounded_sessions_and_membership() {
        assert!(consistent(&snapshot()));
        let mut wrong = snapshot();
        wrong.instance = "not-hex".into();
        assert!(!consistent(&wrong));
        let mut wrong = snapshot();
        wrong.selected = Some("2".repeat(32));
        assert!(!consistent(&wrong));
        let mut ok = snapshot();
        ok.selected = Some("1".repeat(32));
        assert!(consistent(&ok));
        let mut crowded = snapshot();
        crowded.sessions = vec![session(&"1".repeat(32)); 33];
        assert!(!consistent(&crowded));
    }

    #[test]
    fn duplicate_session_ids_are_rejected() {
        let mut duplicated = snapshot();
        duplicated.sessions.push(session(&"1".repeat(32)));
        assert!(!consistent(&duplicated));
        let mut distinct = snapshot();
        distinct.sessions.push(session(&"2".repeat(32)));
        assert!(consistent(&distinct));
    }

    #[test]
    fn constructor_requires_absolute_paths() {
        let io = NativeIo::new(1).unwrap().process_handle();
        assert!(matches!(
            Remote::new(io.clone(), PathBuf::from("rds"), None),
            Err(Error::Io(IoError::InvalidConfiguration))
        ));
        assert!(matches!(
            Remote::new(
                io.clone(),
                std::env::temp_dir().join("rds"),
                Some(PathBuf::from("rel"))
            ),
            Err(Error::Io(IoError::InvalidConfiguration))
        ));
        assert!(
            Remote::new(
                io,
                std::env::temp_dir().join("rds"),
                Some(std::env::temp_dir().join("rds-state"))
            )
            .is_ok()
        );
    }
}

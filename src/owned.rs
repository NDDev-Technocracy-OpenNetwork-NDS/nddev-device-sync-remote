//! Owned native screen primitives. Existing NdsClient retains Accounts purpose
//! owner/fence and cancels/joins native resources before releasing its lease.
#[path = "owned_grant.rs"]
pub mod grant;
#[path = "owned_runtime.rs"]
pub mod runtime;
use nddev_device_sync_domain::{modules::ModuleError, screen::ScreenError};
pub type Result<T> = std::result::Result<T, ScreenError>;
use nddev_device_sync_application::screen::{HostAccess, authorize_host, native_grant_payload};
use nddev_device_sync_domain::{
    DeviceId,
    screen::{
        EndpointBinding, MAX_ENCODED_FRAME_BYTES, ViewLease, ViewProjection, ViewState,
        frame_layout,
    },
};
use rds_core::{
    Codec, FrameHeader, ServiceKind,
    grant::{GRANT_VERSION, Grant, GrantConstraints, GrantPayload},
};
use rds_desktop::client::RelayDecoder;
use rds_net::{Endpoint, EndpointAddr, EndpointConfig, SecretKey, bind_endpoint};
use std::time::Duration;

fn require(condition: bool, error: ScreenError) -> Result<()> {
    if condition { Ok(()) } else { Err(error) }
}
fn input<E>(_: E) -> ScreenError {
    ScreenError::Module(ModuleError::InvalidInput)
}
fn response<E>(_: E) -> ScreenError {
    ScreenError::Module(ModuleError::InvalidResponse)
}
fn unavailable<E>(_: E) -> ScreenError {
    ScreenError::Module(ModuleError::Unavailable)
}
fn timeout<E>(_: E) -> ScreenError {
    ScreenError::Module(ModuleError::Timeout)
}
fn io_error(error: std::io::Error) -> ScreenError {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::TimedOut => timeout(()),
        ErrorKind::WouldBlock => ScreenError::Module(ModuleError::Busy),
        ErrorKind::InvalidData | ErrorKind::InvalidInput => response(()),
        ErrorKind::UnexpectedEof
        | ErrorKind::BrokenPipe
        | ErrorKind::ConnectionReset
        | ErrorKind::ConnectionAborted
        | ErrorKind::NotConnected => ScreenError::Closed,
        _ => unavailable(()),
    }
}
fn connection_error(error: rds_net::ConnectionError) -> ScreenError {
    match error {
        rds_net::ConnectionError::TimedOut => timeout(()),
        rds_net::ConnectionError::ApplicationClosed(closed) if closed.error_code == 2u32.into() => {
            ScreenError::Denied
        }
        rds_net::ConnectionError::ApplicationClosed(closed) if closed.error_code == 3u32.into() => {
            ScreenError::Expired
        }
        rds_net::ConnectionError::ApplicationClosed(_)
        | rds_net::ConnectionError::ConnectionClosed(_)
        | rds_net::ConnectionError::LocallyClosed
        | rds_net::ConnectionError::Reset => ScreenError::Closed,
        _ => unavailable(()),
    }
}
fn authorization_error(error: anyhow::Error) -> ScreenError {
    // The existing native client wraps errors in anyhow. Inspect typed causes
    // only; never format, parse or forward its peer/provider/OS text.
    if error
        .downcast_ref::<tokio::time::error::Elapsed>()
        .is_some()
    {
        return timeout(());
    }
    if let Some(error) = error.downcast_ref::<rds_net::ConnectionError>() {
        return connection_error(error.clone());
    }
    if let Some(error) = error.downcast_ref::<rds_core::grant::GrantError>() {
        return grant::error(error);
    }
    unavailable(())
}
fn desktop_error(error: rds_desktop::DesktopError) -> ScreenError {
    use rds_desktop::DesktopError;
    match error {
        DesktopError::Connection(error) => connection_error(error),
        DesktopError::Io(error) => io_error(error),
        DesktopError::Closed(_)
        | DesktopError::Read(_)
        | DesktopError::ReadExact(_)
        | DesktopError::Write(_) => ScreenError::Closed,
        DesktopError::Capture(_) => unavailable(()),
        DesktopError::Decode(_) | DesktopError::Encode(_) => {
            ScreenError::Module(ModuleError::ProviderFailed)
        }
        DesktopError::Input(_) => ScreenError::Denied,
    }
}
fn body_error(error: iroh::endpoint::ReadToEndError) -> ScreenError {
    match error {
        iroh::endpoint::ReadToEndError::TooLong => ScreenError::FrameLimit,
        iroh::endpoint::ReadToEndError::Read(error) => match error {
            rds_net::ReadError::ConnectionLost(error) => connection_error(error),
            _ => ScreenError::Closed,
        },
    }
}

/// One key/endpoint instance. Never loads a native key path or default identity.
pub struct OwnedEndpoint {
    device: DeviceId,
    instance_id: [u8; 16],
    key: SecretKey,
    endpoint: Endpoint,
    binding: Option<EndpointBinding>,
}

impl OwnedEndpoint {
    /// `key` must be owned by the existing protected Accounts remote-module namespace.
    /// Caller retains its cross-process credential ownership lease until close.
    pub async fn bind(
        device: DeviceId,
        instance_id: [u8; 16],
        key: SecretKey,
        mut config: EndpointConfig,
    ) -> Result<Self> {
        require(instance_id.iter().any(|byte| *byte != 0), input(()))?;
        config.secret_key = Some(key.clone());
        config.alpns = vec![rds_core::ALPN.to_vec()];
        config.validate().map_err(input)?;
        let discovery = config.discovery;
        let endpoint = bind_endpoint(config).await.map_err(unavailable)?;
        if endpoint.id() != key.public() {
            endpoint.close().await;
            return Err(ScreenError::Denied);
        }
        // An identity-only viewer depends on the normal address lookup path.
        // Binding a socket alone can publish an endpoint before a usable relay
        // address exists. Keep this instance/key owned until readiness or close.
        if discovery
            && tokio::time::timeout(Duration::from_secs(10), endpoint.online())
                .await
                .is_err()
        {
            endpoint.close().await;
            return Err(timeout(()));
        }
        Ok(Self {
            device,
            instance_id,
            key,
            endpoint,
            binding: None,
        })
    }

    pub fn public_key(&self) -> [u8; 32] {
        *self.endpoint.id().as_bytes()
    }

    /// Local tuple admission after the caller receives a successful guarded
    /// Accounts screen_bind result. Accounts is the sole HTTP/PoP producer.
    /// This function does not authenticate passive data or authorize media.
    pub fn accept_binding_from_accounts(
        &mut self,
        binding: EndpointBinding,
        original_base: u64,
    ) -> Result<()> {
        require(
            self.binding.is_none(),
            ScreenError::Module(ModuleError::Busy),
        )?;
        require(
            original_base < nddev_device_sync_domain::sync::MAX_REVISION,
            input(()),
        )?;
        require(
            binding.device == self.device
                && binding.endpoint == self.public_key()
                && binding.instance_id == self.instance_id
                && original_base.checked_add(1) == Some(binding.revision),
            ScreenError::Denied,
        )?;
        self.binding = Some(binding);
        Ok(())
    }

    fn binding(&self) -> Result<&EndpointBinding> {
        self.binding.as_ref().ok_or(ScreenError::Denied)
    }

    /// Maps current server intent + real local source reports into native policy.
    /// Caller owns source readiness/consent and current Accounts LocalUseFence.
    /// No capture starts here. This admission is consumed once by make_builtin_x11.
    pub fn admit_host(
        &self,
        view: &mut ViewProjection,
        local: &HostAccess,
        now_ms: u64,
    ) -> Result<HostAdmission> {
        let binding = self.binding()?;
        view.permit().lease.remaining_ms(now_ms)?;
        require(
            view.state(now_ms) == ViewState::Requested,
            ScreenError::Closed,
        )?;
        require(
            local.local_device == self.device
                && local.local_endpoint == self.public_key()
                && local.local_binding_revision == binding.revision,
            ScreenError::Denied,
        )?;
        authorize_host(view, local, now_ms)?;
        let permit = view.permit();
        let grant = Grant::issue_at(
            self.key.signing_key(),
            GrantPayload {
                version: GRANT_VERSION,
                revision: 1,
                issuer: self.public_key(),
                subject: permit.viewer_endpoint,
                audience: self.public_key(),
                nonce: rand::random(),
                not_before: now_ms / 1000,
                expires_at: permit.lease.expires_at_ms() / 1000,
                services: vec![ServiceKind::DesktopView],
                constraints: GrantConstraints {
                    displays: Some(vec![permit.lease.display()]),
                    ..Default::default()
                },
                tenant: Some(view.id().into()),
                policy_revision: Some(binding.revision),
            },
        );
        let artifact = postcard::to_allocvec(&grant)
            .map_err(|_| ScreenError::Module(ModuleError::ProviderFailed))?;
        native_grant_payload(&artifact)?;
        Ok(HostAdmission {
            endpoint: self.endpoint.clone(),
            binding_revision: binding.revision,
            artifact,
            lease: permit.lease,
        })
    }

    /// Verifies real native bytes against Ready, current local binding and peer.
    /// The caller also freshly verifies target binding via the signed API.
    fn admit_viewer(
        &self,
        view: &mut ViewProjection,
        target: EndpointAddr,
        native_bytes: &[u8],
        now_ms: u64,
    ) -> Result<(EndpointAddr, Grant)> {
        let binding = self.binding()?;
        native_grant_payload(native_bytes)?;
        let permit = view.permit();
        require(
            permit.viewer == self.device
                && permit.viewer_endpoint == self.public_key()
                && permit.viewer_revision == binding.revision
                && target.id.as_bytes() == &permit.host_endpoint,
            ScreenError::Denied,
        )?;
        let (grant, trailing): (Grant, &[u8]) =
            postcard::take_from_bytes(native_bytes).map_err(response)?;
        require(trailing.is_empty(), response(()))?;
        grant::verify_native(view, &grant, &self.public_key(), now_ms)?;
        Ok((target, grant))
    }

    /// NdsClient cancels/joins owned sessions first, then closes this endpoint.
    pub async fn close(self) {
        self.endpoint.close().await;
    }

    /// Existing NdsClient calls this under its current fence and checks the
    /// fence again after the await before publishing the handle to rendering.
    pub async fn open_view(
        &self,
        view: &mut ViewProjection,
        target: EndpointAddr,
        native_bytes: &[u8],
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<runtime::NativeViewer> {
        let (target, grant) =
            self.admit_viewer(view, target, native_bytes, runtime::wall_now()?)?;
        runtime::NativeViewer::open(&self.endpoint, target, view.clone(), &grant, cancel).await
    }
}

/// Rust-only artifact/policy; never a Flutter key/grant DTO or durable registry.
pub struct HostAdmission {
    endpoint: Endpoint,
    binding_revision: u64,
    artifact: Vec<u8>,
    lease: ViewLease,
}
impl HostAdmission {
    pub fn native_artifact(&self) -> &[u8] {
        &self.artifact
    }

    /// After the canonical ready ACK, actual bounded X11 inventory and the
    /// NdsClient post-await fence check. Uses the public built-in backend,
    /// without requiring an exported X11Producer/another DesktopSource.
    pub fn make_builtin_x11(
        self,
        mut ready: ViewProjection,
        local: &HostAccess,
        caps: rds_core::DesktopCaps,
    ) -> Result<runtime::BuiltinHost> {
        let now = runtime::wall_now()?;
        let lease = ready.permit().lease;
        lease.remaining_ms(now)?;
        require(
            ready.permit().host_endpoint == *self.endpoint.id().as_bytes()
                && self.binding_revision == local.local_binding_revision
                && (
                    lease.display(),
                    lease.max_fps(),
                    lease.output_height(),
                    lease.admitted_at_ms(),
                    lease.expires_at_ms(),
                ) == (
                    self.lease.display(),
                    self.lease.max_fps(),
                    self.lease.output_height(),
                    self.lease.admitted_at_ms(),
                    self.lease.expires_at_ms(),
                ),
            ScreenError::Denied,
        )?;
        authorize_host(&mut ready, local, now)?;
        let grant: Grant = postcard::from_bytes(&self.artifact).map_err(response)?;
        let subject = ready.permit().viewer_endpoint;
        grant::verify_native(&mut ready, &grant, &subject, now)?;
        runtime::BuiltinHost::new(self.endpoint, ready, caps)
    }
}

/// Native body bounds; the sole owned receiver supplies its earlier causal
/// empty-queue anchor and deadline instead of minting a timestamp here.
async fn read_stream(stream: &mut rds_net::RecvStream) -> Result<(FrameHeader, Vec<u8>)> {
    let header: FrameHeader = rds_net::read_frame(stream).await.map_err(io_error)?;
    let stride = header.width.checked_mul(4).ok_or(ScreenError::FrameLimit)?;
    let length = (stride as usize)
        .checked_mul(header.height as usize)
        .ok_or(ScreenError::FrameLimit)?;
    frame_layout(header.width, header.height, stride, length)?;
    require(
        header.codec == Codec::H264 && header.seq != u64::MAX,
        response(()),
    )?;
    let bytes = stream
        .read_to_end(MAX_ENCODED_FRAME_BYTES)
        .await
        .map_err(body_error)?;
    require(!bytes.is_empty(), response(()))?;
    Ok((header, bytes))
}

/// Uses the existing bounded native blocking decoder, without cloning its
/// capture/scaling/codec algorithms into NDS. Caller owns sequence/freshness,
/// cancellation and <=2 frame tickets; validates output before publication.
pub async fn decode_encoded(
    decoder: RelayDecoder,
    header: FrameHeader,
    payload: Vec<u8>,
) -> Result<(RelayDecoder, rds_desktop::client::RelayOutcome)> {
    let stride = header.width.checked_mul(4).ok_or(ScreenError::FrameLimit)?;
    let length = (stride as usize)
        .checked_mul(header.height as usize)
        .ok_or(ScreenError::FrameLimit)?;
    frame_layout(header.width, header.height, stride, length)?;
    require(
        payload.len() <= MAX_ENCODED_FRAME_BYTES && !payload.is_empty(),
        ScreenError::FrameLimit,
    )?;
    // The checked provider's deadline seals the result, then waits for native
    // completion. Owned receiver cancellation retains this operation in
    // DecodeWork; a timeout cannot be mistaken for a completed native drain.
    let (decoder, outcome) = decoder
        .push_bounded(header, payload)
        .await
        .map_err(desktop_error)?;
    if let rds_desktop::client::RelayOutcome::Frame(frame) = &outcome {
        frame_layout(frame.width, frame.height, frame.stride, frame.data.len())?;
    }
    Ok((decoder, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_boundary_drops_native_os_and_peer_details() {
        let canary = "PRIVATE_NATIVE_PATH_OR_PEER_DETAIL";
        let failures = [
            io_error(std::io::Error::new(std::io::ErrorKind::NotFound, canary)),
            desktop_error(rds_desktop::DesktopError::Capture(canary.into())),
            desktop_error(rds_desktop::DesktopError::Decode(canary.into())),
            authorization_error(anyhow::anyhow!(canary)),
            grant::error(&rds_core::grant::GrantError::Malformed(canary.into())),
        ];
        assert_eq!(failures[0], unavailable(()));
        assert_eq!(failures[1], unavailable(()));
        assert_eq!(
            failures[2],
            ScreenError::Module(ModuleError::ProviderFailed)
        );
        assert_eq!(failures[3], unavailable(()));
        assert_eq!(failures[4], response(()));
        assert!(!format!("{failures:?}").contains(canary));
        assert_eq!(
            io_error(std::io::Error::new(std::io::ErrorKind::TimedOut, canary)),
            timeout(())
        );
        assert_eq!(
            body_error(iroh::endpoint::ReadToEndError::TooLong),
            ScreenError::FrameLimit
        );
        assert_eq!(
            grant::error(&rds_core::grant::GrantError::BadSignature),
            ScreenError::Denied
        );
        assert_eq!(
            grant::error(&rds_core::grant::GrantError::Expired),
            ScreenError::Expired
        );
    }
    /// Transport readiness only; no NDS identity or media permission is fabricated.
    #[tokio::test]
    #[ignore = "requires actual network access to the configured discovery/relay services"]
    async fn discovery_bind_is_dialable_by_identity() {
        let viewer = OwnedEndpoint::bind(
            DeviceId::new("owned-viewer").unwrap(),
            [1; 16],
            SecretKey::generate(),
            EndpointConfig::default(),
        )
        .await
        .unwrap();
        let host = OwnedEndpoint::bind(
            DeviceId::new("owned-host").unwrap(),
            [2; 16],
            SecretKey::generate(),
            EndpointConfig::default(),
        )
        .await
        .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(12), async {
            tokio::join!(
                viewer
                    .endpoint
                    .connect(EndpointAddr::new(host.endpoint.id()), rds_core::ALPN),
                async { host.endpoint.accept().await.unwrap().await }
            )
        })
        .await;
        viewer.close().await;
        host.close().await;
        assert!(
            matches!(result, Ok((Ok(_), Ok(_)))),
            "ready endpoint was not discoverable by identity"
        );
    }

    #[tokio::test]
    async fn invalid_native_configuration_is_rejected_by_existing_validator() {
        let config = EndpointConfig {
            bind_addrs: vec![
                "127.0.0.1:0".parse().unwrap(),
                "127.0.0.1:0".parse().unwrap(),
            ],
            ..Default::default()
        };
        let result = OwnedEndpoint::bind(
            DeviceId::new("owned-fixture").unwrap(),
            [1; 16],
            SecretKey::generate(),
            config,
        )
        .await;
        assert_eq!(result.err().unwrap(), input(()));
    }
}

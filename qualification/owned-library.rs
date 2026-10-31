//! Compile the exported owned API against canonical Core. No I/O is invoked.
use nddev_device_sync_application::screen::HostAccess;
use nddev_device_sync_domain::{
    DeviceId,
    screen::{EndpointBinding, ScreenError, ViewProjection},
};
use nddev_device_sync_remote::owned::{
    HostAdmission, OwnedEndpoint,
    runtime::{BuiltinHost, FrameTicket, NativeViewer},
};
use rds_core::grant::Grant;
use rds_net::{Endpoint, EndpointAddr, EndpointConfig, SecretKey};
use tokio_util::sync::CancellationToken;

pub async fn bind_contract(
    device: DeviceId,
    instance: [u8; 16],
    key: SecretKey,
    config: EndpointConfig,
) -> Result<OwnedEndpoint, ScreenError> {
    OwnedEndpoint::bind(device, instance, key, config).await
}

pub fn binding_contract(
    endpoint: &mut OwnedEndpoint,
    binding: EndpointBinding,
    base: u64,
) -> Result<(), ScreenError> {
    endpoint.accept_binding_from_accounts(binding, base)
}

pub fn host_contract(
    endpoint: &OwnedEndpoint,
    projection: &mut ViewProjection,
    access: &HostAccess,
    now: u64,
) -> Result<HostAdmission, ScreenError> {
    endpoint.admit_host(projection, access, now)
}

pub async fn viewer_contract(
    endpoint: &Endpoint,
    target: EndpointAddr,
    projection: ViewProjection,
    grant: &Grant,
    cancel: CancellationToken,
) -> Result<NativeViewer, ScreenError> {
    NativeViewer::open(endpoint, target, projection, grant, cancel).await
}

pub fn frame_contract(ticket: &mut FrameTicket) -> Result<Vec<u8>, ScreenError> {
    ticket.check_live()?;
    let _ = ticket.dimensions()?;
    ticket.read_once()
}

pub async fn close_contract(viewer: &mut NativeViewer) -> Result<(), ScreenError> {
    viewer.close().await
}

pub fn drained_contract(viewer: &NativeViewer) -> bool {
    viewer.is_drained()
}

/// The downstream witness compiles the exported host path as well as viewing.
/// It supplies canonical Core authority; no host or capture is activated here.
pub fn builtin_host_contract(
    admission: HostAdmission,
    ready: ViewProjection,
    access: &HostAccess,
    caps: rds_core::DesktopCaps,
) -> Result<BuiltinHost, ScreenError> {
    admission.make_builtin_x11(ready, access, caps)
}

pub async fn serve_contract(
    host: BuiltinHost,
    cancel: CancellationToken,
) -> Result<(), ScreenError> {
    host.serve(cancel).await
}

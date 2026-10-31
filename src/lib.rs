//! Read the legacy RDS manager (the frozen `rds` executable) through its
//! keyless CLI. The optional Linux owned-screen adapter consumes
//! Accounts-admitted native viewing inputs;
//! the existing NdsClient owns authority, purpose lifetime and presentation.

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod adapter;
pub mod view;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use adapter::Remote;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use nddev_device_sync_adapter_io::{CancellationToken, IoError, NativeIo, ProcessIo};

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] IoError),
    #[error("rds protocol response is invalid or unsupported")]
    Protocol,
    #[error("native rds provider is not supported on this platform")]
    UnsupportedPlatform,
}

/// Application port: callers depend on a typed snapshot, not a native process.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub trait ReadRemote {
    fn sessions(
        &self,
        cancellation: CancellationToken,
    ) -> impl std::future::Future<Output = Result<view::RemoteSnapshot, Error>> + Send;
}

/// Canonical compiled-in runtime manifest for this integration.
pub const MANIFEST: &str = include_str!("../contracts/module.json");

/// Native owned-view primitives; Accounts and the existing NdsClient own NDS
/// authority, purpose-key lifetime and presentation. Linux qualification only.
#[cfg(all(target_os = "linux", feature = "owned-screen"))]
pub mod owned;

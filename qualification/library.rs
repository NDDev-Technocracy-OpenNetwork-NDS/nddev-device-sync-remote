//! Downstream package/link contract. No provider or credential I/O is invoked.
use rds_agent::{Agent, AgentPolicy};
use rds_net::{Endpoint, SecretKey};
use std::sync::Arc;

pub fn library_contract() {
    let _: &str = nddev_device_sync_remote::MANIFEST;
    let _: fn(Endpoint, AgentPolicy) -> Agent = Agent::new;
    let _: fn(Agent, Arc<dyn rds_desktop::DesktopSource>) -> Agent = Agent::with_desktop_source;
    let _: fn(&[u8; 32]) -> SecretKey = SecretKey::from_bytes;
    let _ = SecretKey::sign;
    let _ = SecretKey::signing_key;
    let _ = AgentPolicy::use_local_revocations;
    let _ = rds_net::bind_endpoint;
    let _ = rds_core::grant::Grant::issue_at;
    let _ = rds_client::connect_authorized;
    let _ = rds_desktop::client::DesktopSession::connect_opts;
    let _ = rds_desktop::client::RelayDecoder::push_bounded;
}

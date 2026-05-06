//! Compatibility entry point for the FIPS Drop receiver.
//!
//! Prefer `fips-drop-agent` for new deployments. This binary keeps the known
//! PoC command working and preserves its legacy default storage root.

#[tokio::main(flavor = "current_thread")]
async fn main() {
    fips::drop_agent::run_from_env().await;
}

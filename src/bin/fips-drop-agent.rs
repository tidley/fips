//! FIPS Drop receiver entry point.

#[tokio::main(flavor = "current_thread")]
async fn main() {
    fips::drop_agent::run_from_env().await;
}

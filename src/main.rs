use noether::cli;
use noether::error::NoetError;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), NoetError> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    cli::run().await
}

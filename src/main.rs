use noether::cli;
use noether::error::NoetError;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), NoetError> {
    let env_filter = EnvFilter::from_default_env();
    if std::env::var("NOET_LOG_FORMAT").is_ok_and(|value| value.eq_ignore_ascii_case("json")) {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(env_filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    }

    cli::run().await
}

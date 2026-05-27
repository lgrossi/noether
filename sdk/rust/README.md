# Noether Rust SDK

Rust client for the Noether decision sidecar API.

Noether does not call providers. Your integration owns provider transport:

```rust
use noether_sidecar::{FailMode, NoetherClient};
use noether::contract::AuthorizeRequest;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let noether = NoetherClient::new("http://127.0.0.1:4051")?
    .with_fail_mode(FailMode::FailClosed);

let decision = noether
    .require_authorization(&AuthorizeRequest {
        project: Some("noether".to_owned()),
        subject: Some("user:local".to_owned()),
        provider: Some("openai".to_owned()),
        model: Some("gpt-4.1".to_owned()),
        estimated_tokens: Some(1500),
        ..Default::default()
    })
    .await?;

// Your integration calls the provider here.

if let Some(reservation) = decision.reservation {
    noether.finalize(&reservation.id, &Default::default()).await?;
}
# Ok(())
# }
```

Use `FailMode::FailClosed` when sidecar unavailability should deny work, and
`FailMode::FailOpen` when local development should continue if Noether is down.

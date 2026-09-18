use std::{env, error::Error, net::SocketAddr, path::Path, sync::Arc};

use mail_push_service::{
    router, ApnsConfig, ApnsEnvironment, ApnsSender, ServiceConfig, SubscriptionStore,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let bind = required("TERN_PUSH_BIND")?.parse::<SocketAddr>()?;
    let database = required("TERN_PUSH_DATABASE")?;
    let environment = match required("TERN_APNS_ENVIRONMENT")?.as_str() {
        "production" => ApnsEnvironment::Production,
        "sandbox" => ApnsEnvironment::Sandbox,
        _ => return Err("TERN_APNS_ENVIRONMENT must be production or sandbox".into()),
    };
    let key_path = required("TERN_APNS_KEY_PATH")?;
    let key_id = required("TERN_APNS_KEY_ID")?;
    let team_id = required("TERN_APNS_TEAM_ID")?;
    let topic = required("TERN_APNS_TOPIC")?;
    let store = Arc::new(SubscriptionStore::open(Path::new(&database))?);
    let sender = Arc::new(ApnsSender::new(&ApnsConfig {
        key_path: Path::new(&key_path),
        key_id: &key_id,
        team_id: &team_id,
        topic: &topic,
        environment,
    })?);
    let app = router(
        store,
        sender,
        ServiceConfig {
            registration_token: required("TERN_PUSH_REGISTRATION_TOKEN")?,
            trigger_token: required("TERN_PUSH_TRIGGER_TOKEN")?,
        },
    );
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn required(name: &str) -> Result<String, Box<dyn Error>> {
    env::var(name).map_err(|_| format!("{name} is required").into())
}

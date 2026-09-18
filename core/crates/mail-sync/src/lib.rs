//! Orchestration owns protocol-to-database transitions; frontends never own IMAP.
use mail_db::Database;
use mail_model::{Account, Mailbox};

/// Implemented by platform credential stores. SQLite holds only the reference.
/// Credentials must never be included in errors or tracing fields.
pub trait CredentialProvider {
    fn password(&self, credential_ref: &str) -> Result<String, SyncError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("The credential is unavailable")]
    CredentialUnavailable,
    #[error("Mail synchronization failed: {0}")]
    Sync(String),
}

/// Fetch fully before opening a SQLite transaction. A network failure leaves the
/// previous offline snapshot readable. This first slice syncs Inbox headers only.
pub async fn sync_inbox(
    db: &Database,
    account: &Account,
    credentials: &dyn CredentialProvider,
) -> Result<Mailbox, SyncError> {
    let password = credentials.password(&account.credential_ref)?;
    let snapshot = mail_imap::fetch_inbox(account, &password)
        .await
        .map_err(|e| SyncError::Sync(e.to_string()))?;
    db.apply_snapshot(&account.id, &snapshot)
        .map_err(|e| SyncError::Sync(e.to_string()))
}

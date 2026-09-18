//! Stable application boundary exposed to native frontends via `UniFFI`.
use mail_db::Database;
use mail_model::{Account, Mailbox, MessageSummary};
use std::sync::{Arc, Mutex};

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MailError {
    #[error("Local mail storage: {message}")]
    Storage { message: String },
}
fn storage(error: impl std::fmt::Display) -> MailError {
    MailError::Storage {
        message: error.to_string(),
    }
}

#[derive(uniffi::Object)]
pub struct MailClient {
    database: Mutex<Database>,
}
#[uniffi::export]
#[allow(
    clippy::needless_pass_by_value,
    reason = "UniFFI requires owned strings at the foreign API boundary"
)]
impl MailClient {
    /// Open the canonical local store.
    ///
    /// # Errors
    /// Returns a storage error if the database cannot be opened or migrated.
    #[uniffi::constructor]
    pub fn new(database_path: String) -> Result<Arc<Self>, MailError> {
        Ok(Arc::new(Self {
            database: Mutex::new(Database::open(&database_path).map_err(storage)?),
        }))
    }
    /// List configured accounts without network access.
    ///
    /// # Errors
    /// Returns a storage error if the database cannot be read.
    pub fn list_accounts(&self) -> Result<Vec<Account>, MailError> {
        self.database
            .lock()
            .map_err(storage)?
            .list_accounts()
            .map_err(storage)
    }
    /// List cached mailboxes for one account.
    ///
    /// # Errors
    /// Returns a storage error if the database cannot be read.
    pub fn list_mailboxes(&self, account_id: String) -> Result<Vec<Mailbox>, MailError> {
        self.database
            .lock()
            .map_err(storage)?
            .list_mailboxes(&account_id)
            .map_err(storage)
    }
    /// Read a bounded page from the local cache.
    ///
    /// # Errors
    /// Returns a storage error if the database cannot be read.
    pub fn list_messages(
        &self,
        mailbox_id: String,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<MessageSummary>, MailError> {
        self.database
            .lock()
            .map_err(storage)?
            .list_messages(&mailbox_id, offset, limit)
            .map_err(storage)
    }
    /// Search the local cache with free text and structured operators.
    ///
    /// # Errors
    /// Returns a storage error if the query is malformed or the database
    /// cannot be read.
    pub fn search_messages(
        &self,
        query: String,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<MessageSummary>, MailError> {
        self.database
            .lock()
            .map_err(storage)?
            .search_messages(&query, offset, limit)
            .map_err(storage)
    }
}
uniffi::setup_scaffolding!();

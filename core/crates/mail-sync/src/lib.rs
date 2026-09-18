//! Orchestration owns protocol-to-database transitions; frontends never own IMAP.
use mail_db::Database;
use mail_model::{Account, DraftSyncStatus, Mailbox, RemoteDraft};
use std::{
    collections::{BTreeMap, HashSet},
    future::Future,
    hash::{DefaultHasher, Hash, Hasher},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MAX_ATTEMPTS: u32 = 4;
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(8);
const IDLE_REFRESH_INTERVAL: Duration = Duration::from_mins(25);

/// Implemented by platform credential stores. `SQLite` holds only the reference.
/// Credentials must never be included in errors or tracing fields.
pub trait CredentialProvider {
    /// Resolve the referenced credential without persisting its value.
    ///
    /// # Errors
    /// Returns `CredentialUnavailable` if the platform cannot provide it.
    fn password(&self, credential_ref: &str) -> Result<String, SyncError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("The credential is unavailable")]
    CredentialUnavailable,
    #[error("Mail synchronization failed: {0}")]
    Sync(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DraftSyncResult {
    pub downloaded: u32,
    pub uploaded: u32,
    pub conflicts_preserved: u32,
}

/// Discover mailboxes, replay expired durable moves, and fetch Inbox headers.
///
/// Each move is claimed in a short local transaction before network work and is
/// attempted once. An ambiguous replay failure remains durable for later
/// reconciliation rather than being issued again automatically. Read-only
/// discovery and snapshot failures use bounded transient retries and leave the
/// previous offline snapshot readable; cache writes happen only afterward.
///
/// # Errors
/// Returns an error if credentials, the server, TLS or local persistence fail.
pub async fn sync_inbox(
    db: &Database,
    account: &Account,
    credentials: &dyn CredentialProvider,
) -> Result<Mailbox, SyncError> {
    let password = credentials.password(&account.credential_ref)?;
    let remote_mailboxes = retry_transient(account, || {
        mail_imap::list_remote_mailboxes(account, &password)
    })
    .await
    .map_err(|e| SyncError::Sync(e.to_string()))?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        });
    let operations = db
        .claim_due_operations(&account.id, now_ms)
        .map_err(|e| SyncError::Sync(e.to_string()))?;
    for operation in operations {
        if let Err(error) = mail_imap::apply_operation(account, &password, &operation).await {
            db.fail_operation(&operation.id, &error.to_string())
                .map_err(|e| SyncError::Sync(e.to_string()))?;
            return Err(SyncError::Sync(error.to_string()));
        }
        db.complete_operation(&operation.id)
            .map_err(|e| SyncError::Sync(e.to_string()))?;
    }
    let snapshot = retry_transient(account, || mail_imap::fetch_inbox(account, &password))
        .await
        .map_err(|e| SyncError::Sync(e.to_string()))?;
    let mailbox = db
        .apply_snapshot(&account.id, &snapshot)
        .map_err(|e| SyncError::Sync(e.to_string()))?;
    db.cache_mailboxes(&account.id, &remote_mailboxes)
        .map_err(|e| SyncError::Sync(e.to_string()))?;
    Ok(mailbox)
}

/// Wait asynchronously for an Inbox update. IDLE disconnects, DNS failures,
/// route changes, and timeouts reconnect with capped exponential backoff.
/// Servers without IDLE trigger a periodic refresh instead.
///
/// # Errors
/// Returns after retry exhaustion, or immediately for configuration, TLS,
/// authentication, and protocol failures that reconnecting cannot repair.
pub async fn wait_for_inbox_change(
    account: &Account,
    credentials: &dyn CredentialProvider,
) -> Result<(), SyncError> {
    let password = credentials.password(&account.credential_ref)?;
    retry_transient(account, || {
        mail_imap::wait_for_inbox_change(account, &password, IDLE_REFRESH_INTERVAL)
    })
    .await
    .map_err(|error| SyncError::Sync(error.to_string()))
}

async fn retry_transient<T, F, Fut>(
    account: &Account,
    operation: F,
) -> Result<T, mail_imap::ImapError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, mail_imap::ImapError>>,
{
    retry_transient_with(account, operation, INITIAL_BACKOFF).await
}

async fn retry_transient_with<T, F, Fut>(
    account: &Account,
    mut operation: F,
    initial_backoff: Duration,
) -> Result<T, mail_imap::ImapError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, mail_imap::ImapError>>,
{
    let mut attempt = 1;
    loop {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(error) if error.is_retryable() && attempt < MAX_ATTEMPTS => {
                tokio::time::sleep(backoff(account, attempt, initial_backoff)).await;
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

fn backoff(account: &Account, retry: u32, initial: Duration) -> Duration {
    let exponent = retry.saturating_sub(1).min(31);
    let ceiling = initial.saturating_mul(1_u32 << exponent).min(MAX_BACKOFF);
    let floor = ceiling / 2;
    let jitter_range = ceiling.saturating_sub(floor);
    if jitter_range.is_zero() {
        return ceiling;
    }

    let mut hasher = DefaultHasher::new();
    account.id.hash(&mut hasher);
    retry.hash(&mut hasher);
    let jitter_nanos = hasher.finish() % u64::try_from(jitter_range.as_nanos()).unwrap_or(u64::MAX);
    floor.saturating_add(Duration::from_nanos(jitter_nanos))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "Test fixtures fail immediately")]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    fn account() -> Account {
        Account {
            id: "account".into(),
            email: "user@example.test".into(),
            display_name: "User".into(),
            imap_host: "imap.example.test".into(),
            imap_port: 993,
            username: "user@example.test".into(),
            credential_ref: "keychain:item".into(),
        }
    }

    #[tokio::test]
    async fn retries_transient_failures_with_fresh_attempts() {
        let attempts = Rc::new(Cell::new(0));
        let observed = Rc::clone(&attempts);
        let result = retry_transient_with(
            &account(),
            move || {
                let attempt = observed.get() + 1;
                observed.set(attempt);
                async move {
                    if attempt < 3 {
                        Err(mail_imap::ImapError::Connection)
                    } else {
                        Ok("connected")
                    }
                }
            },
            Duration::ZERO,
        )
        .await;
        assert_eq!(result, Ok("connected"));
        assert_eq!(attempts.get(), 3);
    }

    #[tokio::test]
    async fn permanent_failures_do_not_retry() {
        let attempts = Rc::new(Cell::new(0));
        let observed = Rc::clone(&attempts);
        let result = retry_transient_with(
            &account(),
            move || {
                observed.set(observed.get() + 1);
                async { Err::<(), _>(mail_imap::ImapError::Tls) }
            },
            Duration::ZERO,
        )
        .await;
        assert_eq!(result, Err(mail_imap::ImapError::Tls));
        assert_eq!(attempts.get(), 1);
    }

    #[tokio::test]
    async fn transient_failures_stop_after_the_attempt_limit() {
        let attempts = Rc::new(Cell::new(0));
        let observed = Rc::clone(&attempts);
        let result = retry_transient_with(
            &account(),
            move || {
                observed.set(observed.get() + 1);
                async { Err::<(), _>(mail_imap::ImapError::Timeout) }
            },
            Duration::ZERO,
        )
        .await;
        assert_eq!(result, Err(mail_imap::ImapError::Timeout));
        assert_eq!(attempts.get(), MAX_ATTEMPTS);
    }

    #[test]
    fn backoff_is_exponential_jittered_and_capped() {
        let account = account();
        for retry in 1_u32..=8 {
            let ceiling = INITIAL_BACKOFF
                .saturating_mul(1_u32 << retry.saturating_sub(1).min(31))
                .min(MAX_BACKOFF);
            let delay = backoff(&account, retry, INITIAL_BACKOFF);
            assert!(delay >= ceiling / 2);
            assert!(delay <= ceiling);
        }
    }
}

/// Reconcile Tern-managed drafts with the provider's Drafts mailbox.
///
/// Local edits are never discarded. If the server advanced from the local base,
/// the local edit is forked to a new draft before the remote version is applied.
/// Equal-revision divergent server copies are likewise preserved before stale UIDs
/// are removed. Each upload is acknowledged only if no newer local save raced it.
///
/// # Errors
/// Returns an error if credentials, safe IMAP replacement, or local persistence fails.
pub async fn sync_drafts(
    db: &Database,
    account: &Account,
    credentials: &dyn CredentialProvider,
) -> Result<DraftSyncResult, SyncError> {
    let password = credentials.password(&account.credential_ref)?;
    let snapshot = mail_imap::fetch_drafts(account, &password)
        .await
        .map_err(sync_error)?;
    let now = unix_timestamp()?;
    let mut result = DraftSyncResult::default();
    let mut stale_uids = Vec::new();
    let mut groups = BTreeMap::<String, Vec<RemoteDraft>>::new();
    for remote in snapshot.drafts {
        groups
            .entry(remote.draft_id.clone())
            .or_default()
            .push(remote);
    }

    for remotes in groups.values_mut() {
        remotes.sort_by_key(|remote| (remote.revision, remote.uid));
        let canonical = remotes.last().cloned().ok_or_else(|| {
            SyncError::Sync("draft reconciliation encountered an empty group".into())
        })?;
        for remote in remotes.iter().take(remotes.len().saturating_sub(1)) {
            if remote.revision == canonical.revision && !same_content(remote, &canonical) {
                db.preserve_remote_conflict(&account.id, remote, snapshot.uid_validity, now)
                    .map_err(sync_error)?;
                result.conflicts_preserved = result.conflicts_preserved.saturating_add(1);
            }
            stale_uids.push(remote.uid);
        }
        if db
            .merge_remote_draft(&account.id, &canonical, snapshot.uid_validity, now)
            .map_err(sync_error)?
            .is_some()
        {
            result.conflicts_preserved = result.conflicts_preserved.saturating_add(1);
        }
        result.downloaded = result.downloaded.saturating_add(1);
    }

    let pending = db
        .draft_sync_records(&account.id)
        .map_err(sync_error)?
        .into_iter()
        .filter(|record| record.draft.sync_status == DraftSyncStatus::Pending)
        .collect::<Vec<_>>();
    let replaced_uids = pending
        .iter()
        .filter_map(|record| record.remote_uid)
        .collect::<HashSet<_>>();
    for record in pending {
        let revision = record.remote_revision.unwrap_or(0).saturating_add(1);
        let uploaded = mail_imap::upload_draft(
            account,
            &password,
            &snapshot.remote_name,
            &record.draft,
            revision,
            record.remote_uid_validity,
            record.remote_uid,
        )
        .await
        .map_err(sync_error)?;
        db.mark_draft_synced(
            &record.draft.id,
            record.local_generation,
            revision,
            snapshot.uid_validity,
            uploaded.uid,
        )
        .map_err(sync_error)?;
        result.uploaded = result.uploaded.saturating_add(1);
    }

    stale_uids.retain(|uid| !replaced_uids.contains(uid));
    stale_uids.sort_unstable();
    stale_uids.dedup();
    mail_imap::delete_draft_uids(
        account,
        &password,
        &snapshot.remote_name,
        snapshot.uid_validity,
        &stale_uids,
    )
    .await
    .map_err(sync_error)?;
    Ok(result)
}

fn same_content(left: &RemoteDraft, right: &RemoteDraft) -> bool {
    left.recipients == right.recipients
        && left.cc == right.cc
        && left.bcc == right.bcc
        && left.subject == right.subject
        && left.body == right.body
}

fn unix_timestamp() -> Result<i64, SyncError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| SyncError::Sync("the system clock is before the Unix epoch".into()))?;
    i64::try_from(duration.as_secs())
        .map_err(|_| SyncError::Sync("the system clock is out of range".into()))
}

fn sync_error(error: impl std::fmt::Display) -> SyncError {
    SyncError::Sync(error.to_string())
}

#[cfg(test)]
mod draft_tests {
    use super::*;

    fn remote(uid: u32, body: &str) -> RemoteDraft {
        RemoteDraft {
            draft_id: "same".into(),
            revision: 2,
            uid,
            recipients: vec!["one@example.test".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "subject".into(),
            body: body.into(),
        }
    }

    #[test]
    fn divergent_equal_revisions_are_not_considered_duplicates() {
        assert!(same_content(&remote(1, "one"), &remote(2, "one")));
        assert!(!same_content(&remote(1, "one"), &remote(2, "two")));
    }
}

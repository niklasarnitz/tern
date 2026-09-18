#![allow(
    clippy::unwrap_used,
    reason = "Test setup and assertions should fail immediately"
)]

use mail_core::MailClient;
use mail_db::Database;
use mail_model::{Account, DraftSave, DraftSyncStatus, MailboxSnapshot, RemoteHeader};

#[test]
fn ffi_api_reopens_a_persisted_snapshot_without_network() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mail.sqlite");
    let path = path.to_str().unwrap();
    let mailbox_id;
    let archive_id;
    {
        let db = Database::open(path).unwrap();
        db.upsert_account(&Account {
            id: "test-account".into(),
            email: "test@example.invalid".into(),
            display_name: "Offline test".into(),
            imap_host: "unreachable.invalid".into(),
            imap_port: 993,
            username: "test".into(),
            credential_ref: "keychain-reference-only".into(),
        })
        .unwrap();
        let mailbox = db
            .apply_snapshot(
                "test-account",
                &MailboxSnapshot {
                    remote_name: "INBOX".into(),
                    uid_validity: 7,
                    uid_next: Some(101),
                    headers: (1..=100)
                        .map(|uid| RemoteHeader {
                            uid,
                            message_id: Some(format!("<{uid}@example.invalid>")),
                            subject: format!("Message {uid}"),
                            sender: "Sender".into(),
                            date: "2026-09-18".into(),
                            is_read: uid % 2 == 0,
                            is_starred: false,
                            ..RemoteHeader::default()
                        })
                        .collect(),
                },
            )
            .unwrap();
        mailbox_id = mailbox.id;
        archive_id = db
            .apply_snapshot(
                "test-account",
                &MailboxSnapshot {
                    remote_name: "Archive".into(),
                    uid_validity: 3,
                    uid_next: Some(1),
                    headers: Vec::new(),
                },
            )
            .unwrap()
            .id;
    }
    let client = MailClient::new(path.into()).unwrap();
    assert_eq!(client.list_accounts().unwrap().len(), 1);
    assert_eq!(
        client.list_mailboxes("test-account".into()).unwrap().len(),
        2
    );
    let page = client.list_messages(mailbox_id.clone(), 0, 30).unwrap();
    assert_eq!(page.len(), 30);
    assert_eq!(page[0].remote_uid, 100);
    let final_page = client.list_messages(mailbox_id.clone(), 90, 30).unwrap();
    assert_eq!(final_page.len(), 10);
    let search_page = client
        .search_messages(
            "subject:\"Message 41\" from:Sender is:unread account:test-account".into(),
            0,
            10,
        )
        .unwrap();
    assert_eq!(search_page.len(), 1);
    assert_eq!(search_page[0].remote_uid, 41);

    let operation_id = client
        .queue_message_move(mailbox_id.clone(), page[0].id.clone(), archive_id, i64::MAX)
        .unwrap();
    assert_eq!(
        client
            .list_messages(mailbox_id.clone(), 0, 100)
            .unwrap()
            .len(),
        99
    );
    assert!(client.undo_operation(operation_id).unwrap());
    assert_eq!(client.list_messages(mailbox_id, 0, 100).unwrap().len(), 100);
    drop(client);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn ffi_draft_save_is_immediately_offline_readable() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mail.sqlite");
    let path = path.to_str().unwrap();
    let db = Database::open(path).unwrap();
    db.upsert_account(&Account {
        id: "draft-account".into(),
        email: "draft@example.invalid".into(),
        display_name: "Draft test".into(),
        imap_host: "unreachable.invalid".into(),
        imap_port: 993,
        username: "draft".into(),
        credential_ref: "keychain-reference-only".into(),
    })
    .unwrap();
    drop(db);

    let client = MailClient::new(path.into()).unwrap();
    let saved = client
        .save_draft(DraftSave {
            id: None,
            account_id: "draft-account".into(),
            recipients: vec!["recipient@example.invalid".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Offline".into(),
            body: "Saved without a network call".into(),
        })
        .unwrap();
    assert_eq!(saved.sync_status, DraftSyncStatus::Pending);
    drop(client);

    let reopened = MailClient::new(path.into()).unwrap();
    let drafts = reopened.list_drafts("draft-account".into()).unwrap();
    assert_eq!(drafts, vec![saved]);
}

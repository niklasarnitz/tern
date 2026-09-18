#![allow(
    clippy::unwrap_used,
    reason = "Test setup and assertions should fail immediately"
)]

use mail_core::MailClient;
use mail_db::Database;
use mail_model::{Account, MailboxSnapshot, RemoteHeader};

#[test]
fn ffi_api_reopens_a_persisted_snapshot_without_network() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mail.sqlite");
    let path = path.to_str().unwrap();
    let mailbox_id;
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
    }
    let client = MailClient::new(path.into()).unwrap();
    assert_eq!(client.list_accounts().unwrap().len(), 1);
    assert_eq!(
        client.list_mailboxes("test-account".into()).unwrap().len(),
        1
    );
    let page = client.list_messages(mailbox_id.clone(), 0, 30).unwrap();
    assert_eq!(page.len(), 30);
    assert_eq!(page[0].remote_uid, 100);
    let final_page = client.list_messages(mailbox_id, 90, 30).unwrap();
    assert_eq!(final_page.len(), 10);
    drop(client);
    std::fs::remove_file(path).unwrap();
}

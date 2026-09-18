//! Explicit, isolated fixture for native bridge and visual testing.
use mail_db::Database;
use mail_model::{Account, MailboxSnapshot, RemoteHeader};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("Provide a new fixture database path")?;
    // Refuse to overwrite an existing real mail store.
    let _reserved = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let db = Database::open(&path)?;
    db.upsert_account(&Account {
        id: "fixture".into(),
        email: "fixture@example.invalid".into(),
        display_name: "Offline fixture".into(),
        imap_host: "unreachable.invalid".into(),
        imap_port: 993,
        username: "fixture".into(),
        credential_ref: "no-credential".into(),
    })?;
    db.apply_snapshot(
        "fixture",
        &MailboxSnapshot {
            remote_name: "INBOX".into(),
            uid_validity: 17,
            uid_next: Some(106),
            headers: (1..=105)
                .map(|uid| {
                    let mut header = RemoteHeader {
                        uid,
                        message_id: Some(format!("<{uid}@example.invalid>")),
                        subject: format!("Offline message {uid}"),
                        sender: "Tern fixture <fixture@example.invalid>".into(),
                        senders: vec!["Tern fixture <fixture@example.invalid>".into()],
                        date: "2026-09-18T09:30:00+00:00".into(),
                        recipients: vec!["Reader <reader@example.invalid>".into()],
                        reply_to: vec!["Tern replies <reply@example.invalid>".into()],
                        sent_at: Some(1_789_723_800),
                        list_id: vec!["Tern Updates <updates.tern.example>".into()],
                        list_post: vec!["mailto:updates@tern.example".into()],
                        list_unsubscribe: vec!["https://tern.example/unsubscribe".into()],
                        authentication_results: vec![
                            "mx.example.invalid; dkim=pass; spf=pass; dmarc=pass".into(),
                        ],
                        received_spf: vec!["pass".into()],
                        is_read: uid % 2 == 0,
                        is_starred: uid % 5 == 0,
                        ..RemoteHeader::default()
                    };
                    if uid == 105 {
                        header.content = Some(mail_model::MessageContent {
                            attachments: vec![mail_model::Attachment {
                                id: "part-1".into(),
                                filename: "quarterly-report.pdf".into(),
                                mime_type: "application/pdf".into(),
                                content_id: None,
                                data: vec![0; 24_576],
                            }],
                            ..mail_model::MessageContent::default()
                        });
                    }
                    header
                })
                .collect(),
        },
    )?;
    println!("Created isolated cache fixture at {path}");
    Ok(())
}

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
                .map(|uid| RemoteHeader {
                    uid,
                    message_id: Some(format!("<{uid}@example.invalid>")),
                    subject: format!("Offline message {uid}"),
                    sender: "Tern fixture <fixture@example.invalid>".into(),
                    date: "2026-09-18T09:30:00+00:00".into(),
                    is_read: uid % 2 == 0,
                    is_starred: uid % 5 == 0,
                })
                .collect(),
        },
    )?;
    println!("Created isolated cache fixture at {path}");
    Ok(())
}

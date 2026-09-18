use clap::{Parser, Subcommand};
use mail_db::Database;
use mail_model::{Account, DiscoveryOverrides};
use mail_sync::{CredentialProvider, SyncError};
use std::{error::Error, path::PathBuf};

#[derive(Parser)]
#[command(about = "Tern local mail store and first-slice IMAP importer")]
struct Cli {
    #[arg(long, default_value = "tern.sqlite")]
    database: PathBuf,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Add configuration from JSON. Never put a password in the JSON file.
    AddAccount {
        config: PathBuf,
    },
    /// Discover and diagnose implicit-TLS IMAP settings without authenticating.
    DiscoverAccount {
        email: String,
        /// Use this server instead of automatic discovery.
        #[arg(long)]
        imap_host: Option<String>,
        /// Override the discovered implicit-TLS port.
        #[arg(long)]
        imap_port: Option<u16>,
        /// Override the discovered login name.
        #[arg(long)]
        username: Option<String>,
    },
    Accounts,
    Mailboxes {
        account: String,
    },
    /// Read cached headers only; makes no network connections.
    Messages {
        mailbox: String,
        #[arg(long, default_value_t = 0)]
        offset: u32,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Read locally cached drafts only; makes no network connections.
    Drafts {
        account: String,
    },
    /// Fetch the most recent 100 Inbox headers using verified implicit TLS.
    /// Password is prompted without echo and is never stored.
    Sync {
        account: String,
    },
    /// Reconcile local drafts with the provider's Drafts mailbox.
    /// Password is prompted without echo and is never stored.
    SyncDrafts {
        account: String,
    },
}
struct PromptCredentials;
impl CredentialProvider for PromptCredentials {
    fn password(&self, _: &str) -> Result<String, SyncError> {
        rpassword::prompt_password("IMAP password (not stored): ")
            .map_err(|_| SyncError::CredentialUnavailable)
    }
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let database_path = cli.database.to_str().ok_or("Invalid database path")?;
    match cli.command {
        Command::AddAccount { config } => {
            let db = Database::open(database_path)?;
            let account: Account = serde_json::from_slice(&std::fs::read(config)?)?;
            if account.id.is_empty()
                || account.imap_host.is_empty()
                || account.username.is_empty()
                || account.imap_port == 0
            {
                return Err("Account id, host, username and nonzero port are required".into());
            }
            db.upsert_account(&account)?;
            println!("Account configuration saved. No credentials stored.");
        }
        Command::DiscoverAccount {
            email,
            imap_host,
            imap_port,
            username,
        } => {
            let discovery = mail_core::discover_account(
                email,
                DiscoveryOverrides {
                    host: imap_host,
                    port: imap_port,
                    username,
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&discovery)?);
        }
        Command::Accounts => {
            let db = Database::open(database_path)?;
            println!("{}", serde_json::to_string_pretty(&db.list_accounts()?)?);
        }
        Command::Mailboxes { account } => {
            let db = Database::open(database_path)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&db.list_mailboxes(&account)?)?
            );
        }
        Command::Messages {
            mailbox,
            offset,
            limit,
        } => {
            let db = Database::open(database_path)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&db.list_messages(&mailbox, offset, limit)?)?
            );
        }
        Command::Drafts { account } => {
            let db = Database::open(database_path)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&db.list_drafts(&account)?)?
            );
        }
        Command::Sync { account } => {
            let db = Database::open(database_path)?;
            let account = db
                .list_accounts()?
                .into_iter()
                .find(|a| a.id == account)
                .ok_or("Account not found")?;
            let mailbox = mail_sync::sync_inbox(&db, &account, &PromptCredentials).await?;
            println!("{}", serde_json::to_string_pretty(&mailbox)?);
        }
        Command::SyncDrafts { account } => {
            let db = Database::open(database_path)?;
            let account = db
                .list_accounts()?
                .into_iter()
                .find(|candidate| candidate.id == account)
                .ok_or("Account not found")?;
            let result = mail_sync::sync_drafts(&db, &account, &PromptCredentials).await?;
            println!(
                "Drafts synchronized: {} downloaded, {} uploaded, {} conflicts preserved",
                result.downloaded, result.uploaded, result.conflicts_preserved
            );
        }
    }
    Ok(())
}

//! Secure IMAP account discovery from provider data, domain standards, and probes.

use std::{collections::HashSet, convert::TryFrom, sync::Arc, time::Duration};

use hickory_resolver::{
    proto::rr::{rdata::SRV, RData},
    TokioResolver,
};
use mail_model::{
    AccountDiscovery, AuthenticationKind, ConnectionDiagnostic, DiagnosticStatus,
    DiscoveryOverrides, DiscoverySource, MailServerConfig,
};
use reqwest::redirect::Policy;
use rustls::{pki_types::ServerName, ClientConfig, RootCertStore};
use serde::Deserialize;
use tokio::{io::AsyncReadExt, net::TcpStream, task::JoinSet, time::timeout};
use tokio_rustls::TlsConnector;
use url::{Host, Url};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_AUTOCONFIG_BYTES: usize = 256 * 1024;
const MAX_CANDIDATES: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("the email address is invalid")]
    InvalidEmail,
    #[error("the manual server override is invalid")]
    InvalidOverride,
    #[error("the platform certificate store is unavailable")]
    CertificateStore,
}

/// Discover and probe implicit-TLS IMAP settings for an email address.
///
/// Discovery never authenticates and never handles a password or token. Only
/// HTTPS autoconfig endpoints owned by the email domain are queried; HTTP
/// redirects are rejected.
///
/// # Errors
/// Returns an error for an invalid address or override, or when required
/// platform networking state cannot be initialized.
pub async fn discover(
    email: &str,
    overrides: &DiscoveryOverrides,
) -> Result<AccountDiscovery, DiscoveryError> {
    let address = EmailAddress::parse(email)?;
    validate_overrides(overrides)?;

    let mut diagnostics = Vec::new();
    let mut candidates = if let Some(host) = overrides.host.as_deref() {
        vec![candidate(
            host,
            overrides.port.unwrap_or(993),
            overrides.username.as_deref().unwrap_or(&address.full),
            DiscoverySource::Manual,
            authentication_for_host(host),
        )]
    } else {
        let mut found = Vec::new();
        if let Some(preset) = provider_preset(&address) {
            found.push(preset);
        } else {
            let (autoconfig, dns) =
                tokio::join!(discover_autoconfig(&address), discover_dns(&address));
            found.extend(autoconfig.candidates);
            diagnostics.extend(autoconfig.diagnostics);
            found.extend(dns.candidates);
            diagnostics.extend(dns.diagnostics);
            found.push(candidate(
                &format!("imap.{}", address.domain),
                993,
                &address.full,
                DiscoverySource::HostGuess,
                AuthenticationKind::Password,
            ));
            found.push(candidate(
                &format!("mail.{}", address.domain),
                993,
                &address.full,
                DiscoverySource::HostGuess,
                AuthenticationKind::Password,
            ));
        }
        found
    };

    if let Some(port) = overrides.port {
        for value in &mut candidates {
            value.port = port;
            value.source = DiscoverySource::Manual;
        }
    }
    if let Some(username) = overrides.username.as_deref() {
        for value in &mut candidates {
            username.clone_into(&mut value.username);
        }
    }
    deduplicate(&mut candidates);
    candidates.truncate(MAX_CANDIDATES);

    let tls = tls_config()?;
    let probe_results = probe_candidates(&candidates, tls).await;
    let recommended = probe_results
        .iter()
        .position(|result| result.status == DiagnosticStatus::Available)
        .and_then(|index| candidates.get(index).cloned());
    diagnostics.extend(probe_results);

    Ok(AccountDiscovery {
        email: address.full,
        recommended,
        candidates,
        diagnostics,
    })
}

struct EmailAddress {
    full: String,
    local: String,
    domain: String,
}

impl EmailAddress {
    fn parse(value: &str) -> Result<Self, DiscoveryError> {
        let value = value.trim();
        let (local, domain) = value.rsplit_once('@').ok_or(DiscoveryError::InvalidEmail)?;
        if local.is_empty()
            || local.contains('@')
            || value.chars().any(char::is_whitespace)
            || value.chars().any(char::is_control)
        {
            return Err(DiscoveryError::InvalidEmail);
        }
        let Host::Domain(domain) = Host::parse(domain).map_err(|_| DiscoveryError::InvalidEmail)?
        else {
            return Err(DiscoveryError::InvalidEmail);
        };
        if !domain.contains('.') {
            return Err(DiscoveryError::InvalidEmail);
        }
        let domain = domain.trim_end_matches('.').to_ascii_lowercase();
        Ok(Self {
            full: format!("{local}@{domain}"),
            local: local.to_owned(),
            domain,
        })
    }
}

fn validate_overrides(overrides: &DiscoveryOverrides) -> Result<(), DiscoveryError> {
    if overrides.port == Some(0)
        || overrides
            .username
            .as_deref()
            .is_some_and(|username| username.trim().is_empty())
        || overrides.host.as_deref().is_some_and(|host| {
            let host = host.trim_end_matches('.');
            host.is_empty()
                || Host::parse(host).map_or(true, |parsed| !matches!(parsed, Host::Domain(_)))
        })
    {
        return Err(DiscoveryError::InvalidOverride);
    }
    Ok(())
}

fn provider_preset(address: &EmailAddress) -> Option<MailServerConfig> {
    let (host, authentication) = match address.domain.as_str() {
        "gmail.com" | "googlemail.com" => ("imap.gmail.com", AuthenticationKind::OAuth2),
        "icloud.com" | "me.com" | "mac.com" => ("imap.mail.me.com", AuthenticationKind::Password),
        "outlook.com" | "hotmail.com" | "live.com" | "msn.com" => {
            ("outlook.office365.com", AuthenticationKind::OAuth2)
        }
        "yahoo.com" | "ymail.com" | "rocketmail.com" => {
            ("imap.mail.yahoo.com", AuthenticationKind::Password)
        }
        "aol.com" => ("imap.aol.com", AuthenticationKind::Password),
        "fastmail.com" | "fastmail.fm" => ("imap.fastmail.com", AuthenticationKind::Password),
        _ => return None,
    };
    Some(candidate(
        host,
        993,
        &address.full,
        DiscoverySource::ProviderPreset,
        authentication,
    ))
}

fn authentication_for_host(host: &str) -> AuthenticationKind {
    if matches!(
        host.trim_end_matches('.').to_ascii_lowercase().as_str(),
        "imap.gmail.com" | "imap.googlemail.com" | "outlook.office365.com"
    ) {
        AuthenticationKind::OAuth2
    } else {
        AuthenticationKind::Password
    }
}

fn candidate(
    host: &str,
    port: u16,
    username: &str,
    source: DiscoverySource,
    authentication: AuthenticationKind,
) -> MailServerConfig {
    MailServerConfig {
        host: host.trim_end_matches('.').to_ascii_lowercase(),
        port,
        username: username.to_owned(),
        source,
        authentication,
    }
}

fn deduplicate(candidates: &mut Vec<MailServerConfig>) {
    let mut seen = HashSet::new();
    candidates.retain(|value| seen.insert((value.host.clone(), value.port)));
}

struct PartialDiscovery {
    candidates: Vec<MailServerConfig>,
    diagnostics: Vec<ConnectionDiagnostic>,
}

#[derive(Deserialize)]
struct XmlClientConfig {
    #[serde(rename = "emailProvider")]
    email_provider: XmlProvider,
}

#[derive(Deserialize)]
struct XmlProvider {
    #[serde(rename = "incomingServer", default)]
    incoming_servers: Vec<XmlIncomingServer>,
}

#[derive(Deserialize)]
struct XmlIncomingServer {
    #[serde(rename = "@type")]
    server_type: String,
    hostname: String,
    port: u16,
    #[serde(rename = "socketType")]
    socket_type: String,
    username: String,
    #[serde(default)]
    authentication: Vec<String>,
}

async fn discover_autoconfig(address: &EmailAddress) -> PartialDiscovery {
    let mut diagnostics = Vec::new();
    let Ok(client) = reqwest::Client::builder()
        .timeout(DISCOVERY_TIMEOUT)
        .redirect(Policy::custom(|attempt| {
            if attempt.previous().len() >= 3 {
                attempt.stop()
            } else if attempt.url().scheme() == "https" {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
    else {
        diagnostics.push(source_diagnostic(
            DiscoverySource::DomainAutoconfig,
            DiagnosticStatus::NotFound,
            "Domain autoconfig could not be initialized.",
        ));
        return PartialDiscovery {
            candidates: Vec::new(),
            diagnostics,
        };
    };

    let endpoints = [
        format!("https://autoconfig.{}/mail/config-v1.1.xml", address.domain),
        format!(
            "https://{}/.well-known/autoconfig/mail/config-v1.1.xml",
            address.domain
        ),
    ]
    .map(|endpoint| {
        Url::parse(&endpoint).ok().map(|mut url| {
            url.query_pairs_mut()
                .append_pair("emailaddress", &address.full);
            url
        })
    });

    let (first, second) = tokio::join!(
        fetch_optional_autoconfig(&client, endpoints[0].clone()),
        fetch_optional_autoconfig(&client, endpoints[1].clone())
    );
    for config in [first, second].into_iter().flatten() {
        let (candidates, unsupported) = xml_candidates(config, address);
        if unsupported {
            diagnostics.push(source_diagnostic(
                DiscoverySource::DomainAutoconfig,
                DiagnosticStatus::Unsupported,
                "The domain advertised IMAP, but not with supported implicit TLS.",
            ));
        }
        if !candidates.is_empty() {
            return PartialDiscovery {
                candidates,
                diagnostics,
            };
        }
    }
    diagnostics.push(source_diagnostic(
        DiscoverySource::DomainAutoconfig,
        DiagnosticStatus::NotFound,
        "No usable HTTPS domain autoconfig response was found.",
    ));
    PartialDiscovery {
        candidates: Vec::new(),
        diagnostics,
    }
}

async fn fetch_optional_autoconfig(
    client: &reqwest::Client,
    endpoint: Option<Url>,
) -> Option<XmlClientConfig> {
    fetch_autoconfig(client, endpoint?).await
}

async fn fetch_autoconfig(client: &reqwest::Client, endpoint: Url) -> Option<XmlClientConfig> {
    let mut response = client
        .get(endpoint)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    if response.url().scheme() != "https"
        || response
            .content_length()
            .is_some_and(|length| length > MAX_AUTOCONFIG_BYTES as u64)
    {
        return None;
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len().saturating_add(chunk.len()) > MAX_AUTOCONFIG_BYTES {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    quick_xml::de::from_reader(body.as_slice()).ok()
}

fn xml_candidates(
    config: XmlClientConfig,
    address: &EmailAddress,
) -> (Vec<MailServerConfig>, bool) {
    let mut candidates = Vec::new();
    let mut unsupported = false;
    for server in config.email_provider.incoming_servers {
        if !server.server_type.eq_ignore_ascii_case("imap") {
            continue;
        }
        if !server.socket_type.eq_ignore_ascii_case("SSL") {
            unsupported = true;
            continue;
        }
        let username = expand_username(&server.username, address);
        if server.port == 0
            || username.is_empty()
            || !matches!(Host::parse(&server.hostname), Ok(Host::Domain(_)))
        {
            continue;
        }
        let authentication = if server
            .authentication
            .iter()
            .any(|value| value.eq_ignore_ascii_case("OAuth2"))
        {
            AuthenticationKind::OAuth2
        } else {
            AuthenticationKind::Password
        };
        candidates.push(candidate(
            &server.hostname,
            server.port,
            &username,
            DiscoverySource::DomainAutoconfig,
            authentication,
        ));
    }
    (candidates, unsupported)
}

fn expand_username(template: &str, address: &EmailAddress) -> String {
    template
        .replace("%EMAILADDRESS%", &address.full)
        .replace("%EMAILLOCALPART%", &address.local)
        .replace("%EMAILDOMAIN%", &address.domain)
}

async fn discover_dns(address: &EmailAddress) -> PartialDiscovery {
    let Some(resolver) = TokioResolver::builder_tokio()
        .ok()
        .and_then(|builder| builder.build().ok())
    else {
        return PartialDiscovery {
            candidates: Vec::new(),
            diagnostics: vec![source_diagnostic(
                DiscoverySource::DnsSrv,
                DiagnosticStatus::NotFound,
                "DNS service discovery could not be initialized.",
            )],
        };
    };
    let secure_name = format!("_imaps._tcp.{}", address.domain);
    let starttls_name = format!("_imap._tcp.{}", address.domain);
    let (secure, starttls) = tokio::join!(
        resolver.srv_lookup(secure_name),
        resolver.srv_lookup(starttls_name)
    );
    let (candidates, rejected_external_target): (Vec<MailServerConfig>, bool) = secure
        .map(|lookup| {
            let records: Vec<_> = lookup
                .answers()
                .iter()
                .filter_map(|record| match &record.data {
                    RData::SRV(server) => Some(server),
                    _ => None,
                })
                .collect();
            let records = order_srv_records(records);
            let rejected_external_target = records.iter().any(|record| {
                let target = record.target.to_utf8();
                record.port != 0 && target != "." && !is_same_or_subdomain(&target, &address.domain)
            });
            let candidates = records
                .into_iter()
                .filter(|record| {
                    let target = record.target.to_utf8();
                    record.port != 0
                        && target != "."
                        && is_same_or_subdomain(&target, &address.domain)
                })
                .map(|record| {
                    candidate(
                        &record.target.to_utf8(),
                        record.port,
                        &address.full,
                        DiscoverySource::DnsSrv,
                        AuthenticationKind::Password,
                    )
                })
                .collect();
            (candidates, rejected_external_target)
        })
        .unwrap_or_default();
    let mut diagnostics = Vec::new();
    if candidates.is_empty() {
        diagnostics.push(source_diagnostic(
            DiscoverySource::DnsSrv,
            DiagnosticStatus::NotFound,
            "No usable _imaps._tcp DNS SRV record was found.",
        ));
    }
    if rejected_external_target {
        diagnostics.push(source_diagnostic(
            DiscoverySource::DnsSrv,
            DiagnosticStatus::Unsupported,
            "An external DNS SRV target was ignored because DNSSEC was not validated.",
        ));
    }
    if starttls.is_ok_and(|lookup| {
        lookup
            .answers()
            .iter()
            .any(|record| matches!(&record.data, RData::SRV(server) if server.port != 0))
    }) {
        diagnostics.push(source_diagnostic(
            DiscoverySource::DnsSrv,
            DiagnosticStatus::Unsupported,
            "The domain advertises STARTTLS IMAP, which is not yet supported.",
        ));
    }
    PartialDiscovery {
        candidates,
        diagnostics,
    }
}

fn order_srv_records(mut records: Vec<&SRV>) -> Vec<&SRV> {
    records.sort_by_key(|record| record.priority);
    let mut ordered = Vec::with_capacity(records.len());
    while let Some(priority) = records.first().map(|record| record.priority) {
        let end = records.partition_point(|record| record.priority == priority);
        let mut group: Vec<_> = records.drain(..end).collect();
        while !group.is_empty() {
            group.sort_by_key(|record| record.weight != 0);
            let total: u64 = group.iter().map(|record| u64::from(record.weight)).sum();
            let selection = rand::random_range(0..=total);
            let mut running = 0;
            let index = group
                .iter()
                .position(|record| {
                    running += u64::from(record.weight);
                    running >= selection
                })
                .unwrap_or(0);
            ordered.push(group.remove(index));
        }
    }
    ordered
}

fn is_same_or_subdomain(host: &str, domain: &str) -> bool {
    let host = host.trim_end_matches('.');
    host.eq_ignore_ascii_case(domain) || host.to_ascii_lowercase().ends_with(&format!(".{domain}"))
}

fn source_diagnostic(
    source: DiscoverySource,
    status: DiagnosticStatus,
    detail: &str,
) -> ConnectionDiagnostic {
    ConnectionDiagnostic {
        source,
        host: None,
        port: None,
        status,
        detail: detail.to_owned(),
    }
}

async fn probe_candidates(
    candidates: &[MailServerConfig],
    tls: ClientConfig,
) -> Vec<ConnectionDiagnostic> {
    let mut tasks = JoinSet::new();
    for (index, server) in candidates.iter().cloned().enumerate() {
        let tls = tls.clone();
        tasks.spawn(async move { (index, probe(&server, tls).await) });
    }
    let mut results = vec![None; candidates.len()];
    while let Some(result) = tasks.join_next().await {
        if let Ok((index, diagnostic)) = result {
            results[index] = Some(diagnostic);
        }
    }
    results
        .into_iter()
        .enumerate()
        .map(|(index, result)| {
            result.unwrap_or_else(|| {
                diagnostic_for(&candidates[index], DiagnosticStatus::Unreachable)
            })
        })
        .collect()
}

async fn probe(server: &MailServerConfig, tls: ClientConfig) -> ConnectionDiagnostic {
    let Ok(Ok(tcp)) = timeout(
        PROBE_TIMEOUT,
        TcpStream::connect((server.host.as_str(), server.port)),
    )
    .await
    else {
        return diagnostic_for(server, DiagnosticStatus::Unreachable);
    };
    let Ok(server_name) = ServerName::try_from(server.host.clone()) else {
        return diagnostic_for(server, DiagnosticStatus::TlsFailed);
    };
    let connector = TlsConnector::from(Arc::new(tls));
    let Ok(Ok(mut stream)) = timeout(PROBE_TIMEOUT, connector.connect(server_name, tcp)).await
    else {
        return diagnostic_for(server, DiagnosticStatus::TlsFailed);
    };
    let mut greeting = [0_u8; 1024];
    let Ok(Ok(count)) = timeout(PROBE_TIMEOUT, stream.read(&mut greeting)).await else {
        return diagnostic_for(server, DiagnosticStatus::InvalidResponse);
    };
    let greeting = String::from_utf8_lossy(&greeting[..count]);
    if greeting.starts_with("* OK") || greeting.starts_with("* PREAUTH") {
        diagnostic_for(server, DiagnosticStatus::Available)
    } else {
        diagnostic_for(server, DiagnosticStatus::InvalidResponse)
    }
}

fn diagnostic_for(server: &MailServerConfig, status: DiagnosticStatus) -> ConnectionDiagnostic {
    let detail = match status {
        DiagnosticStatus::Available => "Verified TLS and received an IMAP greeting.",
        DiagnosticStatus::Unreachable => "The server could not be reached before the timeout.",
        DiagnosticStatus::TlsFailed => "TLS certificate verification or negotiation failed.",
        DiagnosticStatus::InvalidResponse => "The server did not return a valid IMAP greeting.",
        DiagnosticStatus::NotFound => "No configuration was found.",
        DiagnosticStatus::Unsupported => "The discovered configuration is not supported.",
    };
    ConnectionDiagnostic {
        source: server.source,
        host: Some(server.host.clone()),
        port: Some(server.port),
        status,
        detail: detail.to_owned(),
    }
}

fn tls_config() -> Result<ClientConfig, DiscoveryError> {
    let native = rustls_native_certs::load_native_certs();
    if native.certs.is_empty() {
        return Err(DiscoveryError::CertificateStore);
    }
    let mut roots = RootCertStore::empty();
    for certificate in native.certs {
        roots
            .add(certificate)
            .map_err(|_| DiscoveryError::CertificateStore)?;
    }
    Ok(ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use super::{
        candidate, expand_username, is_same_or_subdomain, probe, provider_preset,
        validate_overrides, xml_candidates, EmailAddress, XmlClientConfig,
    };
    use mail_model::{AuthenticationKind, DiagnosticStatus, DiscoveryOverrides, DiscoverySource};
    use rcgen::generate_simple_self_signed;
    use rustls::{
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
        ClientConfig, RootCertStore, ServerConfig,
    };
    use tokio::{io::AsyncWriteExt, net::TcpListener};
    use tokio_rustls::TlsAcceptor;

    #[test]
    fn rejects_non_domain_and_ambiguous_addresses() {
        assert!(EmailAddress::parse("person@localhost").is_err());
        assert!(EmailAddress::parse("one@two@example.com").is_err());
        assert!(EmailAddress::parse(" person @example.com").is_err());
    }

    #[test]
    fn normalizes_domain_without_changing_local_part() {
        let address = EmailAddress::parse("Person@EXAMPLE.COM.").unwrap();
        assert_eq!(address.full, "Person@example.com");
        assert_eq!(address.local, "Person");
        assert_eq!(address.domain, "example.com");
    }

    #[test]
    fn known_provider_requires_expected_authentication() {
        let gmail = provider_preset(&EmailAddress::parse("me@gmail.com").unwrap()).unwrap();
        assert_eq!(gmail.host, "imap.gmail.com");
        assert_eq!(gmail.port, 993);
        assert_eq!(gmail.authentication, AuthenticationKind::OAuth2);
        assert_eq!(gmail.source, DiscoverySource::ProviderPreset);

        let icloud = provider_preset(&EmailAddress::parse("me@icloud.com").unwrap()).unwrap();
        assert_eq!(icloud.host, "imap.mail.me.com");
        assert_eq!(icloud.authentication, AuthenticationKind::Password);
    }

    #[test]
    fn validates_manual_overrides() {
        assert!(validate_overrides(&DiscoveryOverrides {
            host: Some("imap.example.com".into()),
            port: Some(1993),
            username: Some("login".into()),
        })
        .is_ok());
        assert!(validate_overrides(&DiscoveryOverrides {
            host: Some("bad host".into()),
            ..DiscoveryOverrides::default()
        })
        .is_err());
        assert!(validate_overrides(&DiscoveryOverrides {
            port: Some(0),
            ..DiscoveryOverrides::default()
        })
        .is_err());
    }

    #[test]
    fn accepts_only_same_domain_dns_targets() {
        assert!(is_same_or_subdomain("imap.example.com.", "example.com"));
        assert!(is_same_or_subdomain("example.com", "example.com"));
        assert!(!is_same_or_subdomain(
            "example.com.evil.test",
            "example.com"
        ));
        assert!(!is_same_or_subdomain("evil-example.com", "example.com"));
    }

    #[test]
    fn parses_only_implicit_tls_imap_and_expands_username() {
        let xml = br#"
            <clientConfig version="1.1"><emailProvider id="example.com">
              <incomingServer type="imap">
                <hostname>imap.example.com</hostname><port>993</port>
                <socketType>SSL</socketType><authentication>OAuth2</authentication>
                <username>%EMAILLOCALPART%</username>
              </incomingServer>
              <incomingServer type="imap">
                <hostname>imap.example.com</hostname><port>143</port>
                <socketType>STARTTLS</socketType><username>%EMAILADDRESS%</username>
              </incomingServer>
              <incomingServer type="pop3">
                <hostname>pop.example.com</hostname><port>995</port>
                <socketType>SSL</socketType><username>%EMAILADDRESS%</username>
              </incomingServer>
            </emailProvider></clientConfig>"#;
        let parsed: XmlClientConfig = quick_xml::de::from_reader(xml.as_slice()).unwrap();
        let address = EmailAddress::parse("person@example.com").unwrap();
        let (candidates, unsupported) = xml_candidates(parsed, &address);
        assert!(unsupported);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].host, "imap.example.com");
        assert_eq!(candidates[0].username, "person");
        assert_eq!(candidates[0].authentication, AuthenticationKind::OAuth2);
        assert_eq!(
            expand_username("%EMAILADDRESS%/%EMAILDOMAIN%", &address),
            "person@example.com/example.com"
        );
    }

    #[tokio::test]
    async fn probe_requires_verified_tls_and_an_imap_greeting() {
        assert_eq!(
            scripted_probe(b"* OK IMAP4 ready\r\n").await,
            DiagnosticStatus::Available
        );
        assert_eq!(
            scripted_probe(b"HTTP/1.1 200 OK\r\n").await,
            DiagnosticStatus::InvalidResponse
        );
    }

    async fn scripted_probe(greeting: &'static [u8]) -> DiagnosticStatus {
        let certified = generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let certificate = certified.cert.der().to_vec();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            certified.signing_key.serialize_der(),
        ));
        let server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(certificate.clone())], key)
            .unwrap();
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from(certificate.clone()))
            .unwrap();
        let client_config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let acceptor = TlsAcceptor::from(Arc::new(server_config));
            let mut tls = acceptor.accept(tcp).await.unwrap();
            tls.write_all(greeting).await.unwrap();
        });
        let configuration = candidate(
            "localhost",
            port,
            "person@example.com",
            DiscoverySource::Manual,
            AuthenticationKind::Password,
        );
        let status = probe(&configuration, client_config).await.status;
        server.await.unwrap();
        status
    }
}

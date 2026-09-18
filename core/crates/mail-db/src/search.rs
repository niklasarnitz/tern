use chrono::NaiveDate;

use crate::{DatabaseError, Result};

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct SearchPlan {
    pub(super) fts_query: Option<String>,
    pub(super) before: Vec<i64>,
    pub(super) after: Vec<i64>,
    pub(super) has_attachments: bool,
    pub(super) is_unread: bool,
    pub(super) is_starred: bool,
    pub(super) mailboxes: Vec<String>,
    pub(super) accounts: Vec<String>,
}

pub(super) fn parse(query: &str) -> Result<SearchPlan> {
    let mut plan = SearchPlan::default();
    let mut fts_terms = Vec::new();

    for token in tokens(query)? {
        let Some((operator, value)) = token.split_once(':') else {
            fts_terms.push(fts_phrase(&token));
            continue;
        };
        if value.is_empty() {
            return Err(invalid(format!("{operator}: requires a value")));
        }
        match operator.to_ascii_lowercase().as_str() {
            "from" => fts_terms.push(format!("sender:{}", fts_phrase(value))),
            "to" => fts_terms.push(format!("{{recipients_json cc_json}}:{}", fts_phrase(value))),
            "subject" => fts_terms.push(format!("subject:{}", fts_phrase(value))),
            "before" => plan.before.push(date_timestamp(operator, value)?),
            "after" => plan.after.push(date_timestamp(operator, value)?),
            "has" if value.eq_ignore_ascii_case("attachment") => {
                plan.has_attachments = true;
            }
            "is" if value.eq_ignore_ascii_case("unread") => plan.is_unread = true,
            "is" if value.eq_ignore_ascii_case("starred") => plan.is_starred = true,
            "in" => plan.mailboxes.push(value.to_owned()),
            "account" => plan.accounts.push(value.to_owned()),
            _ => fts_terms.push(fts_phrase(&token)),
        }
    }

    if !fts_terms.is_empty() {
        plan.fts_query = Some(fts_terms.join(" AND "));
    }
    Ok(plan)
}

fn tokens(query: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quoted = false;
    let mut escaped = false;

    for character in query.chars() {
        if escaped {
            token.push(character);
            escaped = false;
        } else if character == '\\' && quoted {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character.is_whitespace() && !quoted {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
        } else {
            token.push(character);
        }
    }
    if quoted || escaped {
        return Err(invalid("unterminated quoted search value"));
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    Ok(tokens)
}

fn date_timestamp(operator: &str, value: &str) -> Result<i64> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|date_time| date_time.and_utc().timestamp())
        .ok_or_else(|| invalid(format!("{operator}: dates must use YYYY-MM-DD")))
}

fn fts_phrase(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn invalid(message: impl Into<String>) -> DatabaseError {
    DatabaseError::InvalidSearch {
        message: message.into(),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_phrases_operators_and_flags() {
        let plan = parse(
            r#"roadmap from:"Ada Lovelace" to:team@example.test subject:"Q4 plan" before:2026-10-01 after:2026-08-31 has:attachment is:unread is:starred in:"Sent Items" account:work"#,
        )
        .expect("search should parse");

        assert_eq!(
            plan.fts_query.as_deref(),
            Some(
                r#""roadmap" AND sender:"Ada Lovelace" AND {recipients_json cc_json}:"team@example.test" AND subject:"Q4 plan""#
            )
        );
        assert_eq!(plan.before, vec![1_790_812_800]);
        assert_eq!(plan.after, vec![1_788_134_400]);
        assert!(plan.has_attachments);
        assert!(plan.is_unread);
        assert!(plan.is_starred);
        assert_eq!(plan.mailboxes, vec!["Sent Items"]);
        assert_eq!(plan.accounts, vec!["work"]);
    }

    #[test]
    fn rejects_invalid_dates_and_quotes() {
        assert!(parse("before:2026-02-30").is_err());
        assert!(parse("subject:\"unfinished").is_err());
    }
}

use std::collections::BTreeSet;

use ctx_core::ir::DatabaseAccessKind;

/// Extracts the database entities named by a static SQL statement.
///
/// This is intentionally a small deterministic recognizer, not a SQL parser.
/// It covers the common `SELECT ... FROM/JOIN`, `INSERT INTO`, `UPDATE`,
/// `DELETE FROM`, and `MERGE INTO/USING` forms. Unknown or dynamic text yields
/// no facts instead of a guessed entity.
pub(crate) fn sql_entities(statement: &str) -> Vec<(DatabaseAccessKind, String)> {
    let tokens = tokenize(statement);
    let mut accesses = BTreeSet::new();
    let mut consumed_write_from = BTreeSet::new();

    for (index, token) in tokens.iter().enumerate() {
        if keyword(token, "UPDATE") {
            insert_entity(&tokens, index + 1, DatabaseAccessKind::Write, &mut accesses);
        } else if keyword(token, "INSERT") || keyword(token, "MERGE") {
            if let Some(marker) = find_keyword(&tokens, index + 1, index + 6, "INTO") {
                insert_entity(
                    &tokens,
                    marker + 1,
                    DatabaseAccessKind::Write,
                    &mut accesses,
                );
            }
        } else if keyword(token, "DELETE")
            && let Some(marker) = find_keyword(&tokens, index + 1, index + 4, "FROM")
        {
            insert_entity(
                &tokens,
                marker + 1,
                DatabaseAccessKind::Write,
                &mut accesses,
            );
            consumed_write_from.insert(marker);
        }
    }

    for (index, token) in tokens.iter().enumerate() {
        if (keyword(token, "FROM") || keyword(token, "JOIN") || keyword(token, "USING"))
            && !consumed_write_from.contains(&index)
        {
            insert_entity(&tokens, index + 1, DatabaseAccessKind::Read, &mut accesses);
        }
    }

    accesses.into_iter().collect()
}

/// Returns the bytes inside a plain Python or Rust string literal. Interpolated
/// Python strings are rejected because their SQL is not a static fact.
pub(crate) fn static_string_content(literal: &str) -> Option<String> {
    let quote_index = literal.find(['\'', '"'])?;
    let prefix = &literal[..quote_index];
    if prefix
        .chars()
        .any(|character| matches!(character, 'f' | 'F'))
    {
        return None;
    }
    let quote = literal.as_bytes()[quote_index] as char;
    let triple = literal[quote_index..].starts_with(&quote.to_string().repeat(3));
    let opening_len = if triple { 3 } else { 1 };
    let raw_hashes = prefix.rsplit_once('r').map_or(0, |(_, suffix)| {
        suffix.chars().filter(|character| *character == '#').count()
    });
    let closing = if raw_hashes > 0 {
        format!("{quote}{}", "#".repeat(raw_hashes))
    } else {
        quote.to_string().repeat(opening_len)
    };
    let content_start = quote_index + opening_len;
    let content_end = literal.strip_suffix(&closing)?.len();
    (content_end >= content_start).then(|| literal[content_start..content_end].to_owned())
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Token {
    Word(String),
    Dot,
}

fn tokenize(statement: &str) -> Vec<Token> {
    let bytes = statement.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            b'\'' => skip_quoted(bytes, &mut index, b'\'', b'\''),
            b'"' | b'`' => {
                let delimiter = bytes[index];
                if let Some(word) = quoted_word(bytes, &mut index, delimiter, delimiter) {
                    tokens.push(Token::Word(word));
                }
            }
            b'[' => {
                if let Some(word) = quoted_word(bytes, &mut index, b'[', b']') {
                    tokens.push(Token::Word(word));
                }
            }
            b'.' => {
                tokens.push(Token::Dot);
                index += 1;
            }
            byte if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$') => {
                let start = index;
                index += 1;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'_' | b'$'))
                {
                    index += 1;
                }
                tokens.push(Token::Word(
                    String::from_utf8_lossy(&bytes[start..index]).into_owned(),
                ));
            }
            _ => index += 1,
        }
    }
    tokens
}

fn skip_quoted(bytes: &[u8], index: &mut usize, opening: u8, closing: u8) {
    debug_assert_eq!(bytes[*index], opening);
    *index += 1;
    while *index < bytes.len() {
        if bytes[*index] == closing {
            if bytes.get(*index + 1) == Some(&closing) {
                *index += 2;
            } else {
                *index += 1;
                break;
            }
        } else if bytes[*index] == b'\\' {
            *index = (*index + 2).min(bytes.len());
        } else {
            *index += 1;
        }
    }
}

fn quoted_word(bytes: &[u8], index: &mut usize, opening: u8, closing: u8) -> Option<String> {
    let start = *index + 1;
    skip_quoted(bytes, index, opening, closing);
    let end = index.checked_sub(1)?;
    (end >= start).then(|| String::from_utf8_lossy(&bytes[start..end]).into_owned())
}

fn keyword(token: &Token, expected: &str) -> bool {
    matches!(token, Token::Word(word) if word.eq_ignore_ascii_case(expected))
}

fn find_keyword(tokens: &[Token], start: usize, end: usize, expected: &str) -> Option<usize> {
    tokens
        .iter()
        .enumerate()
        .take(end.min(tokens.len()))
        .skip(start)
        .find_map(|(index, token)| keyword(token, expected).then_some(index))
}

fn insert_entity(
    tokens: &[Token],
    start: usize,
    kind: DatabaseAccessKind,
    accesses: &mut BTreeSet<(DatabaseAccessKind, String)>,
) {
    if let Some(entity) = entity_at(tokens, start) {
        accesses.insert((kind, entity));
    }
}

fn entity_at(tokens: &[Token], start: usize) -> Option<String> {
    let Token::Word(first) = tokens.get(start)? else {
        return None;
    };
    if is_clause_keyword(first) {
        return None;
    }
    let mut parts = vec![first.to_ascii_lowercase()];
    let mut index = start + 1;
    while matches!(tokens.get(index), Some(Token::Dot)) {
        let Some(Token::Word(part)) = tokens.get(index + 1) else {
            break;
        };
        parts.push(part.to_ascii_lowercase());
        index += 2;
    }
    Some(parts.join("."))
}

fn is_clause_keyword(word: &str) -> bool {
    [
        "SELECT",
        "SET",
        "WHERE",
        "ON",
        "VALUES",
        "RETURNING",
        "GROUP",
        "ORDER",
        "LIMIT",
        "OFFSET",
        "UNION",
    ]
    .iter()
    .any(|keyword| word.eq_ignore_ascii_case(keyword))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_reads_and_writes_from_common_sql_forms() {
        assert_eq!(
            sql_entities(
                "UPDATE billing.subscriptions SET status = ? FROM accounts a WHERE a.id = ?"
            ),
            vec![
                (DatabaseAccessKind::Read, "accounts".to_owned()),
                (
                    DatabaseAccessKind::Write,
                    "billing.subscriptions".to_owned()
                ),
            ]
        );
        assert_eq!(
            sql_entities("INSERT INTO audit_log(id) SELECT id FROM subscriptions"),
            vec![
                (DatabaseAccessKind::Read, "subscriptions".to_owned()),
                (DatabaseAccessKind::Write, "audit_log".to_owned()),
            ]
        );
        assert_eq!(
            sql_entities("DELETE FROM subscriptions WHERE status = 'inactive'"),
            vec![(DatabaseAccessKind::Write, "subscriptions".to_owned())]
        );
    }

    #[test]
    fn ignores_non_sql_and_unquotes_python_and_rust_literals() {
        assert!(sql_entities("not a database statement").is_empty());
        assert_eq!(
            static_string_content("r#\"SELECT * FROM subscriptions\"#").as_deref(),
            Some("SELECT * FROM subscriptions")
        );
        assert_eq!(
            static_string_content("'''UPDATE subscriptions SET status = ?'''").as_deref(),
            Some("UPDATE subscriptions SET status = ?")
        );
        assert!(static_string_content("f\"SELECT * FROM {table}\"").is_none());
    }
}

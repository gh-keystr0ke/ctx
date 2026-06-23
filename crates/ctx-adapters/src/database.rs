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

/// Returns the bytes inside a plain Python, Rust, or Go string literal.
/// Interpolated Python strings are rejected because their SQL is not a static
/// fact. Go backtick raw strings never contain escapes, so the same
/// quote-stripping logic applies to them unchanged.
pub(crate) fn static_string_content(literal: &str) -> Option<String> {
    let quote_index = literal.find(['\'', '"', '`'])?;
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

/// Recognizes the table name and column definitions from a static `CREATE
/// TABLE` or `ALTER TABLE ... ADD COLUMN` statement.
///
/// This is a conservative recognizer for common goose/Postgres/MySQL/SQLite
/// migration syntax, not a SQL dialect parser: table-level constraints
/// (`PRIMARY KEY`, `FOREIGN KEY`, `UNIQUE`, `CHECK`, `CONSTRAINT`, `INDEX`)
/// are skipped rather than misread as columns, per-column constraints are
/// left attached to the raw declared type text instead of being stripped,
/// and anything it cannot confidently locate a table name and at least one
/// column for yields `None` instead of a guessed schema.
pub(crate) fn ddl_table_columns(statement: &str) -> Option<(String, Vec<(String, String)>)> {
    let cleaned = strip_sql_comments(statement);
    let upper = cleaned.to_ascii_uppercase();
    if let Some(after_keywords) = find_keyword_pair(&upper, "CREATE", "TABLE") {
        let rest = &cleaned[after_keywords..];
        let rest = strip_word_ci(rest, "IF")
            .and_then(|rest| strip_word_ci(rest, "NOT"))
            .and_then(|rest| strip_word_ci(rest, "EXISTS"))
            .unwrap_or_else(|| rest.trim_start());
        let (name, after_name) = leading_identifier(rest)?;
        let (open, close) = matching_parens(after_name)?;
        let columns = split_top_level(&after_name[open + 1..close])
            .into_iter()
            .filter_map(parse_column_definition)
            .collect::<Vec<_>>();
        return (!columns.is_empty()).then_some((name, columns));
    }
    if let Some(after_keywords) = find_keyword_pair(&upper, "ALTER", "TABLE") {
        let rest = &cleaned[after_keywords..];
        let (name, after_name) = leading_identifier(rest)?;
        let columns = parse_add_column_clauses(after_name);
        return (!columns.is_empty()).then_some((name, columns));
    }
    None
}

fn strip_sql_comments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
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
            byte => {
                result.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&result).into_owned()
}

/// Finds `first` then, separated only by whitespace, `second` as whole words
/// in an already-uppercased haystack. Returns the byte offset right after
/// `second`.
fn find_keyword_pair(upper: &str, first: &str, second: &str) -> Option<usize> {
    let first_at = find_whole_word(upper, first, 0)?;
    let after_first = first_at + first.len();
    let second_at = find_whole_word(upper, second, after_first)?;
    upper[after_first..second_at]
        .trim()
        .is_empty()
        .then_some(second_at + second.len())
}

fn find_whole_word(haystack: &str, word: &str, from: usize) -> Option<usize> {
    let mut search_from = from;
    while let Some(relative) = haystack.get(search_from..)?.find(word) {
        let start = search_from + relative;
        let end = start + word.len();
        let before_ok = start == 0 || !is_word_byte(haystack.as_bytes()[start - 1]);
        let after_ok = end == haystack.len() || !is_word_byte(haystack.as_bytes()[end]);
        if before_ok && after_ok {
            return Some(start);
        }
        search_from = start + 1;
    }
    None
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// If `text` (after leading whitespace) begins with `word` as a whole word,
/// case-insensitively, returns the remainder with the word and any following
/// whitespace stripped.
fn strip_word_ci<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let trimmed = text.trim_start();
    if trimmed.len() < word.len() || !trimmed[..word.len()].eq_ignore_ascii_case(word) {
        return None;
    }
    let after = &trimmed[word.len()..];
    let boundary_ok = after
        .as_bytes()
        .first()
        .is_none_or(|byte| !is_word_byte(*byte));
    boundary_ok.then(|| after.trim_start())
}

/// Parses a possibly schema-qualified, possibly quoted identifier from the
/// start of `text` (for example `public.subscriptions` or `` `orders` ``).
/// Returns the lowercased, dot-joined name and the remaining text.
fn leading_identifier(text: &str) -> Option<(String, &str)> {
    let mut parts = Vec::new();
    let mut rest = text.trim_start();
    loop {
        let (part, remainder) = leading_identifier_part(rest)?;
        parts.push(part);
        if let Some(after_dot) = remainder.strip_prefix('.') {
            rest = after_dot;
        } else {
            rest = remainder;
            break;
        }
    }
    Some((parts.join(".").to_ascii_lowercase(), rest))
}

fn leading_identifier_part(text: &str) -> Option<(String, &str)> {
    let bytes = text.as_bytes();
    if matches!(bytes.first(), Some(b'"' | b'`')) {
        let quote = bytes[0];
        let mut index = 1;
        while index < bytes.len() && bytes[index] != quote {
            index += 1;
        }
        return (index < bytes.len()).then(|| (text[1..index].to_owned(), &text[index + 1..]));
    }
    let end = text
        .find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .unwrap_or(text.len());
    (end > 0).then(|| (text[..end].to_owned(), &text[end..]))
}

/// Finds the parenthesized group starting at (after leading whitespace) the
/// front of `text` and returns its open/close byte offsets, respecting
/// nested parentheses and skipping single-quoted content.
fn matching_parens(text: &str) -> Option<(usize, usize)> {
    let open = text.len() - text.trim_start().len();
    let bytes = text.as_bytes();
    if bytes.get(open) != Some(&b'(') {
        return None;
    }
    let mut depth = 0i32;
    let mut index = open;
    while index < bytes.len() {
        match bytes[index] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((open, index));
                }
            }
            b'\'' => {
                index += 1;
                while index < bytes.len() && bytes[index] != b'\'' {
                    index += 1;
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// Splits `body` on top-level commas, respecting nested parentheses and
/// quoted content so a type's own comma (`NUMERIC(10, 2)`) is not mistaken
/// for a column separator.
fn split_top_level(body: &str) -> Vec<&str> {
    let bytes = body.as_bytes();
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b'\'' | b'"' | b'`' => {
                let quote = bytes[index];
                index += 1;
                while index < bytes.len() && bytes[index] != quote {
                    index += 1;
                }
            }
            b',' if depth == 0 => {
                parts.push(&body[start..index]);
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    parts.push(&body[start..]);
    parts
}

fn first_word(text: &str) -> Option<&str> {
    let text = text.trim_start();
    if text.is_empty() {
        return None;
    }
    let end = text.find(char::is_whitespace).unwrap_or(text.len());
    Some(&text[..end])
}

fn unquote_identifier(token: &str) -> String {
    for quote in ['"', '`', '\''] {
        if token.len() >= 2 && token.starts_with(quote) && token.ends_with(quote) {
            return token[1..token.len() - 1].to_owned();
        }
    }
    token.to_owned()
}

fn parse_column_definition(chunk: &str) -> Option<(String, String)> {
    let chunk = chunk.trim();
    let name_token = first_word(chunk)?;
    if matches!(
        name_token.to_ascii_uppercase().as_str(),
        "PRIMARY" | "FOREIGN" | "UNIQUE" | "CONSTRAINT" | "CHECK" | "KEY" | "INDEX"
    ) {
        return None;
    }
    let rest = chunk[name_token.len()..].trim_start();
    let type_token = first_word(rest)?;
    let name = unquote_identifier(name_token).to_ascii_lowercase();
    (!name.is_empty()).then_some((name, type_token.to_owned()))
}

fn parse_add_column_clauses(rest: &str) -> Vec<(String, String)> {
    split_top_level(rest)
        .into_iter()
        .filter_map(|clause| {
            let after_add = strip_word_ci(clause, "ADD")?;
            let after_column = strip_word_ci(after_add, "COLUMN").unwrap_or(after_add);
            let after_if_not_exists = strip_word_ci(after_column, "IF")
                .and_then(|rest| strip_word_ci(rest, "NOT"))
                .and_then(|rest| strip_word_ci(rest, "EXISTS"))
                .unwrap_or(after_column);
            parse_column_definition(after_if_not_exists)
        })
        .collect()
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

    #[test]
    fn recognizes_backtick_raw_strings_as_static_content() {
        assert_eq!(
            static_string_content("`SELECT * FROM subscriptions`").as_deref(),
            Some("SELECT * FROM subscriptions")
        );
    }

    #[test]
    fn extracts_create_table_columns_skipping_table_constraints() {
        let (table, columns) = ddl_table_columns(
            "CREATE TABLE IF NOT EXISTS public.subscriptions (
                id UUID PRIMARY KEY,
                account_id UUID NOT NULL,
                status VARCHAR(50) NOT NULL DEFAULT 'active',
                amount NUMERIC(10, 2),
                FOREIGN KEY (account_id) REFERENCES accounts(id)
            )",
        )
        .expect("create table columns");

        assert_eq!(table, "public.subscriptions");
        assert_eq!(
            columns,
            vec![
                ("id".to_owned(), "UUID".to_owned()),
                ("account_id".to_owned(), "UUID".to_owned()),
                ("status".to_owned(), "VARCHAR(50)".to_owned()),
                ("amount".to_owned(), "NUMERIC(10,".to_owned()),
            ]
        );
    }

    #[test]
    fn extracts_alter_table_add_column_clauses() {
        let (table, columns) = ddl_table_columns(
            "ALTER TABLE subscriptions ADD COLUMN grace_period_days INT, ADD COLUMN dry_run BOOLEAN DEFAULT false",
        )
        .expect("alter table columns");

        assert_eq!(table, "subscriptions");
        assert_eq!(
            columns,
            vec![
                ("grace_period_days".to_owned(), "INT".to_owned()),
                ("dry_run".to_owned(), "BOOLEAN".to_owned()),
            ]
        );
    }

    #[test]
    fn ddl_table_columns_ignores_unrecognized_ddl_and_comments() {
        assert!(ddl_table_columns("DROP TABLE subscriptions").is_none());
        assert!(ddl_table_columns("ALTER TABLE subscriptions DROP COLUMN status").is_none());
        assert!(
            ddl_table_columns(
                "-- comment before\nCREATE TABLE empty_table (\n  /* no real columns */\n)"
            )
            .is_none()
        );
    }
}

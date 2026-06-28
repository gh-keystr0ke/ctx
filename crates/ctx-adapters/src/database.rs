use std::collections::BTreeSet;

use ctx_core::ir::{
    ColumnRename, DatabaseAccessKind, ForeignKeyRef, SchemaColumn, SchemaIndex,
    SchemaTableDefinition,
};

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

/// Recognizes one statically declared schema operation from a `CREATE
/// TABLE`, `ALTER TABLE`, `DROP TABLE`, `CREATE [UNIQUE] INDEX`, or `DROP
/// INDEX ... ON ...` statement.
///
/// This is a conservative recognizer for common goose/Postgres/MySQL/SQLite
/// migration syntax, not a SQL dialect parser. `ALTER TABLE ... ADD/DROP
/// CONSTRAINT` is deliberately unsupported (recognizing a constraint's target
/// columns without the table's already-declared column list is unreliable)
/// and yields `None` rather than a guessed partial fact. The returned
/// `SchemaTableDefinition.range` is left at its default; callers own line
/// evidence, since only the caller knows the statement's offset in its file.
pub(crate) fn parse_ddl_statement(statement: &str) -> Option<SchemaTableDefinition> {
    let cleaned = strip_sql_comments(statement);
    let upper = cleaned.to_ascii_uppercase();
    if let Some(after_keywords) = find_keyword_pair(&upper, "CREATE", "TABLE") {
        return parse_create_table(&cleaned[after_keywords..]);
    }
    if let Some(after_keywords) = find_keyword_pair(&upper, "ALTER", "TABLE") {
        return parse_alter_table(&cleaned[after_keywords..]);
    }
    if let Some(after_keywords) = find_keyword_pair(&upper, "DROP", "TABLE") {
        return parse_drop_table(&cleaned[after_keywords..]);
    }
    if let Some(parsed) = parse_create_index(&cleaned, &upper) {
        return Some(parsed);
    }
    if let Some(after_keywords) = find_keyword_pair(&upper, "DROP", "INDEX") {
        return parse_drop_index(&cleaned[after_keywords..]);
    }
    None
}

fn parse_create_table(rest: &str) -> Option<SchemaTableDefinition> {
    let rest = strip_word_ci(rest, "IF")
        .and_then(|rest| strip_word_ci(rest, "NOT"))
        .and_then(|rest| strip_word_ci(rest, "EXISTS"))
        .unwrap_or_else(|| rest.trim_start());
    let (name, after_name) = leading_identifier(rest)?;
    let (open, close) = matching_parens(after_name)?;
    let mut columns = Vec::new();
    let mut checks = Vec::new();
    for chunk in split_top_level(&after_name[open + 1..close]) {
        if let Some(column) = parse_column_definition(chunk) {
            columns.push(column);
        } else {
            apply_table_level_constraint(chunk, &mut columns, &mut checks);
        }
    }
    (!columns.is_empty() || !checks.is_empty()).then_some(SchemaTableDefinition {
        entity: name,
        table_created: true,
        columns,
        checks,
        ..SchemaTableDefinition::default()
    })
}

fn parse_alter_table(rest: &str) -> Option<SchemaTableDefinition> {
    let (name, after_name) = leading_identifier(rest)?;
    let mut table = SchemaTableDefinition {
        entity: name,
        ..SchemaTableDefinition::default()
    };
    let mut recognized_any = false;
    for clause in split_top_level(after_name) {
        let clause = clause.trim();
        if clause.is_empty() {
            continue;
        }
        if let Some(after) = strip_word_ci(clause, "ADD") {
            let after_column = strip_word_ci(after, "COLUMN").unwrap_or(after);
            let after_if_not_exists = strip_word_ci(after_column, "IF")
                .and_then(|rest| strip_word_ci(rest, "NOT"))
                .and_then(|rest| strip_word_ci(rest, "EXISTS"))
                .unwrap_or(after_column);
            if let Some(column) = parse_column_definition(after_if_not_exists) {
                table.columns.push(column);
                recognized_any = true;
            }
        } else if let Some(after) = strip_word_ci(clause, "DROP") {
            // `DROP CONSTRAINT name` is a different operation from `DROP
            // [COLUMN] name`; without the table's already-declared columns
            // there is no reliable way to know what a constraint name
            // refers to, so it is left unrecognized rather than misread as
            // dropping a column literally named `constraint`.
            if strip_word_ci(after, "CONSTRAINT").is_some() {
                continue;
            }
            let after_column = strip_word_ci(after, "COLUMN").unwrap_or(after);
            let after_if_exists = strip_word_ci(after_column, "IF")
                .and_then(|rest| strip_word_ci(rest, "EXISTS"))
                .unwrap_or(after_column);
            if let Some((name, _)) = leading_identifier(after_if_exists) {
                table.dropped_columns.push(name);
                recognized_any = true;
            }
        } else if let Some(after) = strip_word_ci(clause, "RENAME") {
            if let Some(after_to) = strip_word_ci(after, "TO") {
                if let Some((new_name, _)) = leading_identifier(after_to) {
                    table.renamed_from = Some(table.entity.clone());
                    table.entity = new_name;
                    recognized_any = true;
                }
            } else if let Some((old_name, after_old)) =
                strip_word_ci(after, "COLUMN").and_then(leading_identifier)
                && let Some((new_name, _)) =
                    strip_word_ci(after_old, "TO").and_then(leading_identifier)
            {
                table.renamed_columns.push(ColumnRename {
                    previous_name: old_name,
                    new_name,
                });
                recognized_any = true;
            }
        }
        // `ADD CONSTRAINT`/`DROP CONSTRAINT` are left unrecognized: without the
        // table's already-declared columns there is no reliable way to know
        // which columns a bare constraint name refers to.
    }
    recognized_any.then_some(table)
}

fn parse_drop_table(rest: &str) -> Option<SchemaTableDefinition> {
    let rest = strip_word_ci(rest, "IF")
        .and_then(|rest| strip_word_ci(rest, "EXISTS"))
        .unwrap_or_else(|| rest.trim_start());
    let (name, _) = leading_identifier(rest)?;
    Some(SchemaTableDefinition {
        entity: name,
        table_dropped: true,
        ..SchemaTableDefinition::default()
    })
}

fn parse_create_index(cleaned: &str, upper: &str) -> Option<SchemaTableDefinition> {
    let (after_keywords, unique) = if let Some(after) = find_keyword_pair(upper, "CREATE", "INDEX")
    {
        (after, false)
    } else {
        let first_at = find_whole_word(upper, "CREATE", 0)?;
        let after_create = first_at + "CREATE".len();
        let unique_at = find_whole_word(upper, "UNIQUE", after_create)?;
        if !upper[after_create..unique_at].trim().is_empty() {
            return None;
        }
        let after_unique = unique_at + "UNIQUE".len();
        let index_at = find_whole_word(upper, "INDEX", after_unique)?;
        if !upper[after_unique..index_at].trim().is_empty() {
            return None;
        }
        (index_at + "INDEX".len(), true)
    };
    let rest = &cleaned[after_keywords..];
    let rest = strip_word_ci(rest, "IF")
        .and_then(|rest| strip_word_ci(rest, "NOT"))
        .and_then(|rest| strip_word_ci(rest, "EXISTS"))
        .unwrap_or_else(|| rest.trim_start());
    let (index_name, after_index_name) = leading_identifier(rest)?;
    let after_on = strip_word_ci(after_index_name, "ON")?;
    let (table_name, after_table) = leading_identifier(after_on)?;
    let (open, close) = matching_parens(after_table.trim_start())?;
    let body = &after_table.trim_start()[open + 1..close];
    let columns = split_top_level(body)
        .into_iter()
        .filter_map(|chunk| leading_identifier(chunk.trim()).map(|(name, _)| name))
        .collect::<Vec<_>>();
    (!columns.is_empty()).then_some(SchemaTableDefinition {
        entity: table_name,
        indexes_added: vec![SchemaIndex {
            name: Some(index_name),
            columns,
            unique,
        }],
        ..SchemaTableDefinition::default()
    })
}

fn parse_drop_index(rest: &str) -> Option<SchemaTableDefinition> {
    let rest = strip_word_ci(rest, "IF")
        .and_then(|rest| strip_word_ci(rest, "EXISTS"))
        .unwrap_or_else(|| rest.trim_start());
    let (index_name, after_name) = leading_identifier(rest)?;
    // Only the `DROP INDEX name ON table` form names a table; the bare
    // `DROP INDEX name` form (Postgres/SQLite, where an index name alone is
    // enough) cannot be resolved to a table without already knowing every
    // index this repository has declared, so it is left unrecognized.
    let after_on = strip_word_ci(after_name, "ON")?;
    let (table_name, _) = leading_identifier(after_on)?;
    Some(SchemaTableDefinition {
        entity: table_name,
        indexes_dropped: vec![index_name],
        ..SchemaTableDefinition::default()
    })
}

/// Applies a table-level constraint clause (`PRIMARY KEY (...)`, `UNIQUE
/// (...)`, `FOREIGN KEY (...) REFERENCES ...`, `CHECK (...)`, optionally
/// prefixed with `CONSTRAINT name`) onto the columns already parsed from the
/// same `CREATE TABLE` body. A clause this function cannot confidently
/// recognize (an unsupported constraint form, or one naming a column not
/// present in `columns`) is silently skipped rather than guessed.
fn apply_table_level_constraint(
    chunk: &str,
    columns: &mut [SchemaColumn],
    checks: &mut Vec<String>,
) {
    let chunk = chunk.trim();
    let chunk = strip_word_ci(chunk, "CONSTRAINT")
        .and_then(|rest| leading_identifier(rest).map(|(_, rest)| rest.trim_start()))
        .unwrap_or(chunk);
    if let Some(after) = strip_word_ci(chunk, "PRIMARY").and_then(|rest| strip_word_ci(rest, "KEY"))
    {
        for name in constraint_column_names(after) {
            mark_column(columns, &name, |column| {
                column.primary_key = true;
                if column.nullable.is_none() {
                    column.nullable = Some(false);
                }
            });
        }
    } else if let Some(after) = strip_word_ci(chunk, "UNIQUE") {
        for name in constraint_column_names(after) {
            mark_column(columns, &name, |column| column.unique = true);
        }
    } else if let Some(after) =
        strip_word_ci(chunk, "FOREIGN").and_then(|rest| strip_word_ci(rest, "KEY"))
    {
        let Some((open, close)) = matching_parens(after.trim_start()) else {
            return;
        };
        let names = constraint_column_names_from_body(&after.trim_start()[open + 1..close]);
        let remainder = &after.trim_start()[close + 1..];
        let Some(after_references) = strip_word_ci(remainder, "REFERENCES") else {
            return;
        };
        let Some((table, after_table)) = leading_identifier(after_references) else {
            return;
        };
        let ref_column = matching_parens(after_table.trim_start()).and_then(|(open, close)| {
            leading_identifier(&after_table.trim_start()[open + 1..close]).map(|(name, _)| name)
        });
        if let Some(first_name) = names.first() {
            mark_column(columns, first_name, |column| {
                column.foreign_key = Some(ForeignKeyRef {
                    table: table.clone(),
                    column: ref_column.clone(),
                });
            });
        }
    } else if let Some(after) = strip_word_ci(chunk, "CHECK")
        && let Some((open, close)) = matching_parens(after.trim_start())
    {
        let expression = after.trim_start()[open + 1..close]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !expression.is_empty() {
            checks.push(expression);
        }
    }
}

fn constraint_column_names(after_keyword: &str) -> Vec<String> {
    matching_parens(after_keyword.trim_start())
        .map(|(open, close)| {
            constraint_column_names_from_body(&after_keyword.trim_start()[open + 1..close])
        })
        .unwrap_or_default()
}

fn constraint_column_names_from_body(body: &str) -> Vec<String> {
    split_top_level(body)
        .into_iter()
        .filter_map(|chunk| leading_identifier(chunk.trim()).map(|(name, _)| name))
        .collect()
}

fn mark_column(columns: &mut [SchemaColumn], name: &str, apply: impl FnOnce(&mut SchemaColumn)) {
    if let Some(column) = columns.iter_mut().find(|column| column.name == name) {
        apply(column);
    }
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
    // `word` is always a plain ASCII keyword, but `trimmed` is arbitrary
    // UTF-8; `get` (not `[..]`) avoids a panic when `word.len()` does not
    // land on a `trimmed` char boundary (for example a multi-byte character
    // positioned right where an ASCII keyword would start).
    if !trimmed.get(..word.len())?.eq_ignore_ascii_case(word) {
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

fn parse_column_definition(chunk: &str) -> Option<SchemaColumn> {
    let chunk = chunk.trim();
    let name_token = first_word(chunk)?;
    if matches!(
        name_token.to_ascii_uppercase().as_str(),
        "PRIMARY" | "FOREIGN" | "UNIQUE" | "CONSTRAINT" | "CHECK" | "KEY" | "INDEX"
    ) {
        return None;
    }
    let rest = chunk[name_token.len()..].trim_start();
    let (data_type, rest) = parse_column_type(rest)?;
    let name = unquote_identifier(name_token).to_ascii_lowercase();
    if name.is_empty() {
        return None;
    }
    let mut column = SchemaColumn {
        name,
        data_type,
        ..SchemaColumn::default()
    };
    apply_column_constraints(rest, &mut column);
    Some(column)
}

/// Parses a column's declared type, including a trailing parenthesized
/// argument list (`NUMERIC(10, 2)`), so an internal-comma type never gets
/// truncated at the first whitespace inside its own parentheses.
fn parse_column_type(text: &str) -> Option<(String, &str)> {
    let text = text.trim_start();
    let (base, after_base) = leading_identifier_part(text)?;
    if after_base.starts_with('(') {
        let (_, close) = matching_parens(after_base)?;
        let end = base.len() + close + 1;
        Some((text[..end].to_owned(), &text[end..]))
    } else {
        Some((base, after_base))
    }
}

fn apply_column_constraints(mut rest: &str, column: &mut SchemaColumn) {
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        if let Some(after) = strip_word_ci(rest, "NOT").and_then(|rest| strip_word_ci(rest, "NULL"))
        {
            column.nullable = Some(false);
            rest = after;
        } else if let Some(after) = strip_word_ci(rest, "NULL") {
            column.nullable = Some(true);
            rest = after;
        } else if let Some(after) =
            strip_word_ci(rest, "PRIMARY").and_then(|rest| strip_word_ci(rest, "KEY"))
        {
            column.primary_key = true;
            if column.nullable.is_none() {
                column.nullable = Some(false);
            }
            rest = after;
        } else if let Some(after) = strip_word_ci(rest, "UNIQUE") {
            column.unique = true;
            rest = after;
        } else if let Some(after) = strip_word_ci(rest, "DEFAULT") {
            let Some((value, remainder)) = parse_default_value(after) else {
                break;
            };
            column.default = Some(value);
            rest = remainder;
        } else if let Some(after) = strip_word_ci(rest, "REFERENCES") {
            let Some((table, after_table)) = leading_identifier(after) else {
                break;
            };
            let after_table = after_table.trim_start();
            if let Some((open, close)) = matching_parens(after_table) {
                let column_name =
                    leading_identifier(&after_table[open + 1..close]).map(|(name, _)| name);
                column.foreign_key = Some(ForeignKeyRef {
                    table,
                    column: column_name,
                });
                rest = &after_table[close + 1..];
            } else {
                column.foreign_key = Some(ForeignKeyRef {
                    table,
                    column: None,
                });
                rest = after_table;
            }
        } else if let Some(after) = strip_word_ci(rest, "CHECK") {
            let trimmed = after.trim_start();
            let Some((_, close)) = matching_parens(trimmed) else {
                break;
            };
            rest = &trimmed[close + 1..];
        } else {
            // An unrecognized clause (COLLATE, GENERATED ALWAYS, a
            // dialect-specific attribute, ...): stop rather than misread it,
            // keeping every constraint already parsed from earlier clauses.
            break;
        }
    }
}

/// Parses a `DEFAULT` value expression: a single-quoted string literal
/// (respecting `''` escaping), or a bare token that may itself be a function
/// call (`now()`), extended to its matching close parenthesis so an argument
/// list's internal whitespace/commas are not mistaken for the end of the
/// value.
fn parse_default_value(text: &str) -> Option<(String, &str)> {
    let text = text.trim_start();
    if text.is_empty() {
        return None;
    }
    if text.starts_with('\'') {
        let bytes = text.as_bytes();
        let mut index = 1;
        while index < bytes.len() {
            if bytes[index] == b'\'' {
                index += 1;
                if bytes.get(index) == Some(&b'\'') {
                    index += 1;
                    continue;
                }
                break;
            }
            index += 1;
        }
        return Some((text[..index].to_owned(), &text[index..]));
    }
    let mut end = text.find(char::is_whitespace).unwrap_or(text.len());
    if end == 0 {
        return None;
    }
    if let Some(paren_start) = text[..end].find('(')
        && let Some((_, close)) = matching_parens(&text[paren_start..])
    {
        end = paren_start + close + 1;
    }
    Some((text[..end].to_owned(), &text[end..]))
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

    fn column<'a>(table: &'a SchemaTableDefinition, name: &str) -> &'a SchemaColumn {
        table
            .columns
            .iter()
            .find(|column| column.name == name)
            .unwrap_or_else(|| panic!("no column named {name}"))
    }

    #[test]
    fn extracts_create_table_columns_with_inline_and_table_level_constraints() {
        let table = parse_ddl_statement(
            "CREATE TABLE IF NOT EXISTS public.subscriptions (
                id UUID PRIMARY KEY,
                account_id UUID NOT NULL,
                status VARCHAR(50) NOT NULL DEFAULT 'active',
                amount NUMERIC(10, 2),
                email VARCHAR(255) UNIQUE,
                CHECK (amount >= 0),
                FOREIGN KEY (account_id) REFERENCES accounts(id)
            )",
        )
        .expect("create table columns");

        assert_eq!(table.entity, "public.subscriptions");
        assert_eq!(
            table
                .columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["id", "account_id", "status", "amount", "email"]
        );
        let id = column(&table, "id");
        assert!(id.primary_key);
        assert_eq!(id.nullable, Some(false));
        let account_id = column(&table, "account_id");
        assert_eq!(account_id.nullable, Some(false));
        assert_eq!(
            account_id.foreign_key,
            Some(ForeignKeyRef {
                table: "accounts".to_owned(),
                column: Some("id".to_owned()),
            })
        );
        let status = column(&table, "status");
        assert_eq!(status.data_type, "VARCHAR(50)");
        assert_eq!(status.nullable, Some(false));
        assert_eq!(status.default.as_deref(), Some("'active'"));
        let amount = column(&table, "amount");
        assert_eq!(amount.data_type, "NUMERIC(10, 2)");
        assert!(column(&table, "email").unique);
        assert_eq!(table.checks, vec!["amount >= 0".to_owned()]);
    }

    #[test]
    fn extracts_alter_table_add_drop_and_rename_column_clauses() {
        let table = parse_ddl_statement(
            "ALTER TABLE subscriptions ADD COLUMN grace_period_days INT, DROP COLUMN legacy_flag, RENAME COLUMN status TO state",
        )
        .expect("alter table clauses");

        assert_eq!(table.entity, "subscriptions");
        assert_eq!(
            table
                .columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["grace_period_days"]
        );
        assert_eq!(table.dropped_columns, vec!["legacy_flag".to_owned()]);
        assert_eq!(
            table.renamed_columns,
            vec![ColumnRename {
                previous_name: "status".to_owned(),
                new_name: "state".to_owned(),
            }]
        );
    }

    #[test]
    fn extracts_alter_table_rename_to() {
        let table = parse_ddl_statement("ALTER TABLE subscriptions RENAME TO subscription_plans")
            .expect("rename to");
        assert_eq!(table.entity, "subscription_plans");
        assert_eq!(table.renamed_from.as_deref(), Some("subscriptions"));
    }

    #[test]
    fn alter_table_add_or_drop_constraint_is_unsupported_not_guessed() {
        assert!(
            parse_ddl_statement("ALTER TABLE subscriptions ADD CONSTRAINT uq_email UNIQUE (email)")
                .is_none()
        );
        assert!(
            parse_ddl_statement("ALTER TABLE subscriptions DROP CONSTRAINT uq_email").is_none()
        );
    }

    #[test]
    fn extracts_drop_table() {
        let table = parse_ddl_statement("DROP TABLE IF EXISTS subscriptions").expect("drop table");
        assert_eq!(table.entity, "subscriptions");
        assert!(table.table_dropped);
    }

    #[test]
    fn extracts_create_and_drop_index() {
        let created = parse_ddl_statement(
            "CREATE UNIQUE INDEX idx_subscriptions_email ON subscriptions (email)",
        )
        .expect("create index");
        assert_eq!(created.entity, "subscriptions");
        assert_eq!(
            created.indexes_added,
            vec![SchemaIndex {
                name: Some("idx_subscriptions_email".to_owned()),
                columns: vec!["email".to_owned()],
                unique: true,
            }]
        );

        let dropped = parse_ddl_statement("DROP INDEX idx_subscriptions_email ON subscriptions")
            .expect("drop index");
        assert_eq!(dropped.entity, "subscriptions");
        assert_eq!(
            dropped.indexes_dropped,
            vec!["idx_subscriptions_email".to_owned()]
        );

        // A bare `DROP INDEX name` (no `ON table`) cannot be resolved to a
        // table by this recognizer; it stays unrecognized instead of guessed.
        assert!(parse_ddl_statement("DROP INDEX idx_subscriptions_email").is_none());
    }

    #[test]
    fn parse_ddl_statement_never_panics_on_multi_byte_utf8_near_a_keyword_boundary() {
        // A 3-byte character positioned so an ASCII keyword's byte length
        // would slice into the middle of it must not panic; it should just
        // fail to recognize that clause instead of guessing.
        assert!(parse_ddl_statement("ALTER TABLE t xy見ADD COLUMN x INT").is_none());
        assert!(parse_ddl_statement("CREATE TABLE 見x (id INT)").is_none());
        let table =
            parse_ddl_statement("CREATE TABLE t (id INT, 見 TEXT)").expect("utf8 column name");
        assert_eq!(
            table
                .columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["id", "見"]
        );
    }

    #[test]
    fn ignores_unrecognized_ddl_and_comments() {
        assert!(parse_ddl_statement("SELECT 1").is_none());
        assert!(
            parse_ddl_statement(
                "-- comment before\nCREATE TABLE empty_table (\n  /* no real columns */\n)"
            )
            .is_none()
        );
    }
}

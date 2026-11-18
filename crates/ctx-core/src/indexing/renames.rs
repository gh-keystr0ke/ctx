use std::collections::{BTreeMap, BTreeSet};

use crate::{
    domain::StableKey,
    ir::{SymbolDefinition, SymbolKind},
};

use super::IndexedSymbol;

/// Both sides must have one candidate in the same namespace after exact names
/// are reserved. Rename matching cannot transfer an existing declaration's key.
pub(super) fn same_file_renames<'a>(
    previous: &'a [IndexedSymbol],
    current: &[SymbolDefinition],
) -> BTreeMap<String, &'a StableKey> {
    let previous_names: BTreeSet<_> = previous
        .iter()
        .map(|symbol| symbol.canonical_path.as_str())
        .collect();
    let current_names: BTreeSet<_> = current
        .iter()
        .map(|symbol| symbol.canonical_path.as_str())
        .collect();
    let removed = previous
        .iter()
        .filter(|symbol| !current_names.contains(symbol.canonical_path.as_str()));
    let added = current
        .iter()
        .filter(|symbol| !previous_names.contains(symbol.canonical_path.as_str()));
    let added: Vec<_> = added.collect();
    let mut result = BTreeMap::new();
    for old in removed {
        let mut matches = added.iter().copied().filter(|new| equivalent(old, new));
        let Some(new) = matches.next() else {
            continue;
        };
        if matches.next().is_some() {
            continue;
        }
        if previous
            .iter()
            .filter(|other| equivalent(other, new))
            .take(2)
            .count()
            == 1
            && current
                .iter()
                .filter(|other| equivalent(old, other))
                .take(2)
                .count()
                == 1
        {
            result.insert(new.canonical_path.clone(), &old.stable_key);
        }
    }
    result
}

fn equivalent(old: &IndexedSymbol, new: &SymbolDefinition) -> bool {
    matches!(
        old.kind,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Test
    ) && old.kind == new.kind
        && owner(&old.canonical_path) == owner(&new.canonical_path)
        && old.body_hash == new.body_hash
        && old.structural_fingerprint == new.structural_fingerprint
        && old
            .signature
            .as_deref()
            .zip(new.signature.as_deref())
            .is_some_and(|(left, right)| {
                normalized_signature(left, &old.name) == normalized_signature(right, &new.name)
            })
}

fn owner(name: &str) -> &str {
    name.rsplit_once('.').map_or("", |(owner, _)| owner)
}

fn normalized_signature(signature: &str, name: &str) -> String {
    let mut normalized = String::new();
    let mut start = 0;
    for (offset, character) in signature.char_indices() {
        if character.is_alphanumeric() || character == '_' {
            continue;
        }
        let token = &signature[start..offset];
        normalized.push_str(if token == name { "#" } else { token });
        if !character.is_whitespace() {
            normalized.push(character);
        }
        start = offset + character.len_utf8();
    }
    let tail = &signature[start..];
    normalized.push_str(if tail == name { "#" } else { tail });
    normalized
}

//! Git-trackable coverage state in the active context store.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use ctx_core::coverage::{CoverageRecord, DecisionAnchor};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Module {
    schema: u32,
    #[serde(rename = "module")]
    source_path: String,
    records: Vec<CoverageRecord>,
}

pub struct CoverageStore {
    directory: PathBuf,
}

impl CoverageStore {
    #[must_use]
    pub fn new(context_root: &Path) -> Self {
        Self {
            directory: context_root.join(".ctx-coverage"),
        }
    }

    /// Loads coverage records in a stable order.
    ///
    /// # Errors
    /// Returns an error for invalid files or conflicting duplicate anchors.
    pub fn read(&self) -> io::Result<BTreeMap<DecisionAnchor, CoverageRecord>> {
        let directory = match fs::read_dir(&self.directory) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(error) => return Err(error),
        };
        let mut paths = directory
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()?;
        paths.sort();
        let mut records = BTreeMap::new();
        for path in paths
            .iter()
            .filter(|path| path.extension().is_some_and(|ext| ext == "yaml"))
        {
            for record in read_module(path)?.records {
                if let Some(previous) = records.insert(record.anchor.clone(), record.clone())
                    && previous != record
                {
                    return Err(io::Error::other(format!(
                        "conflicting coverage records for {}",
                        record.anchor
                    )));
                }
            }
        }
        Ok(records)
    }

    /// Replaces one module record while preserving stable file order.
    ///
    /// # Errors
    /// Returns an error if a module file cannot be read or updated.
    pub fn record(&self, module: &str, record: CoverageRecord) -> io::Result<()> {
        let module = self
            .existing_module(&record.anchor)?
            .unwrap_or_else(|| module.to_owned());
        self.record_many(&module, [record])
    }

    fn existing_module(&self, anchor: &DecisionAnchor) -> io::Result<Option<String>> {
        let directory = match fs::read_dir(&self.directory) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        for entry in directory {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "yaml")
            {
                let module = read_module(&path)?;
                if module.records.iter().any(|record| record.anchor == *anchor) {
                    return Ok(Some(module.source_path));
                }
            }
        }
        Ok(None)
    }

    /// Updates a module's coverage records in one write.
    ///
    /// # Errors
    /// Returns an error if the module cannot be loaded or saved.
    pub fn record_many(
        &self,
        module: &str,
        records: impl IntoIterator<Item = CoverageRecord>,
    ) -> io::Result<()> {
        fs::create_dir_all(&self.directory)?;
        let path = self
            .directory
            .join(format!("{}.yaml", blake3::hash(module.as_bytes()).to_hex()));
        let mut stored = match read_module(&path) {
            Ok(stored) if stored.source_path == module => stored,
            Ok(_) => {
                return Err(io::Error::other(
                    "coverage module name does not match its file",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Module {
                schema: 1,
                source_path: module.to_owned(),
                records: Vec::new(),
            },
            Err(error) => return Err(error),
        };
        for record in records {
            match stored
                .records
                .binary_search_by(|previous| previous.anchor.cmp(&record.anchor))
            {
                Ok(index) => stored.records[index] = record,
                Err(index) => stored.records.insert(index, record),
            }
        }
        write_module(&path, &stored)
    }

    /// Merges two Git versions with their common ancestor, updating ours.
    ///
    /// # Errors
    /// Returns an error if input files differ in module identity or cannot be read.
    pub fn merge_files(ancestor: &Path, ours: &Path, theirs: &Path) -> io::Result<()> {
        let mut left = read_module(ours)?;
        let right = read_module(theirs)?;
        let original = match fs::metadata(ancestor) {
            Ok(metadata) if metadata.len() == 0 => Module {
                schema: 1,
                source_path: left.source_path.clone(),
                records: Vec::new(),
            },
            Ok(_) => read_module(ancestor)?,
            Err(error) => return Err(error),
        };
        if left.source_path != right.source_path || left.source_path != original.source_path {
            return Err(io::Error::other(
                "coverage merge inputs describe different modules",
            ));
        }
        left.records = merge_records(original.records, left.records, right.records);
        write_module(ours, &left)
    }
}

fn write_module(path: &Path, data: &Module) -> io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("coverage module has no parent"))?;
    let yaml = yaml_serde::to_string(data).map_err(io::Error::other)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(yaml.as_bytes())?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn merge_records(
    ancestor: Vec<CoverageRecord>,
    left: Vec<CoverageRecord>,
    right: Vec<CoverageRecord>,
) -> Vec<CoverageRecord> {
    use std::collections::BTreeSet;
    let by_anchor = |records: Vec<CoverageRecord>| {
        records
            .into_iter()
            .map(|record| (record.anchor.clone(), record))
            .collect::<BTreeMap<_, _>>()
    };
    let ancestor = by_anchor(ancestor);
    let left = by_anchor(left);
    let right = by_anchor(right);
    let keys: BTreeSet<_> = ancestor
        .keys()
        .chain(left.keys())
        .chain(right.keys())
        .collect();
    keys.into_iter()
        .filter_map(|key| {
            let old = ancestor.get(key);
            let ours = left.get(key);
            let theirs = right.get(key);
            if ours == old {
                return theirs.cloned();
            }
            if theirs == old || ours == theirs {
                return ours.cloned();
            }
            let (Some(ours), Some(theirs)) = (ours, theirs) else {
                return ours.or(theirs).cloned();
            };
            let mut merged = ours.clone();
            merged.state = ctx_core::coverage::GapState::Ambiguous;
            merged.citations = ours
                .citations
                .iter()
                .chain(&theirs.citations)
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            merged.document = None;
            merged.research = None;
            merged.context_documents = None;
            merged.note = Some(format!(
                "Git conflict: ours {:?} at {} ({:?}), theirs {:?} at {} ({:?})",
                ours.state, ours.revision, ours.note, theirs.state, theirs.revision, theirs.note
            ));
            Some(merged)
        })
        .collect()
}

fn read_module(path: &Path) -> io::Result<Module> {
    let text = fs::read_to_string(path)?;
    let mut parsed: Module = yaml_serde::from_str(&text)
        .map_err(|error| io::Error::other(format!("{}: {error}", path.display())))?;
    if parsed.schema != 1 {
        return Err(io::Error::other(format!(
            "{}: unsupported coverage schema",
            path.display()
        )));
    }
    parsed
        .records
        .sort_by(|left, right| left.anchor.cmp(&right.anchor));
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_core::{coverage::GapState, domain::StableKey};

    #[test]
    fn persists_sorted_records_and_updates_one_anchor() {
        let root = tempfile::tempdir().unwrap();
        let store = CoverageStore::new(root.path());
        assert!(store.read().unwrap().is_empty());
        let make_record = |ordinal, state| CoverageRecord {
            anchor: DecisionAnchor {
                symbol: StableKey::new("symbol:test").unwrap(),
                fingerprint: "tree".to_owned(),
                ordinal,
            },
            state,
            revision: "head".to_owned(),
            document: None,
            citations: Vec::new(),
            note: None,
            research: None,
            context_documents: None,
        };
        store
            .record("source.py", make_record(2, GapState::Unknown))
            .unwrap();
        store
            .record("source.py", make_record(1, GapState::Asked))
            .unwrap();
        store
            .record("source.py", make_record(2, GapState::Dismissed))
            .unwrap();
        let records = store.read().unwrap();
        assert_eq!(
            records
                .into_values()
                .map(|record| (record.anchor.ordinal, record.state))
                .collect::<Vec<_>>(),
            [(1, GapState::Asked), (2, GapState::Dismissed)]
        );
    }
}

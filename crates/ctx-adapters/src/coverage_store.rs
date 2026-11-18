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
        match stored
            .records
            .binary_search_by(|previous| previous.anchor.cmp(&record.anchor))
        {
            Ok(index) => stored.records[index] = record,
            Err(index) => stored.records.insert(index, record),
        }
        let yaml = yaml_serde::to_string(&stored).map_err(io::Error::other)?;
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        file.write_all(yaml.as_bytes())?;
        file.persist(path).map_err(|error| error.error)?;
        Ok(())
    }
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

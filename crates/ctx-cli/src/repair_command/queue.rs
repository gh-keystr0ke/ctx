use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub(super) enum Action {
    Replace {
        locator: String,
        original: String,
        symbol: String,
    },
    Remove {
        locator: String,
        original: String,
    },
    Deprecate,
    Amend {
        related_symbol: String,
        classification: String,
    },
    Merge {
        superseded: String,
        superseded_path: String,
        superseded_hash: String,
    },
    Add {
        symbol: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Status {
    Pending,
    Accepted,
    Rejected,
    Deferred,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Proposal {
    pub id: String,
    pub document: String,
    pub path: String,
    pub document_hash: String,
    pub class: String,
    pub revision: String,
    pub producer: Option<String>,
    pub model: Option<String>,
    pub reason: String,
    pub evidence: Vec<String>,
    pub action: Action,
    pub status: Status,
    pub decided_by: Option<String>,
}

pub(super) struct Queue {
    directory: PathBuf,
}

impl Queue {
    pub(super) fn new(context_root: &Path) -> Self {
        Self {
            directory: context_root.join(".ctx-candidates/repairs"),
        }
    }

    pub(super) fn enqueue(&self, proposal: &mut Proposal) -> io::Result<bool> {
        let bytes = serde_json::to_vec(&(
            &proposal.document,
            &proposal.document_hash,
            &proposal.class,
            &proposal.action,
        ))
        .map_err(io::Error::other)?;
        proposal.id = blake3::hash(&bytes).to_hex().to_string();
        let path = self.path(&proposal.id);
        if path.exists() {
            return Ok(false);
        }
        fs::create_dir_all(&self.directory)?;
        let file = self.prepare(proposal)?;
        match file.persist_noclobber(path) {
            Ok(_) => Ok(true),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error.error),
        }
    }

    pub(super) fn pending(&self) -> io::Result<Vec<Proposal>> {
        let directory = match fs::read_dir(&self.directory) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut proposals = Vec::new();
        for entry in directory {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "yaml")
            {
                let proposal: Proposal =
                    yaml_serde::from_str(&fs::read_to_string(&path)?).map_err(io::Error::other)?;
                if !valid_id(&proposal.id)
                    || path.file_stem().and_then(|name| name.to_str()) != Some(&proposal.id)
                {
                    return Err(io::Error::other("invalid repair proposal ID"));
                }
                if proposal.status == Status::Pending {
                    proposals.push(proposal);
                }
            }
        }
        proposals.sort_by(|left, right| {
            (&left.document, &left.class, &left.id).cmp(&(&right.document, &right.class, &right.id))
        });
        Ok(proposals)
    }

    pub(super) fn update(&self, proposal: &Proposal) -> io::Result<()> {
        self.prepare(proposal)?
            .persist(self.path(&proposal.id))
            .map_err(|error| error.error)?;
        Ok(())
    }

    fn prepare(&self, proposal: &Proposal) -> io::Result<tempfile::NamedTempFile> {
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        let yaml = yaml_serde::to_string(proposal).map_err(io::Error::other)?;
        file.write_all(yaml.as_bytes())?;
        Ok(file)
    }

    fn path(&self, id: &str) -> PathBuf {
        self.directory.join(format!("{id}.yaml"))
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

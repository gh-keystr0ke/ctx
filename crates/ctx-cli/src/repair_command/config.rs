use std::{fs, io};

use ctx_adapters::git::GitRepo;
use ctx_app::ports::ReviewRepository;
use serde::Deserialize;

use crate::CliError;

#[derive(Deserialize, Default)]
struct Document {
    #[serde(default)]
    repair: Config,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Config {
    auto_apply: Vec<Rule>,
    pub agent_auto: bool,
    pub max_proposals: usize,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum Rule {
    IdentityRename,
    TestRename,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            auto_apply: vec![Rule::IdentityRename, Rule::TestRename],
            agent_auto: false,
            max_proposals: 20,
        }
    }
}

impl Config {
    pub(super) fn read(git: &GitRepo) -> Result<Self, CliError> {
        match fs::read_to_string(git.root().join(".ctx/config.toml")) {
            Ok(contents) => parse(&contents),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn safe(&self, test: bool) -> bool {
        self.auto_apply.contains(&if test {
            Rule::TestRename
        } else {
            Rule::IdentityRename
        })
    }
}

pub(super) fn agent_auto(git: &GitRepo) -> Result<bool, CliError> {
    git.source_at("HEAD", ".ctx/config.toml")?
        .map_or(Ok(false), |contents| {
            parse(&contents).map(|config| config.agent_auto)
        })
}

fn parse(contents: &str) -> Result<Config, CliError> {
    let parsed: Document = toml::from_str(contents).map_err(io::Error::other)?;
    Ok(parsed.repair)
}

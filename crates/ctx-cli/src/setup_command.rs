use std::{
    io,
    path::{Path, PathBuf},
};

use clap::ValueEnum;
use serde_json::{Map, Value, json};

use crate::{
    Cli, CliError, GitRepo,
    hooks_command::{hook_path, plan_hook, read_missing, shell_quote, write_file},
};

macro_rules! skill {
    ($file:literal) => {
        include_str!(concat!("../../../.claude/skills/ctx/", $file))
    };
}

const SKILL: &[(&str, &str)] = &[
    ("SKILL.md", skill!("SKILL.md")),
    ("agents/openai.yaml", skill!("agents/openai.yaml")),
    (
        "references/authoring-context.md",
        skill!("references/authoring-context.md"),
    ),
    ("references/commands.md", skill!("references/commands.md")),
    (
        "references/federation.md",
        skill!("references/federation.md"),
    ),
    (
        "references/onboarding.md",
        skill!("references/onboarding.md"),
    ),
];

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum Agent {
    Claude,
    Codex,
    Antigravity,
}

struct Change {
    path: PathBuf,
    previous: String,
    content: String,
    executable: bool,
}

impl Change {
    fn plan(
        path: PathBuf,
        executable: bool,
        edit: impl FnOnce(&str) -> Result<String, CliError>,
    ) -> Result<Self, CliError> {
        let previous = read_missing(&path)?;
        let content = edit(&previous)?;
        Ok(Self {
            path,
            previous,
            content,
            executable,
        })
    }

    fn diff(&self) -> String {
        let path = self.path.display();
        let mut lines = vec![
            format!("--- {path}"),
            format!("+++ {path}"),
            format!(
                "@@ -{},{} +{},{} @@",
                usize::from(!self.previous.is_empty()),
                self.previous.lines().count(),
                usize::from(!self.content.is_empty()),
                self.content.lines().count(),
            ),
        ];
        lines.extend(self.previous.lines().map(|line| format!("-{line}")));
        lines.extend(self.content.lines().map(|line| format!("+{line}")));
        lines.join("\n")
    }
}

pub(crate) fn run(
    cli: &Cli,
    git: &GitRepo,
    agent: Agent,
    hooks: bool,
    dry_run: bool,
) -> Result<(), CliError> {
    let binary = std::env::current_exe()?;
    let executable = binary
        .to_str()
        .ok_or_else(|| io::Error::other("ctx executable path is not UTF-8"))?;
    let root = git.root();
    let (mcp_path, skill_path) = match agent {
        Agent::Claude => (".mcp.json", ".claude/skills/ctx"),
        Agent::Codex => (".codex/config.toml", ".agents/skills/ctx"),
        Agent::Antigravity => (".agents/mcp_config.json", ".agents/skills/ctx"),
    };
    let mut changes = vec![Change::plan(root.join(mcp_path), false, |old| {
        if matches!(agent, Agent::Codex) {
            codex_config(old, executable)
        } else {
            mcp_config(old, executable)
        }
    })?];
    for (path, text) in SKILL {
        changes.push(Change::plan(
            root.join(skill_path).join(path),
            false,
            |_| Ok((*text).to_owned()),
        )?);
    }
    if hooks {
        if matches!(agent, Agent::Claude) {
            changes.push(Change::plan(
                root.join(".claude/settings.json"),
                false,
                |old| claude_settings(old, &binary),
            )?);
        } else {
            changes.push(Change::plan(hook_path(git, "pre-commit")?, true, |old| {
                let command = format!("{} hook git-pre-commit || exit $?;", shell_quote(&binary));
                Ok(plan_hook(old, Some(&command))?)
            })?);
        }
    }
    changes.retain(|change| change.previous != change.content);
    if !dry_run {
        for change in &changes {
            write_file(&change.path, &change.content, change.executable)?;
        }
    }
    if cli.json {
        let paths = changes
            .iter()
            .map(|change| &change.path)
            .collect::<Vec<_>>();
        let diffs = dry_run.then(|| changes.iter().map(Change::diff).collect::<Vec<_>>());
        println!(
            "{}",
            json!({"dry_run": dry_run, "changed_files": paths, "diffs": diffs})
        );
    } else {
        for change in &changes {
            if dry_run {
                println!("{}", change.diff());
            } else {
                println!("{}", change.path.display());
            }
        }
    }
    Ok(())
}

fn object(value: &mut Value) -> Result<&mut Map<String, Value>, io::Error> {
    value
        .as_object_mut()
        .ok_or_else(|| io::Error::other("expected an object in agent configuration"))
}

fn parse_json(text: &str) -> Result<Value, serde_json::Error> {
    if text.trim().is_empty() {
        Ok(json!({}))
    } else {
        serde_json::from_str(text)
    }
}

fn mcp_config(old: &str, executable: &str) -> Result<String, CliError> {
    let mut config = parse_json(old)?;
    let servers = object(
        object(&mut config)?
            .entry("mcpServers")
            .or_insert_with(|| json!({})),
    )?;
    let ctx = object(servers.entry("ctx").or_insert_with(|| json!({})))?;
    ctx.insert("command".to_owned(), json!(executable));
    ctx.insert("args".to_owned(), json!(["serve", "--mcp"]));
    ctx.remove("url");
    ctx.remove("type");
    Ok(format!("{}\n", serde_json::to_string_pretty(&config)?))
}

fn codex_config(old: &str, executable: &str) -> Result<String, CliError> {
    let mut config: toml::Table = toml::from_str(old).map_err(io::Error::other)?;
    let servers = config
        .entry("mcp_servers".to_owned())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(|| io::Error::other("mcp_servers must be a table"))?;
    let ctx = servers
        .entry("ctx".to_owned())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(|| io::Error::other("mcp_servers.ctx must be a table"))?;
    ctx.insert(
        "command".to_owned(),
        toml::Value::String(executable.to_owned()),
    );
    ctx.insert(
        "args".to_owned(),
        toml::Value::Array(
            ["serve", "--mcp"]
                .map(|arg| toml::Value::String(arg.to_owned()))
                .to_vec(),
        ),
    );
    ctx.remove("url");
    toml::to_string_pretty(&config).map_err(|error| io::Error::other(error).into())
}

fn claude_settings(old: &str, binary: &Path) -> Result<String, CliError> {
    let mut config = parse_json(old)?;
    let hooks = object(
        object(&mut config)?
            .entry("hooks")
            .or_insert_with(|| json!({})),
    )?;
    let pre_tool = hooks
        .entry("PreToolUse")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| io::Error::other("PreToolUse hooks must be an array"))?;
    let command = format!("{} hook claude-pre-commit", shell_quote(binary));
    for group in &mut *pre_tool {
        if group["matcher"] == "Bash"
            && let Some(entries) = group["hooks"].as_array_mut()
            && let Some(entry) = entries.iter_mut().find(|entry| {
                entry["type"] == "command"
                    && entry["command"]
                        .as_str()
                        .is_some_and(|value| value.ends_with(" hook claude-pre-commit"))
            })
        {
            entry["command"] = json!(command);
            return Ok(format!("{}\n", serde_json::to_string_pretty(&config)?));
        }
    }
    pre_tool.push(json!({"matcher": "Bash", "hooks": [{"type": "command", "command": command}]}));
    Ok(format!("{}\n", serde_json::to_string_pretty(&config)?))
}

use std::{collections::BTreeSet, fs, path::Path, process::Command};

use serde_json::Value;
use tempfile::TempDir;

struct FixtureRepository {
    directory: TempDir,
}

struct MixedLanguageRepository {
    directory: TempDir,
}

impl MixedLanguageRepository {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary mixed repository");
        fs::create_dir_all(directory.path().join("src")).expect("source directory");
        fs::create_dir_all(directory.path().join(".ctx")).expect("ctx directory");
        fs::write(
            directory.path().join(".ctx/config.toml"),
            "languages = [\"python\", \"rust\", \"go\"]\n\n[paths]\ninclude = [\"src\"]\n",
        )
        .expect("mixed configuration");
        fs::write(
            directory.path().join("src/app.py"),
            "def run():\n    helper()\n\ndef helper():\n    return 1\n",
        )
        .expect("Python source");
        fs::write(
            directory.path().join("src/lib.rs"),
            "pub fn run() {\n    helper();\n}\n\nfn helper() -> u8 {\n    1\n}\n",
        )
        .expect("Rust source");
        fs::write(
            directory.path().join("src/main.go"),
            "package main\n\nfunc Run() {\n\thelper()\n}\n\nfunc helper() int {\n\treturn 1\n}\n",
        )
        .expect("Go source");
        run_git(directory.path(), &["init", "--quiet"]);
        run_git(directory.path(), &["config", "user.name", "ctx tests"]);
        run_git(
            directory.path(),
            &["config", "user.email", "ctx@example.invalid"],
        );
        run_git(directory.path(), &["add", "."]);
        run_git(
            directory.path(),
            &["commit", "--quiet", "-m", "mixed baseline"],
        );
        Self { directory }
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn ctx(&self, arguments: &[&str]) -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_ctx"))
            .current_dir(self.root())
            .arg("--json")
            .args(arguments)
            .output()
            .expect("execute ctx");
        assert!(
            output.status.success(),
            "ctx {} failed\nstdout: {}\nstderr: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("ctx JSON response")
    }
}

impl FixtureRepository {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary fixture repository");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("fixtures")
            .join("subscriptions");
        copy_directory(&fixture, directory.path());
        run_git(directory.path(), &["init", "--quiet"]);
        run_git(directory.path(), &["config", "user.name", "ctx tests"]);
        run_git(
            directory.path(),
            &["config", "user.email", "ctx@example.invalid"],
        );
        run_git(directory.path(), &["add", "."]);
        run_git(
            directory.path(),
            &["commit", "--quiet", "-m", "fixture baseline"],
        );
        Self { directory }
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn ctx(&self, arguments: &[&str]) -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_ctx"))
            .current_dir(self.root())
            .arg("--json")
            .args(arguments)
            .output()
            .expect("execute ctx");
        assert!(
            output.status.success(),
            "ctx {} failed\nstdout: {}\nstderr: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("ctx JSON response")
    }

    fn ctx_failure(&self, arguments: &[&str]) -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_ctx"))
            .current_dir(self.root())
            .arg("--json")
            .args(arguments)
            .output()
            .expect("execute ctx");
        assert!(!output.status.success(), "ctx unexpectedly succeeded");
        serde_json::from_slice(&output.stderr).expect("ctx JSON error")
    }

    fn ctx_text(&self, arguments: &[&str]) -> String {
        let output = Command::new(env!("CARGO_BIN_EXE_ctx"))
            .current_dir(self.root())
            .args(arguments)
            .output()
            .expect("execute ctx");
        assert!(
            output.status.success(),
            "ctx {} failed\nstdout: {}\nstderr: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("ctx text output")
    }

    fn ctx_with_env(&self, arguments: &[&str], env: &[(&str, &str)]) -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_ctx"))
            .current_dir(self.root())
            .envs(env.iter().copied())
            .arg("--json")
            .args(arguments)
            .output()
            .expect("execute ctx");
        assert!(
            output.status.success(),
            "ctx {} failed\nstdout: {}\nstderr: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("ctx JSON response")
    }

    fn introduce_entitlement_regression(&self) {
        let path = self.root().join("src/billing/subscription.py");
        let source = fs::read_to_string(&path).expect("fixture source");
        let guard = r#"        if subscription.paid_until > now:
            subscription.status = "canceling"
        else:
            subscription.status = "inactive""#;
        let replacement = r#"        subscription.status = "inactive""#;
        assert!(source.contains(guard), "fixture guard changed unexpectedly");
        fs::write(path, source.replace(guard, replacement)).expect("write harmful diff");
    }

    fn add_ignored_context(&self) {
        fs::write(self.root().join(".gitignore"), ".context/private.yaml\n")
            .expect("write fixture ignore rule");
        fs::write(
            self.root().join(".context/private.yaml"),
            "id: REQ-IGNORED\ntype: requirement\nstatement: Must not be indexed.\n",
        )
        .expect("write ignored context");
    }

    fn remove_ignored_context(&self) {
        fs::remove_file(self.root().join(".context/private.yaml")).expect("remove ignored context");
        fs::remove_file(self.root().join(".gitignore")).expect("remove fixture ignore rule");
    }
}

#[test]
fn document_visibility_is_reported_by_status_and_explain_in_json_and_text() {
    let repository = FixtureRepository::new();
    let requirement_path = repository
        .root()
        .join(".context/requirements/cancel-at-period-end.yaml");
    let requirement = fs::read_to_string(&requirement_path).expect("requirement");
    fs::write(
        &requirement_path,
        requirement.replacen("status: active", "status: active\nvisibility: public", 1),
    )
    .expect("public requirement");
    run_git(repository.root(), &["add", "."]);
    run_git(
        repository.root(),
        &["commit", "--quiet", "-m", "publish requirement"],
    );

    repository.ctx(&["init"]);
    repository.ctx(&["index"]);
    let status = repository.ctx(&["status"]);
    assert_eq!(status["knowledge"]["public_documents"], 1);

    let public = repository.ctx(&["explain", "REQ-SUB-014"]);
    assert_eq!(public["matches"][0]["subjects"][0]["visibility"], "public");
    assert!(
        repository
            .ctx_text(&["explain", "REQ-SUB-014"])
            .contains("Visibility: public")
    );

    let private = repository.ctx(&["explain", "FEAT-SUBSCRIPTIONS"]);
    assert_eq!(
        private["matches"][0]["subjects"][0]["visibility"],
        "private"
    );
    assert!(
        repository
            .ctx_text(&["explain", "FEAT-SUBSCRIPTIONS"])
            .contains("Visibility: private")
    );
}

#[test]
fn complete_product_journey_is_deterministic_and_evidence_backed() {
    let repository = FixtureRepository::new();

    let initialized = repository.ctx(&["init"]);
    assert_eq!(initialized["ok"], true);
    assert_local_database_is_ignored(&repository);
    repository.add_ignored_context();
    let ignored_context = repository.ctx_failure(&["index"]);
    assert!(
        ignored_context["error"]
            .as_str()
            .is_some_and(|error| error.contains(".context/private.yaml"))
    );
    repository.remove_ignored_context();

    let indexed = repository.ctx(&["index"]);
    assert_eq!(indexed["already_current"], false);
    assert_eq!(indexed["stats"]["files_reparsed"], 2);
    assert_eq!(indexed["business_context"]["documents_created"], 4);
    assert_eq!(indexed["business_context"]["explicit_links_created"], 7);

    let unchanged = repository.ctx(&["index"]);
    assert_eq!(unchanged["already_current"], true);
    assert_eq!(unchanged["stats"]["files_reparsed"], 0);

    assert_index_shape(&repository.ctx(&["status"]));
    assert_product_impact(
        &repository.ctx(&["impact", "billing.subscription.SubscriptionService.cancel"]),
    );
    assert_bounded_context(&repository.ctx(&[
        "context",
        "preserve paid entitlement when canceling a subscription",
        "--symbol",
        "billing.subscription.SubscriptionService.cancel",
        "--token-budget",
        "300",
    ]));

    repository.introduce_entitlement_regression();
    let refused_index = repository.ctx_failure(&["index"]);
    assert!(
        refused_index["error"]
            .as_str()
            .is_some_and(|error| error.contains("uncommitted changes"))
    );
    assert_precise_review(&repository.ctx(&["review", "--base", "HEAD"]));
}

#[test]
fn mixed_python_rust_and_go_repository_indexes_and_reviews_through_one_registry() {
    let repository = MixedLanguageRepository::new();

    repository.ctx(&["init"]);
    let indexed = repository.ctx(&["index"]);
    assert_eq!(indexed["stats"]["files_reparsed"], 3);
    assert_eq!(indexed["stats"]["nodes_created"], 9);
    assert_eq!(indexed["stats"]["edges_recomputed"], 9);

    let status = repository.ctx(&["status"]);
    assert_eq!(
        status["source_scope"]["languages"],
        json_array(&["python", "rust", "go"])
    );
    assert_eq!(status["knowledge"]["files"], 3);
    assert_eq!(status["knowledge"]["symbols"], 6);
    assert_eq!(status["knowledge"]["structural_facts"], 9);

    fs::write(
        repository.root().join("src/lib.rs"),
        "pub fn run() {\n    helper();\n    helper();\n}\n\nfn helper() -> u8 {\n    1\n}\n",
    )
    .expect("Rust change");
    fs::write(
        repository.root().join("src/main.go"),
        "package main\n\nfunc Run() {\n\thelper()\n\thelper()\n}\n\nfunc helper() int {\n\treturn 1\n}\n",
    )
    .expect("Go change");
    let review = repository.ctx(&["review", "--base", "HEAD"]);
    let changed = review["changed_entities"]
        .as_array()
        .expect("changed Rust and Go entities");
    assert!(changed.iter().any(|entity| {
        entity["stable_key"] == "symbol:rust:crate.run:Function"
            && entity["change_kind"] == "behavior_potentially_changed"
    }));
    assert!(changed.iter().any(|entity| {
        entity["stable_key"] == "symbol:go:main.Run:Function"
            && entity["change_kind"] == "behavior_potentially_changed"
    }));

    // PR-LOOKUP-002/003/004: `helper` is defined once per language in this
    // fixture, under one bare short name. Several exact matches must not be
    // an error, must not merge into one neighborhood, and must preserve
    // per-match boundaries in JSON.
    let impact = repository.ctx(&["impact", "helper"]);
    assert_eq!(impact["query"], "helper");
    let matches = impact["matches"].as_array().expect("impact matches array");
    assert_eq!(matches.len(), 3);
    let identifiers = matches
        .iter()
        .map(|report| {
            report["selected"][0]["identifier"]
                .as_str()
                .expect("selected identifier")
                .to_owned()
        })
        .collect::<BTreeSet<_>>();
    assert!(
        identifiers
            .iter()
            .all(|identifier| identifier.ends_with(".helper"))
    );
    assert_eq!(
        identifiers.len(),
        3,
        "each match stays a distinct namespace"
    );

    let find = repository.ctx(&["find", "helper"]);
    assert_eq!(find["matches"].as_array().expect("find matches").len(), 3);
}

#[test]
fn ingest_git_reads_commits_and_branches_and_links_a_referenced_ticket() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);

    run_git(
        repository.root(),
        &["branch", "feature/PAY-317-cancellation"],
    );
    fs::write(
        repository.root().join("NOTES.md"),
        "PAY-317: prepaid cancellation notes\n",
    )
    .expect("write notes file");
    run_git(repository.root(), &["add", "NOTES.md"]);
    run_git(
        repository.root(),
        &[
            "commit",
            "--quiet",
            "-m",
            "PAY-317 document cancellation behavior",
        ],
    );

    let report = repository.ctx(&["ingest", "git"]);
    // Fixture baseline commit + the new commit above; the default branch
    // plus the newly created one.
    assert_eq!(report["artifacts_ingested"], 4);

    // Re-running must stay idempotent (PR-EXT-003): no new artifacts.
    let second_report = repository.ctx(&["ingest", "git"]);
    assert_eq!(second_report["artifacts_ingested"], 4);
}

#[test]
fn ingest_gitlab_without_configuration_fails_clearly_before_any_network_call() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);

    let error = repository.ctx_failure(&["ingest", "gitlab"]);
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("invalid GitLab configuration")
                && message.contains("[gitlab]"))
    );
}

#[test]
fn ingest_rejects_an_unsupported_source() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);

    let error = repository.ctx_failure(&["ingest", "jira"]);
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("unsupported ingest source"))
    );
}

/// Writes a fake `claude` script that always proposes one grounded-evidence
/// candidate (the evidence artifact id is read out of the prompt itself, so
/// it's always valid regardless of the flag under test -- `--allow
/// -ungrounded-symbols` only ever relaxes implementation/test candidate
/// paths, never evidence grounding) naming `nonexistent/module.rs` and
/// `nonexistent/module_test.rs` as its implementation/test candidates --
/// paths that never appear in this fixture's neighborhood.
fn write_fake_claude_script_with_ungrounded_candidate_paths(script_path: &Path) {
    fs::write(
        script_path,
        "#!/bin/sh\n\
         prompt=\"$2\"\n\
         id=$(echo \"$prompt\" | grep -o 'Valid artifact ids for this neighborhood: [^ ,]*' | sed 's/.*: //')\n\
         echo \"{\\\"outcome\\\":\\\"relevant\\\",\\\"candidates\\\":[{\\\"kind\\\":\\\"requirement\\\",\\\"statement\\\":\\\"Commit history documents cancellation behavior.\\\",\\\"evidence\\\":[{\\\"artifact_id\\\":\\\"$id\\\",\\\"locator\\\":\\\"body\\\",\\\"excerpt\\\":\\\"excerpt\\\"}],\\\"implementation_candidates\\\":[\\\"nonexistent/module.rs\\\"],\\\"test_candidates\\\":[\\\"nonexistent/module_test.rs\\\"]}]}\"\n",
    )
    .expect("write fake claude script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(script_path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(script_path, permissions).expect("chmod fake claude script");
    }
}

#[test]
fn enrich_drops_ungrounded_implementation_and_test_candidates_by_default() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);
    repository.ctx(&["ingest", "git"]);

    let script_path = repository.root().join("fake-claude.sh");
    write_fake_claude_script_with_ungrounded_candidate_paths(&script_path);
    let env = [(
        "CTX_CLAUDE_CLI_BINARY",
        script_path.to_str().expect("utf8 path"),
    )];

    let enriched = repository.ctx_with_env(&["enrich", "--agent", "claude"], &env);
    assert!(enriched["candidates_proposed"].as_u64().unwrap_or(0) > 0);

    let pending = repository.ctx(&["verify", "--knowledge"]);
    let candidates = pending.as_array().expect("pending candidates");
    assert_eq!(candidates.len(), 1);
    assert!(candidates[0]["implementation_candidates"]
        .as_array()
        .expect("implementation_candidates")
        .is_empty());
    assert!(candidates[0]["test_candidates"]
        .as_array()
        .expect("test_candidates")
        .is_empty());
}

#[test]
fn enrich_allow_ungrounded_symbols_keeps_implementation_and_test_candidates_outside_the_neighborhood()
 {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);
    repository.ctx(&["ingest", "git"]);

    let script_path = repository.root().join("fake-claude.sh");
    write_fake_claude_script_with_ungrounded_candidate_paths(&script_path);
    let env = [(
        "CTX_CLAUDE_CLI_BINARY",
        script_path.to_str().expect("utf8 path"),
    )];

    let enriched = repository.ctx_with_env(
        &["enrich", "--agent", "claude", "--allow-ungrounded-symbols"],
        &env,
    );
    assert!(enriched["candidates_proposed"].as_u64().unwrap_or(0) > 0);

    let pending = repository.ctx(&["verify", "--knowledge"]);
    let candidates = pending.as_array().expect("pending candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0]["implementation_candidates"],
        serde_json::json!(["nonexistent/module.rs"])
    );
    assert_eq!(
        candidates[0]["test_candidates"],
        serde_json::json!(["nonexistent/module_test.rs"])
    );
}

/// ADR-EXT-004: the pending candidate queue lives under git-tracked
/// `.ctx-candidates/`, not only in the gitignored local `.ctx/ctx.db` --
/// so once committed, a teammate who never ran `ctx enrich` themselves
/// (simulated here by wiping the local database and starting over) still
/// sees the exact same pending candidate.
#[test]
fn a_pending_candidate_survives_losing_the_local_database_once_committed() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);
    repository.ctx(&["ingest", "git"]);

    let script_path = repository.root().join("fake-claude.sh");
    fs::write(
        &script_path,
        "#!/bin/sh\n\
         prompt=\"$2\"\n\
         id=$(echo \"$prompt\" | grep -o 'Valid artifact ids for this neighborhood: [^ ,]*' | sed 's/.*: //')\n\
         echo \"{\\\"outcome\\\":\\\"relevant\\\",\\\"candidates\\\":[{\\\"kind\\\":\\\"requirement\\\",\\\"statement\\\":\\\"Commit history documents cancellation behavior.\\\",\\\"evidence\\\":[{\\\"artifact_id\\\":\\\"$id\\\",\\\"locator\\\":\\\"body\\\",\\\"excerpt\\\":\\\"excerpt\\\"}]}]}\"\n",
    )
    .expect("write fake claude script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod fake claude script");
    }
    let env = [(
        "CTX_CLAUDE_CLI_BINARY",
        script_path.to_str().expect("utf8 path"),
    )];

    let enriched = repository.ctx_with_env(&["enrich", "--agent", "claude"], &env);
    assert!(enriched["candidates_proposed"].as_u64().unwrap_or(0) > 0);

    let queue_dir = repository.root().join(".ctx-candidates");
    let queue_files: Vec<_> = fs::read_dir(&queue_dir)
        .expect("read .ctx-candidates")
        .map(|entry| entry.expect("dir entry").path())
        .collect();
    assert_eq!(queue_files.len(), 1, "exactly one candidate file written");
    let queue_file = &queue_files[0];
    assert_eq!(
        queue_file.extension().and_then(|extension| extension.to_str()),
        Some("yaml")
    );
    let queue_file_content = fs::read_to_string(queue_file).expect("read candidate file");
    assert!(
        queue_file_content.contains("Commit history documents cancellation behavior."),
        "candidate file is plain, git-diffable YAML text: {queue_file_content}"
    );

    run_git(repository.root(), &["add", ".ctx-candidates"]);
    run_git(
        repository.root(),
        &["commit", "--quiet", "-m", "propose candidate"],
    );

    // Simulate a teammate who never ran `ctx enrich`: their `.ctx/ctx.db` never
    // saw this candidate, since it's gitignored and starts empty on any fresh
    // checkout. Deliberately drop CTX_CLAUDE_CLI_BINARY from here on so an
    // accidental re-invocation of the agent fails loudly instead of silently
    // reproducing the candidate.
    fs::remove_file(repository.root().join(".ctx/ctx.db")).expect("remove local database");
    let _ = fs::remove_file(repository.root().join(".ctx/ctx.db-wal"));
    let _ = fs::remove_file(repository.root().join(".ctx/ctx.db-shm"));
    repository.ctx(&["init"]);
    repository.ctx(&["index"]);

    let pending = repository.ctx(&["verify", "--knowledge"]);
    let candidates = pending.as_array().expect("pending candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0]["statement"],
        "Commit history documents cancellation behavior."
    );
}

#[test]
fn enrich_shells_out_to_the_configured_agent_and_reports_its_outcome() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);
    let ingested = repository.ctx(&["ingest", "git"]);
    assert!(ingested["artifacts_ingested"].as_u64().unwrap_or(0) > 0);

    let script_path = repository.root().join("fake-claude.sh");
    fs::write(
        &script_path,
        "#!/bin/sh\necho '{\"outcome\":\"not_relevant\"}'\n",
    )
    .expect("write fake claude script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod fake claude script");
    }

    let report = repository.ctx_with_env(
        &["enrich", "--agent", "claude"],
        &[(
            "CTX_CLAUDE_CLI_BINARY",
            script_path.to_str().expect("utf8 path"),
        )],
    );

    assert!(report["neighborhoods_analyzed"].as_u64().unwrap_or(0) > 0);
    assert_eq!(report["candidates_proposed"], 0);

    // Re-running must not fail even though nothing was left pending
    // (PR-AGENT-002: everything keeps working with zero candidates queued).
    let second = repository.ctx_with_env(
        &["enrich", "--agent", "claude"],
        &[(
            "CTX_CLAUDE_CLI_BINARY",
            script_path.to_str().expect("utf8 path"),
        )],
    );
    assert_eq!(second["candidates_proposed"], 0);
    assert!(repository.ctx(&["status"])["health"].as_str().is_some());
}

/// The JSON-contract behavior (dropping untrusted evidence, malformed output,
/// etc.) is already covered generically by `agent_contract`'s own tests and
/// by the Claude e2e case above -- this confirms only what's actually new
/// per agent: `--agent codex`/`--agent antigravity` dispatch to the right
/// binary via the right env-var override and invoke it with the right argv
/// shape (`codex exec <prompt>`, `agy -p <prompt>`). Each agent gets its own
/// fixture repository so the Phase 8 incremental-analysis ledger from one
/// run never skips the other's.
#[test]
fn enrich_dispatches_codex_to_codex_exec_with_its_own_binary_override() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);
    repository.ctx(&["ingest", "git"]);

    let script_path = repository.root().join("fake-codex.sh");
    fs::write(
        &script_path,
        "#!/bin/sh\nif [ \"$1\" != \"exec\" ]; then exit 1; fi\necho '{\"outcome\":\"not_relevant\"}'\n",
    )
    .expect("write fake codex script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod fake codex script");
    }

    let report = repository.ctx_with_env(
        &["enrich", "--agent", "codex"],
        &[(
            "CTX_CODEX_CLI_BINARY",
            script_path.to_str().expect("utf8 path"),
        )],
    );

    assert!(report["neighborhoods_analyzed"].as_u64().unwrap_or(0) > 0);
}

#[test]
fn enrich_dispatches_antigravity_to_agy_p_with_its_own_binary_override() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);
    repository.ctx(&["ingest", "git"]);

    let script_path = repository.root().join("fake-agy.sh");
    fs::write(
        &script_path,
        "#!/bin/sh\nif [ \"$1\" != \"-p\" ]; then exit 1; fi\necho '{\"outcome\":\"not_relevant\"}'\n",
    )
    .expect("write fake agy script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod fake agy script");
    }

    let report = repository.ctx_with_env(
        &["enrich", "--agent", "antigravity"],
        &[(
            "CTX_ANTIGRAVITY_CLI_BINARY",
            script_path.to_str().expect("utf8 path"),
        )],
    );

    assert!(report["neighborhoods_analyzed"].as_u64().unwrap_or(0) > 0);
}

#[test]
fn enrich_rejects_an_unsupported_agent() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);

    let error = repository.ctx_failure(&["enrich", "--agent", "gpt4"]);
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("unsupported agent"))
    );
}

#[test]
fn accepting_a_knowledge_candidate_writes_a_context_document_ctx_index_then_absorbs() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);
    repository.ctx(&["ingest", "git"]);

    let script_path = repository.root().join("fake-claude.sh");
    fs::write(
        &script_path,
        "#!/bin/sh\n\
         prompt=\"$2\"\n\
         id=$(echo \"$prompt\" | grep -o 'Valid artifact ids for this neighborhood: [^ ,]*' | sed 's/.*: //')\n\
         echo \"{\\\"outcome\\\":\\\"relevant\\\",\\\"candidates\\\":[{\\\"kind\\\":\\\"requirement\\\",\\\"statement\\\":\\\"Commit history documents cancellation behavior.\\\",\\\"evidence\\\":[{\\\"artifact_id\\\":\\\"$id\\\",\\\"locator\\\":\\\"body\\\",\\\"excerpt\\\":\\\"excerpt\\\"}]}]}\"\n",
    )
    .expect("write fake claude script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod fake claude script");
    }
    let env = [(
        "CTX_CLAUDE_CLI_BINARY",
        script_path.to_str().expect("utf8 path"),
    )];

    let enriched = repository.ctx_with_env(&["enrich", "--agent", "claude"], &env);
    assert!(enriched["candidates_proposed"].as_u64().unwrap_or(0) > 0);

    let pending = repository.ctx(&["verify", "--knowledge"]);
    let candidates = pending.as_array().expect("pending candidates");
    assert_eq!(candidates.len(), 1);
    let fingerprint = candidates[0]["fingerprint"]
        .as_str()
        .expect("fingerprint")
        .to_owned();

    // --knowledge --accept without --id is refused rather than allocating a
    // guessed one.
    let missing_id = repository.ctx_failure(&["verify", "--knowledge", "--accept", &fingerprint]);
    assert!(
        missing_id["error"]
            .as_str()
            .is_some_and(|message| message.contains("--id"))
    );

    let accepted = repository.ctx(&[
        "verify",
        "--knowledge",
        "--accept",
        &fingerprint,
        "--id",
        "REQ-COMMIT-DOC-001",
    ]);
    assert_eq!(accepted["ok"], true);
    let written_path = accepted["path"].as_str().expect("written path").to_owned();
    assert!(repository.root().join(&written_path).exists());

    // No longer pending once decided.
    let after_accept = repository.ctx(&["verify", "--knowledge"]);
    assert!(
        after_accept
            .as_array()
            .expect("pending candidates")
            .is_empty()
    );

    // The next `ctx index` absorbs it exactly like any hand-authored
    // `.context/*.yaml` document -- no second, parallel truth store. Written
    // files are ordinary working-tree content, so (per this repo's own
    // committed-inputs invariant) they must be committed before indexing.
    run_git(repository.root(), &["add", &written_path]);
    run_git(
        repository.root(),
        &["commit", "--quiet", "-m", "accept REQ-COMMIT-DOC-001"],
    );
    let indexed = repository.ctx(&["index"]);
    assert!(
        indexed["business_context"]["documents_created"]
            .as_u64()
            .unwrap_or(0)
            > 0
    );
}

#[test]
fn accepting_a_restated_requirement_is_refused_unless_forced() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);
    repository.ctx(&["index"]);
    repository.ctx(&["ingest", "git"]);

    let script_path = repository.root().join("fake-claude.sh");
    fs::write(
        &script_path,
        "#!/bin/sh\n\
         prompt=\"$2\"\n\
         id=$(echo \"$prompt\" | grep -o 'Valid artifact ids for this neighborhood: [^ ,]*' | sed 's/.*: //')\n\
         echo \"{\\\"outcome\\\":\\\"relevant\\\",\\\"candidates\\\":[{\\\"kind\\\":\\\"requirement\\\",\\\"statement\\\":\\\"When a paid user cancels, access must remain active until paid_until.\\\",\\\"evidence\\\":[{\\\"artifact_id\\\":\\\"$id\\\",\\\"locator\\\":\\\"body\\\",\\\"excerpt\\\":\\\"excerpt\\\"}]}]}\"\n",
    )
    .expect("write fake claude script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod fake claude script");
    }
    let env = [(
        "CTX_CLAUDE_CLI_BINARY",
        script_path.to_str().expect("utf8 path"),
    )];

    repository.ctx_with_env(&["enrich", "--agent", "claude"], &env);
    let pending = repository.ctx(&["verify", "--knowledge"]);
    let fingerprint = pending.as_array().expect("pending candidates")[0]["fingerprint"]
        .as_str()
        .expect("fingerprint")
        .to_owned();

    let refused = repository.ctx_failure(&[
        "verify",
        "--knowledge",
        "--accept",
        &fingerprint,
        "--id",
        "REQ-SUB-099",
    ]);
    assert!(
        refused["error"]
            .as_str()
            .is_some_and(|message| message.contains("REQ-SUB-014") && message.contains("force"))
    );
    // Refusing must not write anything.
    assert!(
        !repository
            .root()
            .join(".context/requirements/req-sub-099.yaml")
            .exists()
    );

    let forced = repository.ctx(&[
        "verify",
        "--knowledge",
        "--accept",
        &fingerprint,
        "--id",
        "REQ-SUB-099",
        "--force",
    ]);
    assert_eq!(forced["ok"], true);
    assert!(
        repository
            .root()
            .join(forced["path"].as_str().expect("path"))
            .exists()
    );
}

struct CodeCommentRepository {
    directory: TempDir,
}

impl CodeCommentRepository {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary code-comment repository");
        fs::create_dir_all(directory.path().join("src")).expect("source directory");
        fs::create_dir_all(directory.path().join(".ctx")).expect("ctx directory");
        fs::write(
            directory.path().join(".ctx/config.toml"),
            "languages = [\"python\"]\n\n[paths]\ninclude = [\"src\"]\n",
        )
        .expect("configuration");
        fs::write(
            directory.path().join("src/billing.py"),
            "def cancel():\n    # Keep access until paid_until because the current\n    # period has already been paid for.\n    pass\n",
        )
        .expect("Python source with a doc comment");
        run_git(directory.path(), &["init", "--quiet"]);
        run_git(directory.path(), &["config", "user.name", "ctx tests"]);
        run_git(
            directory.path(),
            &["config", "user.email", "ctx@example.invalid"],
        );
        run_git(directory.path(), &["add", "."]);
        run_git(
            directory.path(),
            &["commit", "--quiet", "-m", "code comment baseline"],
        );
        Self { directory }
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn ctx(&self, arguments: &[&str]) -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_ctx"))
            .current_dir(self.root())
            .arg("--json")
            .args(arguments)
            .output()
            .expect("execute ctx");
        assert!(
            output.status.success(),
            "ctx {} failed\nstdout: {}\nstderr: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("ctx JSON response")
    }
}

#[test]
fn ingest_code_comments_attaches_a_doc_comment_to_its_nearest_symbol() {
    let repository = CodeCommentRepository::new();
    repository.ctx(&["init"]);
    repository.ctx(&["index"]);

    let report = repository.ctx(&["ingest", "code-comments"]);
    assert_eq!(report["artifacts_ingested"], 1);
    assert_eq!(report["links_created"], 1);

    // Idempotent re-ingestion must not duplicate the artifact/link.
    let second_report = repository.ctx(&["ingest", "code-comments"]);
    assert_eq!(second_report["artifacts_ingested"], 1);
    assert_eq!(second_report["links_created"], 1);
}

struct PartiallyBrokenRustRepository {
    directory: TempDir,
}

impl PartiallyBrokenRustRepository {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary partially broken repository");
        fs::create_dir_all(directory.path().join("src")).expect("source directory");
        fs::create_dir_all(directory.path().join(".ctx")).expect("ctx directory");
        fs::write(
            directory.path().join(".ctx/config.toml"),
            "languages = [\"rust\"]\n\n[paths]\ninclude = [\"src\"]\n",
        )
        .expect("configuration");
        fs::write(
            directory.path().join("src/good.rs"),
            "pub fn run() -> u8 {\n    1\n}\n",
        )
        .expect("valid Rust source");
        fs::write(directory.path().join("src/broken.rs"), "fn broken(\n")
            .expect("invalid Rust source");
        run_git(directory.path(), &["init", "--quiet"]);
        run_git(directory.path(), &["config", "user.name", "ctx tests"]);
        run_git(
            directory.path(),
            &["config", "user.email", "ctx@example.invalid"],
        );
        run_git(directory.path(), &["add", "."]);
        run_git(
            directory.path(),
            &["commit", "--quiet", "-m", "partially broken baseline"],
        );
        Self { directory }
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn ctx(&self, arguments: &[&str]) -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_ctx"))
            .current_dir(self.root())
            .arg("--json")
            .args(arguments)
            .output()
            .expect("execute ctx");
        assert!(
            output.status.success(),
            "ctx {} failed\nstdout: {}\nstderr: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("ctx JSON response")
    }
}

#[test]
fn one_file_failing_analysis_does_not_block_indexing_the_rest() {
    let repository = PartiallyBrokenRustRepository::new();

    repository.ctx(&["init"]);
    let indexed = repository.ctx(&["index"]);

    assert_eq!(indexed["stats"]["files_reparsed"], 1);
    let failed = indexed["failed_files"]
        .as_array()
        .expect("failed_files array");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["path"], "src/broken.rs");

    let status = repository.ctx(&["status"]);
    assert_eq!(status["knowledge"]["files"], 1);
    assert_eq!(status["knowledge"]["symbols"], 1);

    let reindexed = repository.ctx(&["index"]);
    assert_eq!(reindexed["stats"]["files_reparsed"], 0);
    let still_failed = reindexed["failed_files"]
        .as_array()
        .expect("failed_files array");
    assert_eq!(still_failed.len(), 1);
    assert_eq!(still_failed[0]["path"], "src/broken.rs");
}

fn json_array(values: &[&str]) -> Value {
    Value::Array(
        values
            .iter()
            .map(|value| Value::String((*value).to_owned()))
            .collect(),
    )
}

fn assert_local_database_is_ignored(repository: &FixtureRepository) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository.root())
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output()
        .expect("inspect Git status");
    let status = String::from_utf8(output.stdout).expect("Git status UTF-8");
    assert_eq!(status, "?? .ctx/config.toml\n");
}

fn assert_index_shape(status: &Value) {
    assert_eq!(status["health"], "ready");
    assert_eq!(status["index_state"], "current");
    assert_eq!(status["knowledge"]["files"], 2);
    assert_eq!(status["knowledge"]["symbols"], 6);
    assert_eq!(status["knowledge"]["db_entities"], 1);
    assert_eq!(status["knowledge"]["public_documents"], 0);
    assert_eq!(status["knowledge"]["structural_facts"], 12);
    assert_eq!(status["knowledge"]["active_assertions"], 7);
    assert_eq!(status["knowledge"]["active_edges"], 19);
    assert_eq!(status["knowledge"]["stale_semantic_edges"], 0);
}

fn assert_product_impact(impact: &Value) {
    let serialized = impact.to_string();
    for expected in [
        "FEAT-SUBSCRIPTIONS",
        "REQ-SUB-014",
        "INV-SUB-003",
        "subscriptions",
        "test_cancel_keeps_access_until_paid_until",
    ] {
        assert!(serialized.contains(expected), "impact omitted {expected}");
    }
}

fn assert_bounded_context(context: &Value) {
    assert_eq!(context["token_budget"], 300);
    assert!(
        context["estimated_tokens"]
            .as_u64()
            .is_some_and(|tokens| tokens <= 300)
    );
    let serialized = context.to_string();
    assert!(serialized.contains("INV-SUB-003"));
    assert!(serialized.contains("REQ-SUB-014"));
}

fn assert_precise_review(review: &Value) {
    let findings = review["findings"].as_array().expect("review findings");
    // `cancel`'s own body regressed: two direct, high-severity findings on
    // the invariant and requirement it implements. `cancel` is also called,
    // one hop away, by `StripeWebhookHandler.handle_subscription_update`,
    // which implements ADR-SUB-001 — the bounded call-graph escalation
    // (`ctx_core::review::indirect_call_findings`) correctly surfaces that
    // as a third, indirect, medium-severity finding, distinct from the two
    // direct ones since `handle_subscription_update` itself never changed.
    assert_eq!(findings.len(), 3);
    let intent_ids = findings
        .iter()
        .filter_map(|finding| finding["affected_intent"]["identifier"].as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        intent_ids,
        BTreeSet::from(["INV-SUB-003", "REQ-SUB-014", "ADR-SUB-001"])
    );
    assert!(findings.iter().all(|finding| {
        finding["tests_modified"] == false
            && finding["evidence"]
                .as_array()
                .is_some_and(|evidence| !evidence.is_empty())
    }));
    let direct = findings
        .iter()
        .filter(|finding| finding["affected_intent"]["identifier"] != "ADR-SUB-001");
    assert_eq!(direct.clone().count(), 2);
    assert!(
        direct
            .clone()
            .all(|finding| { finding["severity"] == "high" && finding["uncertainty"].is_null() })
    );
    let indirect = findings
        .iter()
        .find(|finding| finding["affected_intent"]["identifier"] == "ADR-SUB-001")
        .expect("indirect ADR-SUB-001 finding");
    assert_eq!(indirect["severity"], "medium");
    assert_eq!(
        indirect["changed_entity"],
        "billing.subscription.StripeWebhookHandler.handle_subscription_update"
    );
    assert!(
        indirect["uncertainty"]
            .as_str()
            .is_some_and(|text| text.contains("cancel"))
    );
}

#[test]
fn verify_knowledge_auto_reviews_and_accepts_with_an_honest_agent_decision_method() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);
    repository.ctx(&["ingest", "git"]);

    // One script plays both roles a real agent CLI would across the two
    // calls this test makes: extraction (`ctx enrich`) and independent
    // review (`ctx verify --knowledge --auto`) use genuinely different
    // system prompts, so the fake script tells them apart the same way a
    // human skimming stdin would -- by which prompt it was actually asked.
    let script_path = repository.root().join("fake-claude.sh");
    fs::write(
        &script_path,
        "#!/bin/sh\n\
         prompt=\"$2\"\n\
         if echo \"$prompt\" | grep -q 'second-opinion reviewer'; then\n\
         fp=$(echo \"$prompt\" | grep '^- fingerprint:' | head -1 | sed 's/^- fingerprint: //')\n\
         echo \"{\\\"decisions\\\":[{\\\"fingerprint\\\":\\\"$fp\\\",\\\"verdict\\\":\\\"accept\\\"}]}\"\n\
         else\n\
         id=$(echo \"$prompt\" | grep -o 'Valid artifact ids for this neighborhood: [^ ,]*' | sed 's/.*: //')\n\
         echo \"{\\\"outcome\\\":\\\"relevant\\\",\\\"candidates\\\":[{\\\"kind\\\":\\\"requirement\\\",\\\"statement\\\":\\\"Commit history documents cancellation behavior.\\\",\\\"evidence\\\":[{\\\"artifact_id\\\":\\\"$id\\\",\\\"locator\\\":\\\"body\\\",\\\"excerpt\\\":\\\"excerpt\\\"}]}]}\"\n\
         fi\n",
    )
    .expect("write fake claude script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod fake claude script");
    }
    let env = [(
        "CTX_CLAUDE_CLI_BINARY",
        script_path.to_str().expect("utf8 path"),
    )];

    let enriched = repository.ctx_with_env(&["enrich", "--agent", "claude"], &env);
    assert!(enriched["candidates_proposed"].as_u64().unwrap_or(0) > 0);
    assert_eq!(
        repository
            .ctx(&["verify", "--knowledge"])
            .as_array()
            .expect("pending candidates")
            .len(),
        1
    );

    let report = repository.ctx_with_env(
        &[
            "verify",
            "--knowledge",
            "--auto",
            "--agent",
            "claude",
            "--id-prefix",
            "SUB",
        ],
        &env,
    );

    assert_eq!(report["clusters_reviewed"], 1);
    assert_eq!(report["documents_written"], 1);
    assert_eq!(report["candidates_accepted"], 1);
    assert_eq!(report["candidates_rejected"], 0);

    // Decided, so no longer pending -- and the resulting document exists
    // under the auto-allocated ID.
    assert!(
        repository
            .ctx(&["verify", "--knowledge"])
            .as_array()
            .expect("pending candidates")
            .is_empty()
    );
    let written_path = repository
        .root()
        .join(".context/requirements/req-sub-001.yaml");
    assert!(written_path.exists());
    let written = fs::read_to_string(&written_path).expect("read written document");
    assert!(written.contains("Commit history documents cancellation behavior."));
}

fn run_git(root: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .expect("execute git");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn copy_directory(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source).expect("read fixture directory") {
        let entry = entry.expect("fixture entry");
        let destination_path = destination.join(entry.file_name());
        if entry.file_type().expect("fixture entry type").is_dir() {
            fs::create_dir_all(&destination_path).expect("create fixture directory");
            copy_directory(&entry.path(), &destination_path);
        } else {
            fs::copy(entry.path(), destination_path).expect("copy fixture file");
        }
    }
}

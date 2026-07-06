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

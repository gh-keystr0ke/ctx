use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    process::Command,
};

use ctx_adapters::{git::GitRepo, sqlite::SqliteStore};
use ctx_app::ports::{ArtifactLinkStore, ArtifactRepository, GitRepository, GraphStore};
use ctx_core::{
    artifact::{
        Artifact, ArtifactIdentity, ArtifactKind, ArtifactLink, ArtifactLinkKind,
        ArtifactLinkTarget, ArtifactProvider,
    },
    domain::{Project, Url},
};
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
fn verbosity_keeps_json_stdout_parseable_and_reports_command_lifecycle() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);
    repository.ctx(&["index"]);

    let run = |level: &str| {
        Command::new(env!("CARGO_BIN_EXE_ctx"))
            .current_dir(repository.root())
            .args(["--json", level, "status"])
            .output()
            .expect("execute verbose ctx")
    };
    let output = run("-v");
    assert!(output.status.success());
    serde_json::from_slice::<Value>(&output.stdout).expect("parse JSON stdout");
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 stderr");
    assert!(stderr.contains("command started"));
    assert!(!stderr.contains("SQLite store opening"));

    let output = run("-vv");
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 stderr");
    assert!(stderr.contains("SQLite store opening"));
    assert!(stderr.contains("git command started"));
    assert!(!stderr.contains("git command completed"));

    let output = run("-vvv");
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 stderr");
    assert!(stderr.contains("git command completed"));
}

#[test]
fn ingest_reports_progress_at_the_first_verbosity_level() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);
    let output = Command::new(env!("CARGO_BIN_EXE_ctx"))
        .current_dir(repository.root())
        .args(["--json", "-v", "ingest", "git"])
        .output()
        .expect("execute verbose ingest");

    assert!(output.status.success());
    serde_json::from_slice::<Value>(&output.stdout).expect("parse JSON stdout");
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 stderr");
    assert!(stderr.contains("ingest started"));
    assert!(stderr.contains("ingest completed"));
}

#[test]
fn missing_pyright_type_server_is_a_successful_graph_noop() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);
    repository.ctx(&["index"]);
    let before = repository.ctx(&["status"]);

    let inferred = repository.ctx(&[
        "infer-types",
        "--pyright",
        "/definitely/missing/pyright-typeserver",
    ]);

    assert_eq!(inferred["ok"], true);
    assert_eq!(inferred["status"], "skipped");
    assert_eq!(inferred["reason"], "pyright_typeserver_not_found");
    assert_eq!(repository.ctx(&["status"]), before);
}

#[test]
fn debug_writes_trace_jsonl_and_keeps_it_out_of_git() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);
    repository.ctx(&["index"]);

    let output = Command::new(env!("CARGO_BIN_EXE_ctx"))
        .current_dir(repository.root())
        .args(["--json", "status", "--debug"])
        .output()
        .expect("execute debug ctx");

    assert!(output.status.success());
    serde_json::from_slice::<Value>(&output.stdout).expect("parse JSON stdout");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Debug log:"));
    let files = fs::read_dir(repository.root().join(".ctx/logs"))
        .expect("debug log directory")
        .map(|entry| entry.expect("debug log entry").path())
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 1);
    let log = fs::read_to_string(&files[0]).expect("read debug log");
    assert!(log.contains("command started"));
    assert!(log.contains("command completed"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&files[0])
                .expect("debug log metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let status = Command::new("git")
        .current_dir(repository.root())
        .args(["status", "--short"])
        .output()
        .expect("git status");
    assert!(status.status.success());
    assert!(!String::from_utf8_lossy(&status.stdout).contains(".ctx/logs"));
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
fn markdown_report_is_byte_deterministic_across_the_complete_output_tree() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);
    repository.ctx(&["index"]);
    let output_root = tempfile::tempdir().expect("report output root");
    let first = output_root.path().join("first");
    let second = output_root.path().join("second");

    let first_result = repository.ctx(&[
        "report",
        "markdown",
        "--out",
        first.to_str().expect("UTF-8 path"),
    ]);
    repository.ctx(&[
        "report",
        "markdown",
        "--out",
        second.to_str().expect("UTF-8 path"),
    ]);

    assert_eq!(first_result["format"], "markdown");
    assert_eq!(
        read_report_tree(&first),
        read_report_tree(&second),
        "same-commit reports must be byte-stable across every generated file"
    );
    let index = fs::read_to_string(first.join("index.md")).expect("Markdown dashboard");
    assert!(index.contains("# Context dashboard"));
    assert!(index.contains("[Source tree](tree.md)"));
    let exclude = fs::read_to_string(repository.root().join(".git/info/exclude"))
        .expect("repository-local excludes");
    assert!(exclude.lines().any(|line| line == ".ctx/report/"));
}

#[test]
fn html_report_links_external_evidence_and_code_at_the_source_commit() {
    let repository = FixtureRepository::new();
    run_git(
        repository.root(),
        &[
            "remote",
            "add",
            "origin",
            "https://gitlab.example.com/payments/subscriptions.git",
        ],
    );
    repository.ctx(&["init"]);
    repository.ctx(&["index"]);

    let git = GitRepo::discover(repository.root()).expect("Git repository");
    let mut store = SqliteStore::open(&repository.root().join(".ctx/ctx.db"), git.context_root())
        .expect("store");
    let descriptor = git.descriptor().expect("descriptor");
    let graph = store.load_graph(&descriptor.id).expect("graph");
    let symbol = graph
        .resolve("billing.subscription.SubscriptionService.cancel")
        .into_iter()
        .next()
        .expect("cancel symbol");
    let artifact = Artifact {
        identity: ArtifactIdentity {
            provider: ArtifactProvider::Jira,
            kind: ArtifactKind::Issue,
            external_id: "PAY-317".to_owned(),
        },
        project: Project("PAY".to_owned()),
        title: "Keep paid access during cancellation".to_owned(),
        body: "Cancellation retains prepaid entitlement.".to_owned(),
        author: Some("product-team".to_owned()),
        external_created_at: None,
        external_updated_at: None,
        source_locator: Url("https://jira.example.com/browse/PAY-317".to_owned()),
        content_hash: "jira-pay-317".to_owned(),
    };
    store
        .upsert_artifact(&descriptor.id, &artifact, "2026-09-03T20:00:00Z", "test")
        .expect("Jira artifact");
    store
        .persist_links(
            &descriptor.id,
            &[ArtifactLink {
                source: artifact.identity,
                target: ArtifactLinkTarget::CodeSymbol(symbol.stable_key.clone()),
                kind: ArtifactLinkKind::ChangedSymbol,
                evidence_locator: "src/billing/subscription.py".to_owned(),
            }],
        )
        .expect("artifact link");
    drop(store);

    let output_root = tempfile::tempdir().expect("report output root");
    let output = output_root.path().join("html");
    repository.ctx(&[
        "report",
        "html",
        "--out",
        output.to_str().expect("UTF-8 path"),
    ]);

    let detail = read_report_tree(&output)
        .into_values()
        .filter_map(|bytes| String::from_utf8(bytes).ok())
        .find(|page| page.contains("billing.subscription.SubscriptionService.cancel"))
        .expect("cancel detail page");
    assert!(detail.contains("href=\"https://jira.example.com/browse/PAY-317\""));
    let head = git.head().expect("HEAD");
    assert!(detail.contains(&format!(
        "https://gitlab.example.com/payments/subscriptions/-/blob/{}/src/billing/subscription.py#L",
        head.oid
    )));
    let tree = fs::read_to_string(output.join("tree.html")).expect("tree page");
    assert!(tree.contains("id=\"tree-search\""));
    assert!(output.join("search-index.json").is_file());
}

#[test]
fn python_http_contracts_index_explain_and_retire_with_static_evidence() {
    let repository = FixtureRepository::new();
    let api_path = repository.root().join("src/billing/api.py");
    let source = r#"router = APIRouter(prefix="/v1")

@router.delete("/subscriptions/{subscription_id}")
def cancel_subscription(subscription_id: str, request: Request) -> Subscription:
    requests.post(f"https://audit.internal/subscriptions/{subscription_id}")
    return cancel(subscription_id)
"#;
    fs::write(&api_path, source).expect("API fixture");
    run_git(repository.root(), &["add", "."]);
    run_git(
        repository.root(),
        &["commit", "--quiet", "-m", "add Python HTTP contracts"],
    );

    repository.ctx(&["init"]);
    repository.ctx(&["index"]);
    let impact = repository.ctx(&["impact", "billing.api.cancel_subscription"]);
    assert_eq!(
        impact["matches"][0]["api_contracts"][0]["name"],
        "DELETE /v1/subscriptions/{subscription_id}"
    );
    assert_eq!(
        impact["matches"][0]["data_contracts"][0]["name"],
        "POST https://audit.internal/subscriptions/{param}"
    );
    let explanation = repository.ctx(&[
        "explain",
        "billing.api.cancel_subscription -> /v1/subscriptions/{subscription_id}",
    ]);
    assert_eq!(
        explanation["matches"][0]["claims"][0]["claim_class"],
        "fact"
    );
    assert!(
        explanation["matches"][0]["claims"][0]["evidence"][0]["locator"]
            .as_str()
            .is_some_and(|locator| locator.contains("decorator:DELETE"))
    );

    fs::write(
        &api_path,
        source.replacen(
            "@router.delete(\"/subscriptions/{subscription_id}\")\n",
            "",
            1,
        ),
    )
    .expect("remove decorator");
    let review = repository.ctx(&["review", "--base", "HEAD"]);
    assert_eq!(review["api_findings"][0]["destructive"], true);
    assert_eq!(review["api_findings"][0]["changes"][0]["kind"], "removed");
    run_git(repository.root(), &["add", "."]);
    run_git(
        repository.root(),
        &["commit", "--quiet", "-m", "remove endpoint"],
    );
    repository.ctx(&["index"]);
    let impact = repository.ctx(&["impact", "billing.api.cancel_subscription"]);
    assert!(
        impact["matches"][0]["api_contracts"]
            .as_array()
            .expect("API contracts")
            .is_empty()
    );
    let missing = repository.ctx_failure(&["explain", "/v1/subscriptions/{subscription_id}"]);
    assert!(
        missing["error"]
            .as_str()
            .is_some_and(|error| error.contains("nothing indexed matches"))
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

    // REQ-CTX-032: text output sections multiple matches as `[i/N]`, same as
    // impact/explain.
    let find_text = repository.ctx_text(&["find", "helper"]);
    assert!(find_text.starts_with("3 symbols found\n"));
    assert!(find_text.contains("[1/3]"));
    assert!(find_text.contains("[2/3]"));
    assert!(find_text.contains("[3/3]"));

    let find_unique_text = repository.ctx_text(&["find", "Run"]);
    assert!(
        !find_unique_text.contains("symbols found"),
        "a single unambiguous match should not need a header: {find_unique_text}"
    );
    assert!(!find_unique_text.contains("[1/1]"));
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
fn ingest_git_links_a_commit_to_the_symbol_in_the_file_it_changed() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);
    repository.ctx(&["index"]);

    let path = repository.root().join("src/billing/subscription.py");
    let source = fs::read_to_string(&path).expect("fixture source");
    fs::write(
        &path,
        format!("{source}\n# tightened cancellation window\n"),
    )
    .expect("write fixture change");
    run_git(repository.root(), &["add", "."]);
    run_git(
        repository.root(),
        &["commit", "--quiet", "-m", "tighten cancellation window"],
    );

    repository.ctx(&["ingest", "git"]);

    let explanation =
        repository.ctx(&["explain", "billing.subscription.SubscriptionService.cancel"]);
    let history = explanation["matches"][0]["artifact_history"]
        .as_array()
        .expect("artifact history");
    assert!(
        history
            .iter()
            .any(|entry| entry["artifact"]["title"] == "tighten cancellation window"),
        "commit that changed the file should appear in the symbol's artifact history: {history:?}"
    );
}

#[test]
fn ingest_gitlab_without_configuration_fails_clearly_before_any_network_call() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);

    let error = repository.ctx_failure(&["ingest", "gitlab", "--scope", "business-linked"]);
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("invalid GitLab configuration")
                && message.contains("[gitlab]"))
    );
}

#[test]
fn ingest_jira_without_configuration_fails_clearly_before_any_network_call() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);

    let error = repository.ctx_failure(&[
        "ingest",
        "jira",
        "--scope",
        "business-linked",
        "--related-depth",
        "0",
    ]);
    assert!(error["error"].as_str().is_some_and(|message| {
        message.contains("invalid Jira configuration") && message.contains("[jira]")
    }));
}

#[test]
fn ingest_rejects_an_unsupported_source() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);

    let error = repository.ctx_failure(&["ingest", "bitbucket"]);
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("unsupported ingest source"))
    );
}

#[test]
fn artifacts_prune_is_a_dry_run_until_apply_and_then_is_idempotent() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);
    let ingested = repository.ctx(&["ingest", "git"]);
    assert!(ingested["artifacts_ingested"].as_u64().unwrap_or(0) > 0);

    let dry_run = repository.ctx(&[
        "artifacts",
        "prune",
        "--scope",
        "business-linked",
        "--related-depth",
        "0",
    ]);
    let planned = dry_run["artifacts_pruned"]
        .as_u64()
        .expect("planned prune count");
    assert!(!dry_run["applied"].as_bool().expect("applied flag"));
    assert!(planned > 0);
    assert_eq!(
        dry_run["artifacts_removed"]
            .as_array()
            .expect("removed identities")
            .len(),
        0
    );

    let still_present = repository.ctx(&["artifacts", "prune"]);
    assert_eq!(still_present["artifacts_pruned"].as_u64(), Some(planned));
    let reason_summary = repository.ctx_text(&["-v", "artifacts", "prune"]);
    assert!(reason_summary.contains("NoBusinessAnchor"));
    let identity_detail = repository.ctx_text(&["-vv", "artifacts", "prune"]);
    assert!(identity_detail.contains("git:commit:"));

    let applied = repository.ctx(&["artifacts", "prune", "--apply"]);
    assert!(applied["applied"].as_bool().expect("applied flag"));
    assert_eq!(
        applied["artifacts_removed"]
            .as_array()
            .expect("removed identities")
            .len() as u64,
        planned
    );

    let repeated = repository.ctx(&["artifacts", "prune", "--apply"]);
    assert_eq!(repeated["artifacts_pruned"], 0);
    assert!(
        repeated["artifacts_removed"]
            .as_array()
            .expect("removed identities")
            .is_empty()
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
         prompt=\"$3\"\n\
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
    assert!(
        candidates[0]["implementation_candidates"]
            .as_array()
            .expect("implementation_candidates")
            .is_empty()
    );
    assert!(
        candidates[0]["test_candidates"]
            .as_array()
            .expect("test_candidates")
            .is_empty()
    );
}

#[test]
fn business_linked_enrich_never_invokes_an_agent_for_git_without_jira_context() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);
    repository.ctx(&["ingest", "git"]);
    let env = [("CTX_CLAUDE_CLI_BINARY", "/definitely/not/a/real/agent")];

    let enriched = repository.ctx_with_env(
        &["enrich", "--agent", "claude", "--scope", "business-linked"],
        &env,
    );

    assert_eq!(enriched["neighborhoods_analyzed"], 0);
    assert!(
        enriched["artifacts_skipped_no_business_anchor"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
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
         prompt=\"$3\"\n\
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
        queue_file
            .extension()
            .and_then(|extension| extension.to_str()),
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

/// `ctx enrich --agent claude` always runs `claude` with `--safe-mode`,
/// unconditionally -- there is no flag to turn it off, because this call is
/// always a single self-contained prompt-in/JSON-out contract that never
/// needs a repo's CLAUDE.md, skills, plugins, hooks, MCP servers, or custom
/// commands/agents, and paying their context cost on every enrich/review
/// call would be pure waste. Unlike `--bare`, `--safe-mode` leaves auth
/// (OAuth/keychain included) untouched, so this doesn't break users without
/// an `ANTHROPIC_API_KEY`. Only `claude` gets this treatment: `codex`/`agy`
/// have no equivalent single flag.
#[test]
fn enrich_always_runs_claude_with_safe_mode() {
    let repository = FixtureRepository::new();
    repository.ctx(&["init"]);
    repository.ctx(&["ingest", "git"]);

    let script_path = repository.root().join("fake-claude.sh");
    fs::write(
        &script_path,
        "#!/bin/sh\nif [ \"$1\" = \"-p\" ] && [ \"$2\" = \"--safe-mode\" ]; then echo '{\"outcome\":\"not_relevant\"}'; else exit 1; fi\n",
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
}

/// Regression test for a live bug: `ctx enrich --model haiku` accepted the
/// flag, recorded "haiku" into the enriched candidate's provenance, and
/// never actually put `--model haiku` on the agent CLI's argv -- so the
/// subprocess silently ran whichever model the CLI defaults to (opus)
/// while `ctx explain` kept claiming "haiku" produced it. Each fake script
/// below fails closed (`exit 1`) unless its own agent's exact `--model`
/// placement is present, so a regression here fails the `enrich` call
/// itself rather than just under-asserting on its output.
#[test]
fn enrich_passes_model_flag_through_to_each_agent_cli() {
    for (agent, binary_env, script_name, script_body) in [
        (
            "claude",
            "CTX_CLAUDE_CLI_BINARY",
            "fake-claude.sh",
            "#!/bin/sh\nif [ \"$1\" = \"-p\" ] && [ \"$2\" = \"--safe-mode\" ] && [ \"$3\" = \"--model\" ] && [ \"$4\" = \"haiku\" ]; then echo '{\"outcome\":\"not_relevant\"}'; else exit 1; fi\n",
        ),
        (
            "codex",
            "CTX_CODEX_CLI_BINARY",
            "fake-codex.sh",
            "#!/bin/sh\nif [ \"$1\" = \"exec\" ] && [ \"$2\" = \"--model\" ] && [ \"$3\" = \"haiku\" ]; then echo '{\"outcome\":\"not_relevant\"}'; else exit 1; fi\n",
        ),
        (
            "antigravity",
            "CTX_ANTIGRAVITY_CLI_BINARY",
            "fake-agy.sh",
            "#!/bin/sh\nif [ \"$1\" = \"-p\" ] && [ \"$2\" = \"--model\" ] && [ \"$3\" = \"haiku\" ]; then echo '{\"outcome\":\"not_relevant\"}'; else exit 1; fi\n",
        ),
    ] {
        let repository = FixtureRepository::new();
        repository.ctx(&["init"]);
        repository.ctx(&["ingest", "git"]);

        let script_path = repository.root().join(script_name);
        fs::write(&script_path, script_body).expect("write fake agent script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&script_path)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script_path, permissions).expect("chmod fake agent script");
        }

        let report = repository.ctx_with_env(
            &["enrich", "--agent", agent, "--model", "haiku"],
            &[(binary_env, script_path.to_str().expect("utf8 path"))],
        );

        assert!(
            report["neighborhoods_analyzed"].as_u64().unwrap_or(0) > 0,
            "agent {agent} did not receive --model in the shape its fake CLI required"
        );
    }
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
         prompt=\"$3\"\n\
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
         prompt=\"$3\"\n\
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

    fs::write(
        repository.root().join("src/billing.py"),
        "def cancel():\n    pass\n",
    )
    .expect("remove stale comment");
    run_git(repository.root(), &["add", "src/billing.py"]);
    run_git(
        repository.root(),
        &["commit", "--quiet", "-m", "remove stale comment"],
    );
    repository.ctx(&["index"]);

    let reconciled = repository.ctx(&["ingest", "code-comments", "--reconcile"]);
    assert_eq!(reconciled["artifacts_ingested"], 0);
    assert_eq!(reconciled["artifacts_removed"], 1);
    let repeated = repository.ctx(&["ingest", "code-comments", "--reconcile"]);
    assert_eq!(repeated["artifacts_removed"], 0);
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

#[test]
fn repositories_export_sync_resolve_and_report_federated_contracts() {
    let provider = service_repository(
        "billing-service",
        r#"router = APIRouter(prefix="/v1")

@router.post("/charges/{charge_id}")
def create_charge(charge_id: str) -> Charge:
    return {"id": charge_id}
"#,
        &[
            (
                "public.yaml",
                "id: REQ-BILLING-PUBLIC\ntype: requirement\nstatus: active\nvisibility: public\nstatement: Billing accepts charge creation requests.\n",
            ),
            (
                "private.yaml",
                "id: REQ-BILLING-PRIVATE\ntype: requirement\nstatus: active\nvisibility: private\nstatement: The internal ledger key must remain secret.\n",
            ),
        ],
    );
    let caller = service_repository(
        "checkout-service",
        r#"import requests

def submit_charge(charge_id: str):
    return requests.post(f"https://billing.internal/v1/charges/{charge_id}")

def notify_unknown():
    return requests.get("https://unknown.internal/health")
"#,
        &[],
    );
    let broken = service_repository("broken-service", "def noop():\n    return 1\n", &[]);

    for repository in [&provider, &caller] {
        ctx_json_at(repository.path(), &["init"]);
        ctx_json_at(repository.path(), &["index"]);
    }

    assert_deterministic_public_export(provider.path());

    let provider_path = provider.path().to_str().expect("UTF-8 provider path");
    let added = ctx_json_at(caller.path(), &["registry", "add", provider_path]);
    assert_eq!(added["changed"], true);
    assert_eq!(added["neighbor"]["name"], "billing-service");
    let idempotent = ctx_json_at(caller.path(), &["registry", "add", provider_path]);
    assert_eq!(idempotent["changed"], false);
    let before_sync = ctx_failure_at(caller.path(), &["federation", "show", "billing-service"]);
    assert!(
        before_sync["error"]
            .as_str()
            .is_some_and(|error| error.contains("run 'ctx sync' first"))
    );

    let missing = caller.path().join("does-not-exist");
    let missing_error = ctx_failure_at(
        caller.path(),
        &[
            "registry",
            "add",
            missing.to_str().expect("UTF-8 missing path"),
        ],
    );
    assert!(
        missing_error["error"]
            .as_str()
            .is_some_and(|error| error.contains("does not exist"))
    );
    ctx_json_at(
        caller.path(),
        &[
            "registry",
            "add",
            broken.path().to_str().expect("UTF-8 broken path"),
        ],
    );

    let sync = ctx_json_at(caller.path(), &["sync"]);
    assert_eq!(sync["synced"].as_array().map(Vec::len), Some(1));
    assert_eq!(sync["synced"][0]["name"], "billing-service");
    assert_eq!(sync["synced"][0]["resolutions"], 1);
    assert_eq!(sync["errors"].as_array().map(Vec::len), Some(1));
    assert_eq!(sync["errors"][0]["name"], "broken-service");
    assert_eq!(sync["unresolved_calls"].as_array().map(Vec::len), Some(1));
    assert_eq!(sync["unresolved_calls"][0]["path_template"], "/health");

    let show = ctx_json_at(caller.path(), &["federation", "show", "billing-service"]);
    assert_eq!(show["documents"][0]["id"], "REQ-BILLING-PUBLIC");
    assert_eq!(show["endpoints"][0]["path"], "/v1/charges/{charge_id}");
    assert_eq!(show["resolutions"][0]["status"], "FEDERATED_MATCH");
    assert_eq!(
        show["resolutions"][0]["call"]["path_template"],
        "/v1/charges/{param}"
    );
    assert_eq!(show["unresolved_calls"].as_array().map(Vec::len), Some(1));

    assert_neighbor_staleness(caller.path(), provider.path());
}

#[test]
fn trace_crosses_a_synchronized_neighbor_and_reports_an_unmatched_or_stale_call_honestly() {
    let fraud_checker = service_repository(
        "fraud-checker",
        r#"router = APIRouter()

@router.post("/check")
def check_fraud(order_id: str):
    return {"fraud": False}
"#,
        &[],
    );
    let billing = service_repository(
        "billing",
        r#"import requests

router = APIRouter()

@router.post("/pay")
def pay(order_id: str):
    requests.post("https://fraud-checker.internal/check")
    requests.get("https://unknown.internal/health")
    return {"ok": True}
"#,
        &[],
    );

    for repository in [&fraud_checker, &billing] {
        ctx_json_at(repository.path(), &["init"]);
        ctx_json_at(repository.path(), &["index"]);
    }

    let before_sync = ctx_json_at(billing.path(), &["trace", "POST /pay"]);
    let calls = before_sync["traces"][0]["calls"].as_array().expect("calls");
    assert_eq!(calls.len(), 2);
    for call in calls {
        assert_eq!(call["resolution"]["Unresolved"], "NoNeighborMatch");
    }

    ctx_json_at(
        billing.path(),
        &[
            "registry",
            "add",
            fraud_checker.path().to_str().expect("UTF-8 path"),
        ],
    );
    ctx_json_at(billing.path(), &["sync"]);

    let synced = ctx_json_at(billing.path(), &["trace", "POST /pay"]);
    let traces = synced["traces"][0].clone();
    let calls = traces["calls"].as_array().expect("calls");
    let crossed = calls
        .iter()
        .find_map(|call| call["resolution"]["Crosses"].as_object())
        .expect("one call crosses into fraud-checker");
    assert_eq!(crossed["service"], "fraud-checker");
    assert_eq!(crossed["handler"], "app.check_fraud");
    assert!(
        calls
            .iter()
            .any(|call| call["resolution"]["Unresolved"] == "NoNeighborMatch")
    );

    let wrong_target = ctx_failure_at(billing.path(), &["trace", "POST /check"]);
    let message = wrong_target["error"].as_str().expect("error message");
    assert!(message.contains("app.pay"), "message was: {message}");
    assert!(
        message.contains("ctx trace app.pay"),
        "message was: {message}"
    );

    fs::write(
        fraud_checker.path().join("README.md"),
        "advance the neighbor\n",
    )
    .expect("neighbor readme");
    run_git(fraud_checker.path(), &["add", "README.md"]);
    run_git(
        fraud_checker.path(),
        &["commit", "--quiet", "-m", "neighbor advances"],
    );

    let stale = ctx_json_at(billing.path(), &["trace", "POST /pay"]);
    let calls = stale["traces"][0]["calls"].as_array().expect("calls");
    let stale_reason = calls
        .iter()
        .find_map(|call| call["resolution"]["Unresolved"]["NeighborStale"].as_object())
        .expect("the crossing call now reports the neighbor as stale");
    assert_eq!(stale_reason["service"], "fraud-checker");
}

#[test]
fn trace_verbose_attaches_product_context_across_a_federation_crossing() {
    let fraud_checker = service_repository(
        "fraud-checker",
        r#"router = APIRouter()

@router.post("/check")
def check_fraud(order_id: str):
    return {"fraud": False}
"#,
        &[
            (
                "feature.yaml",
                "id: FEAT-FRAUD\ntype: feature\nstatus: active\nvisibility: public\nname: Fraud checks\ndescription: Fraud checking.\n",
            ),
            (
                "req.yaml",
                "id: REQ-FRAUD-001\ntype: requirement\nstatus: active\nvisibility: public\nfeature: FEAT-FRAUD\nstatement: Every payment must be checked for fraud.\nimplementation:\n  - app.check_fraud\n",
            ),
        ],
    );
    let billing = service_repository(
        "billing",
        r#"import requests

router = APIRouter()

@router.post("/pay")
def pay(order_id: str):
    requests.post("https://fraud-checker.internal/check")
    return {"ok": True}
"#,
        &[
            (
                "feature.yaml",
                "id: FEAT-PAY\ntype: feature\nstatus: active\nvisibility: public\nname: Payments\ndescription: Payment processing.\n",
            ),
            (
                "req.yaml",
                "id: REQ-PAY-001\ntype: requirement\nstatus: active\nvisibility: public\nfeature: FEAT-PAY\nstatement: Billing must process payments.\nimplementation:\n  - app.pay\n",
            ),
        ],
    );

    for repository in [&fraud_checker, &billing] {
        ctx_json_at(repository.path(), &["init"]);
        ctx_json_at(repository.path(), &["index"]);
    }
    ctx_json_at(
        billing.path(),
        &[
            "registry",
            "add",
            fraud_checker.path().to_str().expect("UTF-8 path"),
        ],
    );
    ctx_json_at(billing.path(), &["sync"]);

    let quiet = ctx_json_at(billing.path(), &["trace", "POST /pay"]);
    assert!(quiet["traces"][0]["product_context"].is_null());

    let verbose = ctx_json_at(billing.path(), &["-v", "trace", "POST /pay"]);
    let root = &verbose["traces"][0];
    assert_eq!(
        root["product_context"]["features"],
        json_array(&["FEAT-PAY"])
    );
    assert_eq!(
        root["product_context"]["requirements"],
        json_array(&["REQ-PAY-001"])
    );
    let crossed = &root["calls"][0]["resolution"]["Crosses"];
    assert_eq!(
        crossed["product_context"]["features"],
        json_array(&["FEAT-FRAUD"])
    );
    assert_eq!(
        crossed["product_context"]["requirements"],
        json_array(&["REQ-FRAUD-001"])
    );
}

#[test]
fn explain_trace_finds_every_endpoint_mapped_to_a_feature_and_traces_each() {
    let billing = service_repository(
        "billing",
        r#"router = APIRouter()

@router.post("/pay")
def pay(order_id: str):
    return {"ok": True}

@router.get("/refund/{order_id}")
def refund(order_id: str):
    return {"refunded": True}

def internal_helper():
    return 1
"#,
        &[
            (
                "feature.yaml",
                "id: FEAT-PAY\ntype: feature\nstatus: active\nvisibility: public\nname: Payments\ndescription: Everything payment related.\n",
            ),
            (
                "req-pay.yaml",
                "id: REQ-PAY-001\ntype: requirement\nstatus: active\nvisibility: public\nfeature: FEAT-PAY\nstatement: Billing must process payments.\nimplementation:\n  - app.pay\n",
            ),
            (
                "req-refund.yaml",
                "id: REQ-PAY-002\ntype: requirement\nstatus: active\nvisibility: public\nfeature: FEAT-PAY\nstatement: Billing must allow refunds.\nimplementation:\n  - app.refund\n",
            ),
        ],
    );
    ctx_json_at(billing.path(), &["init"]);
    ctx_json_at(billing.path(), &["index"]);

    let without_trace = ctx_json_at(billing.path(), &["explain", "FEAT-PAY"]);
    assert!(without_trace.get("traces").is_none());

    let with_trace = ctx_json_at(billing.path(), &["explain", "FEAT-PAY", "--trace"]);
    let traces = with_trace["traces"].as_array().expect("traces array");
    let mut paths = traces
        .iter()
        .map(|trace| trace["path"].as_str().expect("path").to_owned())
        .collect::<Vec<_>>();
    paths.sort();
    assert_eq!(
        paths,
        vec!["/pay".to_owned(), "/refund/{order_id}".to_owned()]
    );
    for trace in traces {
        assert!(trace["product_context"].is_null());
    }
}

#[test]
fn verify_stale_reactivates_an_accepted_claim_and_leaves_a_rejected_one_as_a_suggestion_only() {
    let billing = service_repository(
        "billing",
        "def cancel(order_id: str):\n    return {\"cancelled\": True}\n",
        &[(
            "req.yaml",
            "id: REQ-SUB-014\ntype: requirement\nstatus: active\nvisibility: public\nstatement: Cancellation preserves paid access.\nimplementation:\n  - app.cancel\n",
        )],
    );
    ctx_json_at(billing.path(), &["init"]);
    ctx_json_at(billing.path(), &["index"]);

    // Reshapes app.cancel (an added parameter) without changing its own
    // identity -- this is exactly what marks the existing Implements edge
    // stale rather than retiring/recreating the symbol.
    fs::write(
        billing.path().join("src/app.py"),
        "def cancel(order_id: str, reason: str = \"user_requested\"):\n    return {\"cancelled\": True, \"reason\": reason}\n",
    )
    .expect("reshaped source");
    run_git(billing.path(), &["add", "-A"]);
    run_git(
        billing.path(),
        &["commit", "--quiet", "-m", "cancel logs a reason"],
    );
    ctx_json_at(billing.path(), &["index"]);

    let before = ctx_json_at(billing.path(), &["status"]);
    assert_eq!(before["knowledge"]["stale_semantic_edges"], 1);
    assert_eq!(before["health"], "needs_attention");

    // One fake `claude` script stands in for the review agent: it always
    // accepts, since this test's own scenario has exactly one stale claim
    // that genuinely still holds -- the reject path is exercised directly
    // against `agent_contract::review_stale_claims` and `VerificationService`
    // unit tests, which is where a deliberately-wrong verdict belongs.
    let script_path = billing.path().join("fake-claude.sh");
    fs::write(
        &script_path,
        "#!/bin/sh\n\
         fp=$(echo \"$3\" | grep '^- fingerprint:' | head -1 | sed 's/^- fingerprint: //')\n\
         echo \"{\\\"decisions\\\":[{\\\"fingerprint\\\":\\\"$fp\\\",\\\"verdict\\\":\\\"accept\\\",\\\"reasoning\\\":\\\"cancel still preserves paid access; it only gained an optional reason parameter\\\"}]}\"\n",
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

    let review = ctx_json_at_with_env(billing.path(), &["verify", "--stale"], &env);
    assert_eq!(review["report"]["claims_reviewed"], 1);
    assert_eq!(review["report"]["reactivated"], 1);
    assert_eq!(review["report"]["suggested_removals"], 0);
    assert_eq!(review["results"][0]["outcome"]["kind"], "reactivated");
    assert_eq!(review["results"][0]["source"], "app.cancel");
    assert_eq!(review["results"][0]["target"], "REQ-SUB-014");

    let after = ctx_json_at(billing.path(), &["status"]);
    assert_eq!(after["knowledge"]["stale_semantic_edges"], 0);
    assert_eq!(after["health"], "ready");

    // Reviewing again with nothing stale left must not call the agent at
    // all -- the script exits nonzero (empty response) if invoked, which
    // would fail loudly rather than silently, so a passing run here proves
    // it genuinely wasn't called.
    fs::write(&script_path, "#!/bin/sh\nexit 1\n").expect("rewrite fake claude script");
    let empty = ctx_json_at_with_env(billing.path(), &["verify", "--stale"], &env);
    assert_eq!(empty["claims_reviewed"], 0);
}

#[test]
fn export_requires_an_explicit_service_identity() {
    let repository = tempfile::tempdir().expect("temporary repository");
    fs::create_dir_all(repository.path().join("src")).expect("source directory");
    fs::create_dir_all(repository.path().join(".ctx")).expect("ctx directory");
    fs::write(
        repository.path().join(".ctx/config.toml"),
        "languages = [\"python\"]\n\n[paths]\ninclude = [\"src\"]\n",
    )
    .expect("configuration");
    fs::write(
        repository.path().join("src/app.py"),
        "def run():\n    return 1\n",
    )
    .expect("source");
    initialize_git_repository(repository.path());
    ctx_json_at(repository.path(), &["init"]);
    ctx_json_at(repository.path(), &["index"]);

    let error = ctx_failure_at(repository.path(), &["export"]);
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("[service].name"))
    );
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
         prompt=\"$3\"\n\
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

#[test]
fn openapi_specs_are_discovered_reviewed_and_exported_as_public_contracts() {
    let repository = tempfile::tempdir().expect("OpenAPI repository");
    fs::create_dir_all(repository.path().join(".ctx")).expect("ctx directory");
    fs::write(
        repository.path().join(".ctx/config.toml"),
        "languages = [\"rust\"]\n\n[paths]\ninclude = [\"src\"]\n\n[service]\nname = \"openapi-service\"\n",
    )
    .expect("configuration");
    let specification = r"
openapi: 3.1.0
security:
  - oauth: [items:read]
paths:
  /items/{id}:
    get:
      operationId: getItem
      parameters:
        - name: id
          in: path
          required: true
          schema: {type: string}
      responses:
        '200':
          description: Found
          content:
            application/json:
              schema: {$ref: '#/components/schemas/Item'}
    head:
      operationId: itemExists
      responses:
        '204': {description: Exists}
components:
  schemas:
    Item: {type: object}
";
    fs::write(repository.path().join("openapi.yaml"), specification)
        .expect("OpenAPI specification");
    initialize_git_repository(repository.path());

    ctx_json_at(repository.path(), &["init"]);
    let indexed = ctx_json_at(repository.path(), &["index"]);
    assert!(
        indexed["failed_files"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    assert!(indexed["stats"]["nodes_created"].as_u64().unwrap_or(0) > 0);

    let impact = ctx_json_at(repository.path(), &["impact", "HEAD /items/{id}"]);
    assert!(impact.to_string().contains("itemExists"));
    assert!(impact.to_string().contains("openapi"));

    let exported = ctx_json_at(repository.path(), &["export"]);
    assert_eq!(exported["endpoints"], 2);
    assert_eq!(exported["documents"], 0);
    let manifest: Value = serde_json::from_slice(
        &fs::read(repository.path().join(".ctx/export.json")).expect("export manifest"),
    )
    .expect("manifest JSON");
    assert_eq!(manifest["schema_version"], 2);
    assert_eq!(manifest["endpoints"].as_array().map(Vec::len), Some(2));
    assert!(manifest["endpoints"].as_array().is_some_and(|endpoints| {
        endpoints
            .iter()
            .all(|endpoint| endpoint["openapi"].is_object())
    }));

    fs::write(
        repository.path().join("openapi.yaml"),
        specification.replace("security:\n  - oauth: [items:read]", "security: []"),
    )
    .expect("change OpenAPI security");
    let review = ctx_json_at(repository.path(), &["review", "--base", "HEAD"]);
    let api_findings = review["api_findings"].as_array().expect("API findings");
    assert_eq!(api_findings.len(), 2);
    assert!(
        api_findings
            .iter()
            .all(|finding| finding["destructive"] == true)
    );
    assert!(
        api_findings
            .iter()
            .all(|finding| finding.to_string().contains("OpenAPI security changed"))
    );
}

#[test]
fn code_and_openapi_endpoints_for_the_same_route_export_as_one_openapi_backed_contract() {
    let repository = tempfile::tempdir().expect("mixed contract repository");
    fs::create_dir_all(repository.path().join(".ctx")).expect("ctx directory");
    fs::create_dir_all(repository.path().join("src")).expect("src directory");
    fs::write(
        repository.path().join(".ctx/config.toml"),
        "languages = [\"python\"]\n\n[paths]\ninclude = [\"src\"]\n\n[service]\nname = \"mixed-service\"\n",
    )
    .expect("configuration");
    fs::write(
        repository.path().join("src/api.py"),
        "@app.get(\"/items/{id}\")\ndef get_item(id: str) -> Item:\n    return load(id)\n",
    )
    .expect("Python handler");
    let specification = r"
openapi: 3.1.0
paths:
  /items/{id}:
    get:
      operationId: getItem
      parameters:
        - name: id
          in: path
          required: true
          schema: {type: string}
      responses:
        '200':
          description: Found
          content:
            application/json:
              schema: {$ref: '#/components/schemas/Item'}
components:
  schemas:
    Item: {type: object}
";
    fs::write(repository.path().join("openapi.yaml"), specification)
        .expect("OpenAPI specification");
    initialize_git_repository(repository.path());

    ctx_json_at(repository.path(), &["init"]);
    let indexed = ctx_json_at(repository.path(), &["index"]);
    assert!(
        indexed["failed_files"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );

    let exported = ctx_json_at(repository.path(), &["export"]);
    assert_eq!(
        exported["endpoints"], 1,
        "code and OpenAPI evidence for the same route must merge into one exported endpoint"
    );
    let manifest: Value = serde_json::from_slice(
        &fs::read(repository.path().join(".ctx/export.json")).expect("export manifest"),
    )
    .expect("manifest JSON");
    let endpoints = manifest["endpoints"].as_array().expect("endpoints array");
    assert_eq!(endpoints.len(), 1);
    let endpoint = &endpoints[0];
    assert_eq!(endpoint["path"], "/items/{id}");
    assert!(
        endpoint["openapi"].is_object(),
        "the OpenAPI contract must win over the code-derived one: {endpoint}"
    );
    let handler = endpoint["handler"].as_str().expect("handler string");
    assert!(
        handler.contains("get_item") && !handler.starts_with("openapi."),
        "the real code handler must be kept as the trace target, not the OpenAPI operation symbol: {handler}"
    );
    let evidence = endpoint["evidence"].as_array().expect("evidence array");
    assert!(
        evidence.iter().any(|item| item["source_uri"]
            .as_str()
            .is_some_and(|uri| uri.contains("api.py"))),
        "evidence must include the code handler's edge: {evidence:?}"
    );
    assert!(
        evidence.iter().any(|item| item["source_uri"]
            .as_str()
            .is_some_and(|uri| uri.contains("openapi.yaml"))),
        "evidence must include the OpenAPI operation's edge: {evidence:?}"
    );
}

#[test]
fn context_store_set_git_redirects_to_a_separate_repository_and_still_enforces_commits() {
    // ADR-CTX-050: .context/ and .ctx-candidates/ may be redirected to a
    // separate repository (e.g. because the checkout being documented isn't
    // ours to commit into), resolved through a local-only registry that
    // never writes into the checkout itself. `--git` opts into the same
    // commit-before-index guarantee this checkout's own .context/ would have.
    let source = tempfile::tempdir().expect("source repository");
    fs::create_dir_all(source.path().join("src")).expect("src directory");
    fs::write(
        source.path().join("src/app.py"),
        "def handler():\n    return 'ok'\n",
    )
    .expect("python source");
    run_git(source.path(), &["init", "--quiet"]);
    run_git(source.path(), &["config", "user.name", "ctx tests"]);
    run_git(
        source.path(),
        &["config", "user.email", "ctx@example.invalid"],
    );
    run_git(source.path(), &["add", "."]);
    run_git(source.path(), &["commit", "--quiet", "-m", "seed source"]);

    let registry_directory = tempfile::tempdir().expect("registry directory");
    let registry_file = registry_directory.path().join("contexts.toml");
    let registry_file_str = registry_file
        .to_str()
        .expect("utf8 registry path")
        .to_owned();
    let context_store = tempfile::tempdir().expect("external context store");
    let context_store_path = context_store
        .path()
        .to_str()
        .expect("utf8 context path")
        .to_owned();
    let env: [(&str, &str); 1] = [("CTX_CONTEXTS_FILE", &registry_file_str)];
    let ctx = |arguments: &[&str]| run_ctx(source.path(), &env, arguments);
    let ctx_failure = |arguments: &[&str]| run_ctx_failure(source.path(), &env, arguments);

    let set_report = ctx(&["context-store", "set", "--git", &context_store_path]);
    assert_eq!(set_report["ok"], true);

    let show_report = ctx(&["context-store", "show"]);
    assert_eq!(show_report["external"], true);
    assert_eq!(show_report["git_backed"], true);
    assert_eq!(
        show_report["context_repository"]
            .as_str()
            .expect("context_repository string"),
        context_store_path
    );

    ctx(&["init"]);
    assert!(
        !source.path().join(".context").exists(),
        ".context must never be created inside a redirected checkout"
    );
    assert!(
        !source.path().join(".ctx-candidates").exists(),
        ".ctx-candidates must never be created inside a redirected checkout"
    );
    assert!(context_store.path().join(".context/requirements").is_dir());
    assert!(context_store.path().join(".git").exists());

    fs::write(
        context_store
            .path()
            .join(".context/requirements/req-handler.yaml"),
        "id: REQ-HANDLER-001\ntype: requirement\nstatus: active\ntitle: Handler returns ok\n\
         statement: The handler must return ok.\nimplementation:\n  - symbol: app.handler\n",
    )
    .expect("write requirement in the external store");
    run_git(context_store.path(), &["add", "."]);
    run_git(context_store.path(), &["config", "user.name", "ctx tests"]);
    run_git(
        context_store.path(),
        &["config", "user.email", "ctx@example.invalid"],
    );
    run_git(
        context_store.path(),
        &["commit", "--quiet", "-m", "add handler requirement"],
    );

    let index_report = ctx(&["index"]);
    assert_eq!(index_report["business_context"]["documents_created"], 1);

    let explain = ctx(&["explain", "REQ-HANDLER-001"]);
    assert!(
        explain
            .to_string()
            .contains(".context/requirements/req-handler.yaml"),
        "expected evidence to cite the external requirement file: {explain}"
    );

    fs::write(
        context_store
            .path()
            .join(".context/requirements/req-handler.yaml"),
        "id: REQ-HANDLER-001\ntype: requirement\nstatus: active\ntitle: Handler returns ok\n\
         statement: dirty\nimplementation:\n  - symbol: app.handler\n",
    )
    .expect("dirty the external store");
    let error = ctx_failure(&["index"]);
    assert!(
        error["error"].as_str().is_some_and(
            |message| message.contains("context:.context/requirements/req-handler.yaml")
        ),
        "expected the uncommitted-inputs error to name the external file: {error}"
    );
}

#[test]
fn context_store_set_defaults_to_a_plain_directory_with_no_commit_gate() {
    // ADR-CTX-050: without --git, the redirected context store is just a
    // directory -- no Git repository is created or required, and .context/*
    // documents there are read as-is with no commit-before-index guarantee.
    let source = tempfile::tempdir().expect("source repository");
    fs::create_dir_all(source.path().join("src")).expect("src directory");
    fs::write(
        source.path().join("src/app.py"),
        "def handler():\n    return 'ok'\n",
    )
    .expect("python source");
    run_git(source.path(), &["init", "--quiet"]);
    run_git(source.path(), &["config", "user.name", "ctx tests"]);
    run_git(
        source.path(),
        &["config", "user.email", "ctx@example.invalid"],
    );
    run_git(source.path(), &["add", "."]);
    run_git(source.path(), &["commit", "--quiet", "-m", "seed source"]);

    let registry_directory = tempfile::tempdir().expect("registry directory");
    let registry_file = registry_directory.path().join("contexts.toml");
    let registry_file_str = registry_file
        .to_str()
        .expect("utf8 registry path")
        .to_owned();
    let context_store = tempfile::tempdir().expect("external context store");
    let context_store_path = context_store
        .path()
        .to_str()
        .expect("utf8 context path")
        .to_owned();
    let env: [(&str, &str); 1] = [("CTX_CONTEXTS_FILE", &registry_file_str)];
    let ctx = |arguments: &[&str]| run_ctx(source.path(), &env, arguments);

    let set_report = ctx(&["context-store", "set", &context_store_path]);
    assert_eq!(set_report["ok"], true);
    assert_eq!(set_report["git_backed"], false);

    let show_report = ctx(&["context-store", "show"]);
    assert_eq!(show_report["external"], true);
    assert_eq!(show_report["git_backed"], false);

    ctx(&["init"]);
    assert!(context_store.path().join(".context/requirements").is_dir());
    assert!(
        !context_store.path().join(".git").exists(),
        "no Git repository should be created without --git"
    );

    fs::write(
        context_store
            .path()
            .join(".context/requirements/req-handler.yaml"),
        "id: REQ-HANDLER-001\ntype: requirement\nstatus: active\ntitle: Handler returns ok\n\
         statement: The handler must return ok.\nimplementation:\n  - symbol: app.handler\n",
    )
    .expect("write requirement in the external store");

    // No commit gate at all: indexing must succeed even though nothing was
    // ever committed anywhere in the plain-folder context store.
    let index_report = ctx(&["index"]);
    assert_eq!(index_report["business_context"]["documents_created"], 1);

    let explain = ctx(&["explain", "REQ-HANDLER-001"]);
    assert!(
        explain
            .to_string()
            .contains(".context/requirements/req-handler.yaml"),
        "expected evidence to cite the external requirement file: {explain}"
    );
}

#[test]
fn enrich_and_verify_knowledge_accept_a_candidate_into_an_external_context_store() {
    // ADR-CTX-050, .ctx-candidates/ half: the pending-candidate queue
    // (ADR-EXT-004) and knowledge acceptance must both go through
    // context_root exactly like .context/ itself -- this is the one
    // end-to-end path that touches both.
    let source = tempfile::tempdir().expect("source repository");
    fs::create_dir_all(source.path().join("src")).expect("src directory");
    fs::write(
        source.path().join("src/app.py"),
        "def handler():\n    return 'ok'\n",
    )
    .expect("python source");
    run_git(source.path(), &["init", "--quiet"]);
    run_git(source.path(), &["config", "user.name", "ctx tests"]);
    run_git(
        source.path(),
        &["config", "user.email", "ctx@example.invalid"],
    );
    run_git(source.path(), &["add", "."]);
    run_git(
        source.path(),
        &["commit", "--quiet", "-m", "PAY-1 add handler"],
    );

    let registry_directory = tempfile::tempdir().expect("registry directory");
    let registry_file = registry_directory.path().join("contexts.toml");
    let registry_file_str = registry_file
        .to_str()
        .expect("utf8 registry path")
        .to_owned();
    let context_store = tempfile::tempdir().expect("external context store");
    let context_store_path = context_store
        .path()
        .to_str()
        .expect("utf8 context path")
        .to_owned();
    let base_env: [(&str, &str); 1] = [("CTX_CONTEXTS_FILE", &registry_file_str)];
    let ctx = |arguments: &[&str]| run_ctx(source.path(), &base_env, arguments);

    ctx(&["context-store", "set", "--git", &context_store_path]);
    ctx(&["init"]);
    ctx(&["ingest", "git"]);

    let script_path = source.path().join("fake-claude.sh");
    write_executable_script(
        &script_path,
        "#!/bin/sh\n\
         prompt=\"$3\"\n\
         id=$(echo \"$prompt\" | grep -o 'Valid artifact ids for this neighborhood: [^ ,]*' | sed 's/.*: //')\n\
         echo \"{\\\"outcome\\\":\\\"relevant\\\",\\\"candidates\\\":[{\\\"kind\\\":\\\"requirement\\\",\\\"statement\\\":\\\"Handler must return ok.\\\",\\\"evidence\\\":[{\\\"artifact_id\\\":\\\"$id\\\",\\\"locator\\\":\\\"body\\\",\\\"excerpt\\\":\\\"excerpt\\\"}]}]}\"\n",
    );
    let mut env: Vec<(&str, &str)> = base_env.to_vec();
    let script_path_str = script_path.to_str().expect("utf8 path");
    env.push(("CTX_CLAUDE_CLI_BINARY", script_path_str));
    let ctx_with_agent = |arguments: &[&str]| run_ctx(source.path(), &env, arguments);

    let enriched = ctx_with_agent(&["enrich", "--agent", "claude"]);
    assert!(enriched["candidates_proposed"].as_u64().unwrap_or(0) > 0);
    // The queue itself is a file under .ctx-candidates/ at context_root, not
    // under the source checkout.
    assert!(!source.path().join(".ctx-candidates").exists());
    assert!(
        fs::read_dir(context_store.path().join(".ctx-candidates"))
            .expect("read external candidate queue")
            .next()
            .is_some(),
        "expected a candidate file under the external .ctx-candidates/"
    );

    let pending = ctx(&["verify", "--knowledge"]);
    let candidates = pending.as_array().expect("pending candidates");
    assert_eq!(candidates.len(), 1);
    let fingerprint = candidates[0]["fingerprint"]
        .as_str()
        .expect("fingerprint")
        .to_owned();

    let accepted = ctx(&[
        "verify",
        "--knowledge",
        "--accept",
        &fingerprint,
        "--id",
        "REQ-HANDLER-002",
    ]);
    assert_eq!(accepted["ok"], true);
    let written_path = accepted["path"].as_str().expect("written path").to_owned();
    assert!(
        context_store.path().join(&written_path).exists(),
        "accepted document should land under the external context store, not the source checkout"
    );

    run_git(context_store.path(), &["add", &written_path]);
    run_git(context_store.path(), &["config", "user.name", "ctx tests"]);
    run_git(
        context_store.path(),
        &["config", "user.email", "ctx@example.invalid"],
    );
    run_git(
        context_store.path(),
        &["commit", "--quiet", "-m", "accept REQ-HANDLER-002"],
    );
    let indexed = ctx(&["index"]);
    assert!(
        indexed["business_context"]["documents_created"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "expected the accepted document to be absorbed on the next index: {indexed}"
    );
}

fn write_executable_script(path: &Path, content: &str) {
    fs::write(path, content).expect("write script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path).expect("script metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("chmod script");
    }
}

fn run_ctx(root: &Path, env: &[(&str, &str)], arguments: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_ctx"))
        .current_dir(root)
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

fn run_ctx_failure(root: &Path, env: &[(&str, &str)], arguments: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_ctx"))
        .current_dir(root)
        .envs(env.iter().copied())
        .arg("--json")
        .args(arguments)
        .output()
        .expect("execute ctx");
    assert!(
        !output.status.success(),
        "ctx {} unexpectedly succeeded",
        arguments.join(" ")
    );
    serde_json::from_slice(&output.stderr).expect("ctx JSON error")
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

fn assert_deterministic_public_export(provider: &Path) {
    let first_path = provider.join(".ctx/export-one.json");
    let second_path = provider.join(".ctx/export-two.json");
    ctx_json_at(
        provider,
        &["export", "--out", first_path.to_str().expect("UTF-8 path")],
    );
    ctx_json_at(
        provider,
        &["export", "--out", second_path.to_str().expect("UTF-8 path")],
    );
    let first = fs::read(&first_path).expect("first manifest");
    let second = fs::read(&second_path).expect("second manifest");
    assert_eq!(first, second, "same-commit exports must be byte-stable");
    let manifest: Value = serde_json::from_slice(&first).expect("manifest JSON");
    assert_eq!(manifest["service_name"], "billing-service");
    assert_eq!(manifest["documents"].as_array().map(Vec::len), Some(1));
    assert_eq!(manifest["documents"][0]["id"], "REQ-BILLING-PUBLIC");
    assert!(!String::from_utf8_lossy(&first).contains("REQ-BILLING-PRIVATE"));
    assert!(!String::from_utf8_lossy(&first).contains("return {\"id\""));
    assert_eq!(manifest["endpoints"][0]["method"], "post");
    assert_eq!(manifest["endpoints"][0]["path"], "/v1/charges/{charge_id}");
}

fn assert_neighbor_staleness(caller: &Path, provider: &Path) {
    let list = ctx_json_at(caller, &["federation", "list"]);
    let billing = list["neighbors"]
        .as_array()
        .expect("neighbor list")
        .iter()
        .find(|neighbor| neighbor["name"] == "billing-service")
        .expect("billing neighbor");
    assert_eq!(billing["stale"], false);

    fs::write(provider.join("README.md"), "new provider commit\n").expect("provider readme");
    run_git(provider, &["add", "README.md"]);
    run_git(provider, &["commit", "--quiet", "-m", "provider advances"]);
    let stale = ctx_json_at(caller, &["federation", "list"]);
    let billing = stale["neighbors"]
        .as_array()
        .expect("neighbor list")
        .iter()
        .find(|neighbor| neighbor["name"] == "billing-service")
        .expect("billing neighbor");
    assert_eq!(billing["stale"], true);
}

fn service_repository(service_name: &str, source: &str, documents: &[(&str, &str)]) -> TempDir {
    let repository = tempfile::tempdir().expect("temporary service repository");
    fs::create_dir_all(repository.path().join("src")).expect("source directory");
    fs::create_dir_all(repository.path().join(".ctx")).expect("ctx directory");
    fs::create_dir_all(repository.path().join(".context/requirements"))
        .expect("requirements directory");
    fs::write(
        repository.path().join(".ctx/config.toml"),
        format!(
            "languages = [\"python\"]\n\n[paths]\ninclude = [\"src\"]\n\n[service]\nname = \"{service_name}\"\n"
        ),
    )
    .expect("service configuration");
    fs::write(repository.path().join("src/app.py"), source).expect("service source");
    for (name, content) in documents {
        fs::write(
            repository.path().join(".context/requirements").join(name),
            content,
        )
        .expect("service document");
    }
    initialize_git_repository(repository.path());
    repository
}

fn initialize_git_repository(root: &Path) {
    run_git(root, &["init", "--quiet"]);
    run_git(root, &["config", "user.name", "ctx tests"]);
    run_git(root, &["config", "user.email", "ctx@example.invalid"]);
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "--quiet", "-m", "service baseline"]);
}

fn ctx_json_at(root: &Path, arguments: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_ctx"))
        .current_dir(root)
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

fn ctx_json_at_with_env(root: &Path, arguments: &[&str], env: &[(&str, &str)]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_ctx"))
        .current_dir(root)
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

fn ctx_failure_at(root: &Path, arguments: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_ctx"))
        .current_dir(root)
        .arg("--json")
        .args(arguments)
        .output()
        .expect("execute ctx");
    assert!(
        !output.status.success(),
        "ctx {} unexpectedly succeeded",
        arguments.join(" ")
    );
    serde_json::from_slice(&output.stderr).expect("ctx JSON error")
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

fn read_report_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn collect(root: &Path, directory: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
        let mut entries = fs::read_dir(directory)
            .expect("report directory")
            .map(|entry| entry.expect("report entry").path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                collect(root, &path, files);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .expect("report-relative path")
                    .to_string_lossy()
                    .replace('\\', "/");
                files.insert(relative, fs::read(&path).expect("report file"));
            }
        }
    }

    let mut files = BTreeMap::new();
    collect(root, root, &mut files);
    files
}

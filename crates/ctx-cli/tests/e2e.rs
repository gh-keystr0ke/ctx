use std::{collections::BTreeSet, fs, path::Path, process::Command};

use serde_json::Value;
use tempfile::TempDir;

struct FixtureRepository {
    directory: TempDir,
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
    assert_eq!(status["knowledge"]["structural_facts"], 11);
    assert_eq!(status["knowledge"]["active_assertions"], 7);
    assert_eq!(status["knowledge"]["active_edges"], 18);
    assert_eq!(status["knowledge"]["stale_semantic_edges"], 0);
}

fn assert_product_impact(impact: &Value) {
    let serialized = impact.to_string();
    for expected in [
        "FEAT-SUBSCRIPTIONS",
        "REQ-SUB-014",
        "INV-SUB-003",
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
    assert_eq!(findings.len(), 2);
    let intent_ids = findings
        .iter()
        .filter_map(|finding| finding["affected_intent"]["identifier"].as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(intent_ids, BTreeSet::from(["INV-SUB-003", "REQ-SUB-014"]));
    assert!(findings.iter().all(|finding| {
        finding["severity"] == "high"
            && finding["tests_modified"] == false
            && finding["evidence"]
                .as_array()
                .is_some_and(|evidence| !evidence.is_empty())
    }));
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

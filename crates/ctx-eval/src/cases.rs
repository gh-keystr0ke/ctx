//! The evaluation corpus: a small, deterministic set of Git-history cases
//! with machine-readable ground truth checks.
//!
//! Each case is defined entirely in this file so the corpus is versioned in
//! Git alongside the code it exercises (see `prompt.md`, "First evaluation
//! corpus"). Cases reuse the first-party `fixtures/subscriptions` source and
//! `.context` documents where possible so the corpus tracks the same product
//! example used by the CLI end-to-end scenario, instead of drifting from it.

use ctx_core::review::{ChangeKind, Severity};

use crate::report::Check;

const SUBSCRIPTION_SERVICE: &str = "billing.subscription.SubscriptionService.cancel";
const STRIPE_HANDLER: &str = "billing.subscription.StripeWebhookHandler.handle_subscription_update";

const BASE_SUBSCRIPTION_PY: &str =
    include_str!("../../../fixtures/subscriptions/src/billing/subscription.py");
const BASE_TEST_PY: &str =
    include_str!("../../../fixtures/subscriptions/tests/test_subscription.py");
const FEATURE_YAML: &str =
    include_str!("../../../fixtures/subscriptions/.context/features/subscriptions.yaml");
const REQUIREMENT_YAML: &str =
    include_str!("../../../fixtures/subscriptions/.context/requirements/cancel-at-period-end.yaml");
const INVARIANT_YAML: &str =
    include_str!("../../../fixtures/subscriptions/.context/invariants/paid-entitlement.yaml");
const DECISION_YAML: &str =
    include_str!("../../../fixtures/subscriptions/.context/decisions/stripe-ordering.yaml");

/// One step of an evaluation case's deterministic Git history.
pub enum Step {
    /// Writes (or overwrites) files relative to the repository root.
    WriteFiles(Vec<(&'static str, String)>),
    /// Stages and commits the current working tree.
    Commit(&'static str),
    /// Runs `ctx index` (code indexing plus business-context import).
    Index,
    /// Runs `ctx review --base <base>` and records the result.
    Review { base: &'static str },
    /// Runs `ctx impact <target>` and records the result.
    Impact { target: &'static str },
    /// Runs `ctx context <task>` and records the result.
    Context {
        task: &'static str,
        symbols: Vec<&'static str>,
        token_budget: usize,
    },
}

/// One evaluation case: a scripted Git history plus the ground truth checks
/// its recorded use-case results must satisfy.
pub struct EvaluationCase {
    pub id: &'static str,
    pub description: &'static str,
    pub steps: Vec<Step>,
    pub checks: Vec<Check>,
}

fn subscription_base_files() -> Vec<(&'static str, String)> {
    vec![
        (
            "src/billing/subscription.py",
            BASE_SUBSCRIPTION_PY.to_owned(),
        ),
        ("tests/test_subscription.py", BASE_TEST_PY.to_owned()),
        (
            ".context/features/subscriptions.yaml",
            FEATURE_YAML.to_owned(),
        ),
        (
            ".context/requirements/cancel-at-period-end.yaml",
            REQUIREMENT_YAML.to_owned(),
        ),
        (
            ".context/invariants/paid-entitlement.yaml",
            INVARIANT_YAML.to_owned(),
        ),
        (
            ".context/decisions/stripe-ordering.yaml",
            DECISION_YAML.to_owned(),
        ),
    ]
}

/// The full evaluation corpus. See `prompt.md`'s priority mission for the
/// minimum case list this satisfies: a meaningful behavior change,
/// formatting-only noise, an unrelated refactor, an in-file rename, a
/// cross-file symbol move, a deleted contract implementation, a changed DB
/// write, a stale semantic mapping, shared-test isolation, a newly added call
/// edge, and a realistic multi-commit history (as opposed to every other
/// case's single synthetic diff).
#[must_use]
pub fn corpus() -> Vec<EvaluationCase> {
    vec![
        cancellation_behavior_change(),
        formatting_only_noise(),
        unrelated_refactor_noise(),
        rename_or_move_noise(),
        symbol_move_across_files_noise(),
        deleted_contract_implementation(),
        changed_database_write(),
        stale_semantic_mapping(),
        shared_test_does_not_bridge_requirements(),
        added_call_discovers_intent(),
        multi_commit_feature_evolution(),
    ]
}

/// A real entitlement regression must surface both the invariant and the
/// requirement it breaks, and only those.
fn cancellation_behavior_change() -> EvaluationCase {
    let regressed = BASE_SUBSCRIPTION_PY.replace(
        "        if subscription.paid_until > now:\n            subscription.status = \"canceling\"\n        else:\n            subscription.status = \"inactive\"",
        "        subscription.status = \"inactive\"",
    );
    assert_ne!(
        regressed, BASE_SUBSCRIPTION_PY,
        "entitlement guard text moved"
    );
    EvaluationCase {
        id: "cancellation-behavior-change",
        description: "removing the paid_until guard must surface the invariant and requirement it enforces",
        steps: vec![
            Step::WriteFiles(subscription_base_files()),
            Step::Commit("base"),
            Step::Index,
            Step::WriteFiles(vec![("src/billing/subscription.py", regressed)]),
            Step::Review { base: "HEAD" },
        ],
        checks: vec![
            Check::ChangeKindIs {
                canonical_path: SUBSCRIPTION_SERVICE,
                kind: ChangeKind::BehaviorPotentiallyChanged,
            },
            Check::FindingIntentPresent("INV-SUB-003"),
            Check::FindingIntentPresent("REQ-SUB-014"),
            Check::FindingSeverity("INV-SUB-003", Severity::High),
            Check::FindingSeverity("REQ-SUB-014", Severity::High),
            Check::FindingIntentAbsent("ADR-SUB-001"),
        ],
    }
}

/// A whitespace-only change to a mapped symbol must stay silent.
fn formatting_only_noise() -> EvaluationCase {
    let reformatted = BASE_SUBSCRIPTION_PY.replace(
        "            subscription.status = \"canceling\"\n        else:",
        "            subscription.status = \"canceling\"\n\n        else:",
    );
    assert_ne!(
        reformatted, BASE_SUBSCRIPTION_PY,
        "formatting target text moved"
    );
    EvaluationCase {
        id: "formatting-only",
        description: "a whitespace-only edit to a mapped method must not be classified as a behavior change",
        steps: vec![
            Step::WriteFiles(subscription_base_files()),
            Step::Commit("base"),
            Step::Index,
            Step::WriteFiles(vec![("src/billing/subscription.py", reformatted)]),
            Step::Review { base: "HEAD" },
        ],
        checks: vec![
            Check::ChangeKindIs {
                canonical_path: SUBSCRIPTION_SERVICE,
                kind: ChangeKind::FormattingOnly,
            },
            Check::NoFindings,
            Check::FindingIntentAbsent("INV-SUB-003"),
            Check::FindingIntentAbsent("REQ-SUB-014"),
        ],
    }
}

/// A brand-new, unmapped helper must not manufacture findings out of thin air.
fn unrelated_refactor_noise() -> EvaluationCase {
    let helper = "def format_amount(cents: int) -> str:\n    return f\"${cents / 100:.2f}\"\n";
    EvaluationCase {
        id: "unrelated-refactor",
        description: "adding an unmapped helper elsewhere in the repository must not surface product findings",
        steps: vec![
            Step::WriteFiles(subscription_base_files()),
            Step::Commit("base"),
            Step::Index,
            Step::WriteFiles(vec![("src/billing/formatting.py", helper.to_owned())]),
            Step::Review { base: "HEAD" },
        ],
        checks: vec![
            Check::NoFindings,
            Check::FindingIntentAbsent("INV-SUB-003"),
            Check::FindingIntentAbsent("REQ-SUB-014"),
        ],
    }
}

/// Renaming a mapped method without touching its body must classify as a
/// rename and stay silent, since the explicit mapping still resolves to the
/// same stable node identity.
fn rename_or_move_noise() -> EvaluationCase {
    let renamed = BASE_SUBSCRIPTION_PY.replacen(
        "def cancel(self, subscription: Subscription, now: datetime) -> None:",
        "def cancel_subscription(self, subscription: Subscription, now: datetime) -> None:",
        1,
    );
    assert_ne!(
        renamed, BASE_SUBSCRIPTION_PY,
        "cancel definition text moved"
    );
    EvaluationCase {
        id: "rename-or-move",
        description: "renaming a mapped method with an unchanged body must classify as a rename and stay silent",
        steps: vec![
            Step::WriteFiles(subscription_base_files()),
            Step::Commit("base"),
            Step::Index,
            Step::WriteFiles(vec![("src/billing/subscription.py", renamed)]),
            Step::Review { base: "HEAD" },
        ],
        checks: vec![
            Check::ChangeKindIs {
                canonical_path: SUBSCRIPTION_SERVICE,
                kind: ChangeKind::Rename,
            },
            Check::NoFindings,
            Check::FindingIntentAbsent("INV-SUB-003"),
            Check::FindingIntentAbsent("REQ-SUB-014"),
        ],
    }
}

/// Relocate the mapped `SubscriptionService` class, body byte-identical, from
/// `subscription.py` to a new `cancellation.py`. Git reports this as one
/// `Modified` (old file, symbol removed) plus one `Added` (new file, symbol
/// present), never a `Renamed`. Before the cross-file move merge in
/// `resolve_changed_entities`, this produced two independent
/// `BehaviorPotentiallyChanged` entities sharing the same stored stable key
/// (fingerprint-matched), which doubled every finding on the untouched
/// invariant/requirement it implements.
fn symbol_move_across_files_noise() -> EvaluationCase {
    let without_service = "from dataclasses import dataclass\nfrom datetime import datetime\n\nfrom billing.cancellation import SubscriptionService\n\n\n@dataclass\nclass Subscription:\n    status: str\n    paid_until: datetime\n\n\nclass StripeWebhookHandler:\n    def handle_subscription_update(\n        self, subscription: Subscription, now: datetime\n    ) -> None:\n        SubscriptionService().cancel(subscription, now)\n";
    let moved_service = "from datetime import datetime\n\nfrom billing.subscription import Subscription\n\n\nclass SubscriptionService:\n    database = None\n\n    def cancel(self, subscription: Subscription, now: datetime) -> None:\n        if subscription.paid_until > now:\n            subscription.status = \"canceling\"\n        else:\n            subscription.status = \"inactive\"\n        if self.database is not None:\n            self.database.execute(\n                \"UPDATE subscriptions SET status = ? WHERE paid_until = ?\",\n                (subscription.status, subscription.paid_until),\n            )\n";
    EvaluationCase {
        id: "symbol-move-across-files",
        description: "moving a mapped class to a new file, body unchanged, must classify as a rename and stay silent",
        steps: vec![
            Step::WriteFiles(subscription_base_files()),
            Step::Commit("base"),
            Step::Index,
            Step::WriteFiles(vec![
                ("src/billing/subscription.py", without_service.to_owned()),
                ("src/billing/cancellation.py", moved_service.to_owned()),
            ]),
            Step::Review { base: "HEAD" },
        ],
        checks: vec![
            Check::ChangeKindIs {
                canonical_path: "billing.cancellation.SubscriptionService.cancel",
                kind: ChangeKind::Rename,
            },
            Check::NoFindings,
            Check::FindingIntentAbsent("INV-SUB-003"),
            Check::FindingIntentAbsent("REQ-SUB-014"),
        ],
    }
}

/// Deleting a decision-mapped integration point is a real contract change and
/// must be surfaced, without spuriously touching the untouched cancellation
/// invariant/requirement.
fn deleted_contract_implementation() -> EvaluationCase {
    let handler_removed = BASE_SUBSCRIPTION_PY
        .lines()
        .take_while(|line| !line.starts_with("class StripeWebhookHandler"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert!(
        !handler_removed.contains("StripeWebhookHandler"),
        "handler class was not removed"
    );
    EvaluationCase {
        id: "deleted-contract-implementation",
        description: "deleting the only implementation of a decision must surface it as a contract-impact finding",
        steps: vec![
            Step::WriteFiles(subscription_base_files()),
            Step::Commit("base"),
            Step::Index,
            Step::WriteFiles(vec![("src/billing/subscription.py", handler_removed)]),
            Step::Review { base: "HEAD" },
        ],
        checks: vec![
            Check::ChangeKindIs {
                canonical_path: STRIPE_HANDLER,
                kind: ChangeKind::BehaviorPotentiallyChanged,
            },
            Check::FindingIntentPresent("ADR-SUB-001"),
            Check::FindingSeverity("ADR-SUB-001", Severity::Medium),
            Check::FindingIntentAbsent("INV-SUB-003"),
            Check::FindingIntentAbsent("REQ-SUB-014"),
        ],
    }
}

/// The final missing fixture-matrix point: a mapped behavior changes the
/// database entity it writes. Review must expose both the product contracts
/// and the concrete write-change signal; after the commit is indexed, impact
/// and Context Pack must contain the new data contract and retire the old one.
fn changed_database_write() -> EvaluationCase {
    let changed = BASE_SUBSCRIPTION_PY.replace(
        "UPDATE subscriptions SET status = ? WHERE paid_until = ?",
        "INSERT INTO subscription_archive(status, paid_until) VALUES (?, ?)",
    );
    assert_ne!(changed, BASE_SUBSCRIPTION_PY, "database statement moved");
    EvaluationCase {
        id: "changed-database-write",
        description: "changing a mapped symbol's SQL write target must be reviewable and update impact/context data contracts",
        steps: vec![
            Step::WriteFiles(subscription_base_files()),
            Step::Commit("base"),
            Step::Index,
            Step::WriteFiles(vec![("src/billing/subscription.py", changed)]),
            Step::Review { base: "HEAD" },
            Step::Commit("archive canceled subscriptions"),
            Step::Index,
            Step::Impact {
                target: SUBSCRIPTION_SERVICE,
            },
            Step::Context {
                task: "archive subscription cancellation state",
                symbols: vec![SUBSCRIPTION_SERVICE],
                token_budget: 1_000,
            },
        ],
        checks: vec![
            Check::ChangeKindIs {
                canonical_path: SUBSCRIPTION_SERVICE,
                kind: ChangeKind::BehaviorPotentiallyChanged,
            },
            Check::ChangeSignalContains {
                canonical_path: SUBSCRIPTION_SERVICE,
                needle: "database writes changed: subscriptions -> subscription_archive",
            },
            Check::FindingIntentPresent("INV-SUB-003"),
            Check::FindingIntentPresent("REQ-SUB-014"),
            Check::FindingIntentAbsent("ADR-SUB-001"),
            Check::ImpactDataContractPresent("subscription_archive"),
            Check::ImpactDataContractAbsent("subscriptions"),
            Check::ContextIdentifierPresent("subscription_archive"),
            Check::ContextIdentifierAbsent("subscriptions"),
            Check::ContextWithinBudget,
        ],
    }
}

/// Once a real behavior change is committed and indexed, the human-verified
/// assertions it invalidates must become stale rather than silently staying
/// active or silently disappearing, and `ctx impact` must surface the
/// staleness as uncertainty rather than hiding it.
fn stale_semantic_mapping() -> EvaluationCase {
    let regressed = BASE_SUBSCRIPTION_PY.replace(
        "        if subscription.paid_until > now:\n            subscription.status = \"canceling\"\n        else:\n            subscription.status = \"inactive\"",
        "        subscription.status = \"inactive\"",
    );
    assert_ne!(
        regressed, BASE_SUBSCRIPTION_PY,
        "entitlement guard text moved"
    );
    EvaluationCase {
        id: "stale-semantic-mapping",
        description: "indexing a committed behavior change must mark the assertions it invalidates as stale, not silently active or silently gone",
        steps: vec![
            Step::WriteFiles(subscription_base_files()),
            Step::Commit("base"),
            Step::Index,
            Step::WriteFiles(vec![("src/billing/subscription.py", regressed)]),
            Step::Commit("regress entitlement guard"),
            Step::Index,
            Step::Review { base: "HEAD~1" },
            Step::Impact {
                target: SUBSCRIPTION_SERVICE,
            },
        ],
        checks: vec![
            Check::NoFindings,
            Check::StaleRelationshipContains("INV-SUB-003"),
            Check::StaleRelationshipContains("REQ-SUB-014"),
            Check::ImpactIntentPresent("INV-SUB-003"),
            Check::ImpactIntentPresent("REQ-SUB-014"),
        ],
    }
}

/// A test shared by two independent requirements must not bridge `ctx
/// impact`/`ctx context` traversal from one requirement's implementation into
/// the other's, matching the shared-node isolation fix recorded for
/// `ctx impact`/`ctx context` on this repository's own dogfood graph.
fn shared_test_does_not_bridge_requirements() -> EvaluationCase {
    let subscription_py = "from dataclasses import dataclass\nfrom datetime import datetime\n\n\n@dataclass\nclass Subscription:\n    status: str\n    paid_until: datetime\n\n\nclass SubscriptionService:\n    def cancel(self, subscription: Subscription, now: datetime) -> None:\n        if subscription.paid_until > now:\n            subscription.status = \"canceling\"\n        else:\n            subscription.status = \"inactive\"\n";
    let refunds_py = "from dataclasses import dataclass\n\n\n@dataclass\nclass Refund:\n    amount_cents: int\n    approved: bool\n\n\nclass RefundService:\n    def process(self, refund: Refund) -> None:\n        refund.approved = refund.amount_cents > 0\n";
    let shared_test_py = "from datetime import datetime, timedelta\n\nfrom billing.refunds import Refund, RefundService\nfrom billing.subscription import Subscription, SubscriptionService\n\n\ndef test_billing_workflow_end_to_end() -> None:\n    now = datetime.now()\n    subscription = Subscription(status=\"active\", paid_until=now + timedelta(days=10))\n    SubscriptionService().cancel(subscription, now)\n\n    refund = Refund(amount_cents=500, approved=False)\n    RefundService().process(refund)\n\n    assert subscription.status == \"canceling\"\n    assert refund.approved is True\n";
    let feature_subscriptions =
        "id: FEAT-SUBSCRIPTIONS\ntype: feature\nname: Subscription cancellation\nstatus: active\n";
    let feature_refunds =
        "id: FEAT-REFUNDS\ntype: feature\nname: Refund approval\nstatus: active\n";
    let requirement_subscriptions = "id: REQ-SUB-014\ntype: requirement\nfeature: FEAT-SUBSCRIPTIONS\nstatus: active\nstatement: >\n  When a paid user cancels, access must remain active until paid_until.\nimplementation:\n  - symbol: billing.subscription.SubscriptionService.cancel\ntests:\n  - symbol: tests.test_billing_workflow.test_billing_workflow_end_to_end\n";
    let requirement_refunds = "id: REQ-REFUND-001\ntype: requirement\nfeature: FEAT-REFUNDS\nstatus: active\nstatement: >\n  An approved refund must have a positive amount.\nimplementation:\n  - symbol: billing.refunds.RefundService.process\ntests:\n  - symbol: tests.test_billing_workflow.test_billing_workflow_end_to_end\n";
    EvaluationCase {
        id: "shared-test-isolation",
        description: "a test shared by two requirements must not bridge impact/context traversal between them",
        steps: vec![
            Step::WriteFiles(vec![
                ("src/billing/subscription.py", subscription_py.to_owned()),
                ("src/billing/refunds.py", refunds_py.to_owned()),
                ("tests/test_billing_workflow.py", shared_test_py.to_owned()),
                (
                    ".context/features/subscriptions.yaml",
                    feature_subscriptions.to_owned(),
                ),
                (".context/features/refunds.yaml", feature_refunds.to_owned()),
                (
                    ".context/requirements/cancel-at-period-end.yaml",
                    requirement_subscriptions.to_owned(),
                ),
                (
                    ".context/requirements/refund-approval.yaml",
                    requirement_refunds.to_owned(),
                ),
            ]),
            Step::Commit("base"),
            Step::Index,
            Step::Impact {
                target: SUBSCRIPTION_SERVICE,
            },
            Step::Context {
                task: "preserve entitlement when canceling a subscription",
                symbols: vec![SUBSCRIPTION_SERVICE],
                token_budget: 4_000,
            },
        ],
        checks: vec![
            Check::ImpactIntentPresent("REQ-SUB-014"),
            Check::ImpactIntentAbsent("REQ-REFUND-001"),
            Check::ContextIdentifierPresent("REQ-SUB-014"),
            Check::ContextIdentifierAbsent("REQ-REFUND-001"),
            Check::ContextWithinBudget,
        ],
    }
}

/// A brand-new caller added elsewhere in the repository, with no `.context`
/// mapping of its own, must still surface the product intent it now reaches:
/// the fresh structural (`FACT`) call edge to a mapped symbol grants the
/// caller the same one-hop semantic-discovery rights as any other direct
/// structural neighbor. This is the corpus's "added call" fixture point from
/// `prompt.md`'s evaluation matrix.
fn added_call_discovers_intent() -> EvaluationCase {
    const SCHEDULER_SWEEP: &str = "billing.scheduler.run_daily_cancellation_sweep";
    let scheduler_py = "from datetime import datetime\n\nfrom billing.subscription import Subscription, SubscriptionService\n\n\ndef run_daily_cancellation_sweep(subscription: Subscription, now: datetime) -> None:\n    SubscriptionService().cancel(subscription, now)\n";
    EvaluationCase {
        id: "added-call-discovers-intent",
        description: "a newly added, unmapped caller of a mapped symbol must discover that symbol's product intent through the fresh structural call edge",
        steps: vec![
            Step::WriteFiles(subscription_base_files()),
            Step::Commit("base"),
            Step::Index,
            Step::WriteFiles(vec![("src/billing/scheduler.py", scheduler_py.to_owned())]),
            Step::Commit("add daily cancellation sweep"),
            Step::Index,
            Step::Review { base: "HEAD~1" },
            Step::Impact {
                target: SCHEDULER_SWEEP,
            },
        ],
        checks: vec![
            Check::ChangeKindIs {
                canonical_path: SCHEDULER_SWEEP,
                kind: ChangeKind::BehaviorPotentiallyChanged,
            },
            Check::FindingIntentAbsent("INV-SUB-003"),
            Check::FindingIntentAbsent("REQ-SUB-014"),
            Check::ImpactIntentPresent("REQ-SUB-014"),
            Check::ImpactIntentPresent("INV-SUB-003"),
            Check::ImpactIntentAbsent("ADR-SUB-001"),
        ],
    }
}

/// A realistic three-commit history, unlike every other case's single
/// synthetic diff: a real behavior change (extending cancellation with a
/// grace period) followed by an unrelated signature-only follow-up (an
/// unused `dry_run` flag). Indexing the first (real) commit already marks
/// the invariant/requirement assertions on `cancel` stale, the same way
/// `stale_semantic_mapping` demonstrates for a single-commit case; nothing
/// in the second commit re-verifies them, so they are still stale, not
/// re-surfaced as fresh findings, when the whole span is reviewed at once.
/// Reviewing the whole span at once, the way a reviewer would look at a
/// multi-commit PR, must still classify the aggregate change correctly (a
/// changed public signature makes the whole span `ContractChanged`, not just
/// `BehaviorPotentiallyChanged`). The final impact query also guards against
/// the cross-file identity-conflation defect this repository hit
/// historically, this time across three sequential indexing transitions
/// instead of one.
fn multi_commit_feature_evolution() -> EvaluationCase {
    let grace_period_added = BASE_SUBSCRIPTION_PY
        .replace(
            "from datetime import datetime",
            "from datetime import datetime, timedelta",
        )
        .replace(
            "        if subscription.paid_until > now:\n            subscription.status = \"canceling\"\n        else:\n            subscription.status = \"inactive\"",
            "        grace_period = timedelta(days=3)\n        if subscription.paid_until + grace_period > now:\n            subscription.status = \"canceling\"\n        else:\n            subscription.status = \"inactive\"",
        );
    assert_ne!(
        grace_period_added, BASE_SUBSCRIPTION_PY,
        "grace period text moved"
    );
    let dry_run_param_added = grace_period_added.replace(
        "def cancel(self, subscription: Subscription, now: datetime) -> None:",
        "def cancel(self, subscription: Subscription, now: datetime, *, dry_run: bool = False) -> None:",
    );
    assert_ne!(
        dry_run_param_added, grace_period_added,
        "dry_run parameter text moved"
    );
    EvaluationCase {
        id: "multi-commit-feature-evolution",
        description: "a real three-commit history must classify and score correctly when reviewed as one span, and identity must survive three sequential indexing transitions",
        steps: vec![
            Step::WriteFiles(subscription_base_files()),
            Step::Commit("base"),
            Step::Index,
            Step::WriteFiles(vec![("src/billing/subscription.py", grace_period_added)]),
            Step::Commit("add grace period to cancellation"),
            Step::Index,
            Step::WriteFiles(vec![("src/billing/subscription.py", dry_run_param_added)]),
            Step::Commit("add dry_run flag to cancel"),
            Step::Index,
            Step::Review { base: "HEAD~2" },
            Step::Impact {
                target: SUBSCRIPTION_SERVICE,
            },
        ],
        checks: vec![
            Check::ChangeKindIs {
                canonical_path: SUBSCRIPTION_SERVICE,
                kind: ChangeKind::ContractChanged,
            },
            Check::NoFindings,
            Check::StaleRelationshipContains("INV-SUB-003"),
            Check::StaleRelationshipContains("REQ-SUB-014"),
            Check::FindingIntentAbsent("ADR-SUB-001"),
            Check::ImpactIntentPresent("REQ-SUB-014"),
            Check::ImpactIntentPresent("INV-SUB-003"),
        ],
    }
}

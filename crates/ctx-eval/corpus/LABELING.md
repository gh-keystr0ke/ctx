Version: 2

# Decision point labels

## Label rules

**Significant.** The point implements a documented rule that affects users or the business.
Use at least one relevant source from levels 1 through 3.

1. User documentation or an API reference.
2. A specification, RFC, or ADR.
3. An issue or bug referenced by a commit that added or changed the point.
4. A test that checks the behavior. A test can support evidence from levels 1 through 3.

**Trivial.** The point performs a routine type check, emptiness check, error propagation, logging, or metrics.
Record a category. Evidence is optional.
Examples of routine propagation include Rust `?`, Go `if err != nil { return err }`, and a Python re-raise with no added behavior.
Classify the anchored expression or branch. Consider its callers when its effect is unclear.
For Rust `?`, check whether the operand also selects a fallback or enforces a product rule.
Adding diagnostic context alone still counts as routine propagation.

**Unclear.** Use this label if evidence is missing, the source describes different behavior, or the reviewer is unsure.
Metrics exclude this label.

Use one category from this list.

- `limit`
- `state-transition`
- `permission`
- `default`
- `retry` for retry or timeout
- `validation`
- `error-handling`
- `observability` for logging or metrics

## Borderline cases

| Decision point | Label rule |
| --- | --- |
| Documented user-facing limit, such as message length | Significant |
| Internal buffer or batch limit | Significant if documented, otherwise unclear |
| Configuration default | Significant if documented, otherwise unclear |
| Feature flag | Significant if documented, otherwise unclear |
| Retry or timeout | Significant if evidence specifies when attempts stop, otherwise unclear |
| Permission check | Significant when evidence supports the permission rule |
| Input format or range check | Significant if evidence describes the rule, otherwise unclear |
| Routine nonempty check | Trivial |
| Compatibility branch for an API version or old format | Significant when an API reference describes it |
| Logging or metrics with no other behavior | Trivial |
| Error return forwards the failure with an optional diagnostic message and no changed outcome | Trivial |
| Failed lookup becomes a permission decision, domain outcome, or distinct API status | Significant if a source identifies that outcome for the condition, otherwise unclear |
| Internal work is skipped because the requested state already holds | Trivial |
| Data moves between equivalent representations with the same values and meaning | Trivial |
| A conversion merges cases, drops data, or picks a default | Significant if a source establishes the effect, otherwise unclear |
| A branch enforces an API field's availability, an on/off option, or a documented scheduler predicate | Significant when the source specifies the rule and the code implements it |

A source that describes a feature in general does not establish each branch in its implementation.
For example, a routing overview does not establish when a metrics-only setting starts a routing call.

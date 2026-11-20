Version: 4

# Decision point labels

## Label rules

**Significant.** The point implements a documented rule that affects users or the business.
Use at least one relevant source from levels 1 through 3.

1. User documentation or an API reference. An OpenAPI or Swagger file in the pinned code repository is an API reference.
2. A specification, RFC, or ADR.
3. An issue, bug, or pull request referenced by a commit that added or changed the point.
4. A test that checks the behavior. A test can support evidence from levels 1 through 3.

A source establishes a point only when it states the condition and the effect of that point.
A purpose statement for a larger change does not establish each branch that the change adds.
Cite each source at the pinned revision.
If you find a passage in living documentation, find the same text at the pinned revision and cite that location.
If the pinned revision does not contain the text, do not use the passage.

**Trivial.** The point performs a routine type check, emptiness check, error propagation, logging, or metrics.
Record a category. Evidence is optional.
Examples of routine propagation include Rust `?`, Go `if err != nil { return err }`, and a Python re-raise with no added behavior.
Classify the anchored expression or branch. Consider its callers when its effect is unclear.
For Rust `?`, check whether the operand also selects a fallback or enforces a product rule.
Adding diagnostic context alone still counts as routine propagation.

**Unclear.** Use this label if evidence is missing, the source describes different behavior, or the reviewer is unsure.
Before you use this label, search the pinned documentation and API reference, the specifications, and the issues or pull requests referenced by the commits that added or changed the point.
In the note, name the sources that you searched and state why they do not establish the point.
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
| A fallback arm only logs or returns its input unchanged, and other arms implement the rule | Trivial |
| A check passes because the request specifies no requirement to check | Trivial |
| A check passes or fails because the inspected resource lacks data that a requirement needs | Significant if a source states that outcome, otherwise unclear |
| A lookup continues in the next location after a not-found result, and the final outcome equals one search of all locations | Trivial |
| Test-only code controls the test run, for example with an assertion, an invariant panic, a shrinker, or a random yield | Trivial, category `validation` |
| A test model reproduces a product rule | Use the label of that product rule |

A source that describes a feature in general does not establish each branch in its implementation.
For example, a routing overview does not establish when a metrics-only setting starts a routing call.

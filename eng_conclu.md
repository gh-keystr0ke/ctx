# Implementation Prompt — ctx

Ты — principal Rust engineer, software architect и product-minded technical founder. Ты реализуешь **ctx** — local-first persistent context layer для coding agents.

Твоя задача — не просто написать работающий код, а построить небольшой, хорошо спроектированный фундамент продукта, который можно развивать без последующего полного rewrite.

При каждом решении задавай два вопроса:

1. Это действительно нужно текущему vertical slice?
2. Делает ли это систему проще для изменения, тестирования и повторного использования?

Если ответ на первый вопрос «нет» — не реализуй это сейчас.

---

# 1. Что такое ctx

`ctx` связывает **product/business intent** с конкретной реализацией в коде.

Обычный code intelligence отвечает:

- кто кого вызывает;
- где объявлен symbol;
- какие файлы зависят друг от друга.

`ctx` должен дополнительно отвечать:

> Почему этот код существует и какой продуктовый контракт может быть нарушен его изменением?

Пример:

```text
Feature: Subscription cancellation

→ Requirement: paid user retains access until paid_until

→ Invariant:
  cancellation must not revoke already-paid entitlement

→ SubscriptionService.cancel()

→ subscriptions.status
→ subscriptions.paid_until

→ StripeWebhookHandler

→ tests
```

Главный продуктовый сценарий:

```bash
ctx review
```

который для git diff должен показать:

- какое поведение потенциально изменилось;
- какие requirements затронуты;
- какие invariants связаны с изменённым кодом;
- какие tests относятся к этому поведению;
- какие semantic links сомнительны или stale;
- почему ctx пришёл к каждому выводу.

Главное продуктовое обещание:

> Know which product contracts a code change touches before you merge it.

Не строить generic graph platform.

---

# 2. Основные ограничения

Архитектура обязана соблюдать:

- local-first;
- source code не требует отправки во внешний cloud;
- LLM полностью optional;
- продукт полезен без LLM;
- deterministic analysis предпочтительнее inference;
- любое inference имеет provenance;
- inference никогда автоматически не становится fact;
- SQLite — primary storage MVP;
- Tree-sitter — parsing MVP;
- Git — основа incremental indexing;
- Markdown/YAML — business context;
- MCP — delivery mechanism для coding agents;
- CLI — основной UX MVP;
- никакого UI до доказанной полезности CLI;
- никакого Neo4j до profiling, доказывающего необходимость;
- начать с одного языка;
- избегать premature distributed/cloud architecture.

---

# 3. Основной архитектурный принцип

Центральная модель системы — не «graph of things».

Она должна рассматриваться как:

```text
versioned claims
+
evidence
+
validity
+
relationships between product intent and implementation
```

То есть:

```text
Claim
+
Evidence
+
Validity
```

важнее самого Graph API.

Graph — способ выполнять traversal поверх этих claims.

---

# 4. Engineering principles

Используй следующие принципы совместно, а не догматически.

## Functional Core / Imperative Shell

Это главный архитектурный принцип проекта.

### Functional Core

Максимум бизнес-логики должен быть pure или практически pure.

Core не должен самостоятельно:

- читать filesystem;
- вызывать git;
- работать с SQLite;
- читать environment;
- обращаться к network;
- вызывать LLM;
- получать текущее время;
- печатать CLI output.

Примеры функций core:

```rust
plan_incremental_index(...)
resolve_changed_entities(...)
classify_behavior_change(...)
resolve_semantic_candidates(...)
calculate_relation_confidence(...)
rank_context_candidates(...)
compile_context_pack(...)
build_review_findings(...)
determine_stale_relations(...)
```

Input → deterministic output.

Такие функции должны легко тестироваться без mocks.

### Imperative Shell

Shell отвечает за:

- filesystem;
- Git;
- SQLite transactions;
- Tree-sitter execution;
- terminal;
- MCP;
- optional external integrations.

Типичная структура use case:

```text
load inputs
    ↓
convert into domain values
    ↓
call pure core
    ↓
persist effects
    ↓
render result
```

Не смешивай эти этапы.

---

# 5. Object Calisthenics

Используй Object Calisthenics как heuristics, не как религию.

Главная цель — снижать cognitive complexity.

## Prefer value objects over primitive obsession

Плохо:

```rust
fn edge(
    src: i64,
    dst: i64,
    confidence: f64,
    commit: String,
)
```

Лучше:

```rust
NodeId
Confidence
CommitOid
StableKey
```

Создавай domain types там, где primitive имеет бизнес-семантику или ограничения.

Например:

```rust
pub struct Confidence(f32);

impl Confidence {
    pub fn new(value: f32) -> Result<Self, InvalidConfidence> {
        ...
    }
}
```

Невалидное состояние желательно сделать непредставимым.

---

## Keep nesting shallow

Предпочитай:

- early return;
- guard clauses;
- маленькие функции;
- `?`;
- exhaustive `match`;
- iterators там, где они увеличивают ясность.

Не создавай искусственные helper-функции только ради формального ограничения indentation.

---

## Small focused types

Если struct начинает отвечать одновременно за:

- persistence;
- graph traversal;
- ranking;
- formatting;

раздели ответственность.

---

## Avoid getters/setters architecture

Domain object должен выражать intent через поведение.

Но DTO/read models могут быть простыми immutable structs с public fields.

Не создавай boilerplate API без пользы.

---

## Wrap important collections

Если коллекция имеет собственную семантику, дай ей имя.

Например:

```rust
ChangedSymbols
TraversalFrontier
EvidenceSet
ContextCandidates
```

Но не оборачивай каждый `Vec<T>` автоматически.

---

# 6. SOLID

Применяй SOLID на уровне Rust idioms.

## SRP

Module/type должен иметь одну понятную причину измениться.

## OCP

Используй extension points только там, где вариативность уже очевидна.

Например language analyzer действительно требует abstraction.

Не создавай interfaces «на будущее».

## LSP

Implementations одного trait должны сохранять его semantic contract.

## ISP

Traits должны быть маленькими.

Плохо:

```rust
trait Repository {
    fn load_node(...);
    fn save_node(...);
    fn parse_file(...);
    fn execute_git(...);
    fn find_requirements(...);
    ...
}
```

Лучше узкие capabilities.

## DIP

Domain/core не зависит от SQLite, Git, Tree-sitter, MCP или CLI.

Dependencies направлены внутрь.

---

# 7. Не переабстрагировать

Не реализуй «Clean Architecture enterprise edition».

Особенно запрещено без реальной необходимости:

- abstract factories;
- repository interface для каждого SQL table;
- generic persistence framework;
- event bus;
- internal message broker;
- dependency injection framework;
- generic graph database abstraction;
- generic query language;
- plugin framework;
- generic ontology engine.

Предпочитай concrete implementation, пока не появилась реальная вторая implementation.

Пример:

SQLite может быть concrete implementation.

При этом SQL не должен проникнуть в domain algorithms.

Это достаточная граница.

---

# 8. Rust workspace

Начальная рекомендуемая структура:

```text
ctx/
├── Cargo.toml
├── crates/
│   ├── ctx-core/
│   ├── ctx-app/
│   ├── ctx-adapters/
│   ├── ctx-cli/
│   └── ctx-mcp/
└── fixtures/
```

Не добавляй новый crate без серьёзной причины.

---

# 9. ctx-core

Это Functional Core.

Он не должен зависеть от:

- rusqlite;
- git libraries;
- tree-sitter;
- clap;
- MCP libraries;
- network clients.

Содержит:

```text
domain/
graph/
indexing/
resolution/
impact/
review/
context/
provenance/
```

---

## Domain entities

Минимальный набор node kinds:

```rust
enum NodeKind {
    Feature,
    Requirement,
    Invariant,
    Decision,

    DomainConcept,
    ExternalSystem,

    File,
    CodeSymbol,
    Endpoint,
    DbEntity,
    Event,
}
```

Test не обязательно отдельная storage entity.

Можно использовать:

```text
CodeSymbol(kind = test)
```

если это не ухудшает запросы.

---

# 10. Product intent types

## Feature

Required:

```text
stable id
name
status
```

Optional:

```text
description
owner
tags
```

Stable ID задаётся человеком:

```text
FEAT-SUBSCRIPTIONS
```

---

## Requirement

Required:

```text
id
statement
status
```

Optional:

```text
feature
category
priority
owner
```

Например:

```text
REQ-SUB-014
```

ID не меняется при редактировании формулировки.

---

## Invariant

Invariant — особенно важный semantic object.

Это утверждение, нарушение которого считается bug.

Например:

```text
INV-SUB-003

Paid entitlement must not terminate before paid_until.
```

Fields:

```text
id
statement
scope
severity?
rationale?
exceptions?
```

---

## Decision

Минимальный ADR:

```text
id
title
status
decision
rationale?
```

---

# 11. Implementation entities

## File

Identity:

```text
repository + normalized path
```

---

## CodeSymbol

Identity нельзя основывать на line numbers.

Использовать:

```text
canonical symbol path
+
kind
+
parent
+
structural fingerprint
```

Например:

```text
billing.subscription.SubscriptionService.cancel
```

Fields:

```text
language
kind
canonical_path
file
source_range
signature?
visibility?
body_hash
structural_fingerprint
```

---

## Endpoint

Например:

```text
HTTP POST /subscriptions/{id}/cancel
```

---

## DbEntity

Для MVP достаточно:

```text
table
column
```

Не моделировать полноценный DB engine.

---

# 12. Relations

Разделяй structural relations и semantic relations.

## Structural

```text
CONTAINS
CALLS
REFERENCES
READS_FROM
WRITES_TO
EMITS
HANDLES
```

Обычно:

```text
epistemic_class = FACT
provenance = StaticAnalysis
confidence = 1
```

если анализ действительно deterministic.

---

## Semantic

```text
IMPLEMENTS
ENFORCES
COVERED_BY
DEPENDS_ON
SATISFIES
```

Пример:

```text
SubscriptionService.cancel
    ENFORCES
INV-SUB-003
```

Human-confirmed relation:

```text
epistemic_class = ASSERTION
provenance = Human
```

Machine-proposed relation:

```text
epistemic_class = INFERENCE
```

---

# 13. Epistemic model

Обязательно различать:

```rust
enum ClaimClass {
    Fact,
    Assertion,
    Inference,
}
```

## FACT

Deterministically observable.

Например:

```text
A CALLS B
symbol is defined in file
function writes DB column
```

## ASSERTION

Кто-то явно утверждает semantic relationship:

```text
human
documentation
explicit business context
```

## INFERENCE

System считает relationship вероятным.

Например:

```text
heuristic
embedding
LLM
```

---

# 14. Critical inference rule

Inference никогда автоматически не становится Fact или Assertion.

Human confirmation может сделать:

```text
Inference
```

основанием для нового:

```text
Assertion(provenance = Human)
```

Но исходная inference должна остаться в history/evidence.

Также:

> inference не должна бесконтрольно усиливать другую inference.

В MVP запрещай длинные reasoning chains через inferred edges.

Default:

```text
max inferred-edge depth = 1
```

---

# 15. Provenance

Для любого semantic relationship система должна уметь ответить:

```bash
ctx explain ...
```

и показать:

> Почему ctx считает эту связь существующей?

Минимальная provenance information:

```text
source kind
source URI/location
commit
author/origin
timestamp
confidence
evidence
validity
producer
```

Provenance является обязательной частью data model с первого дня.

Не добавлять её «позже».

---

# 16. Storage — SQLite

SQLite является source of truth MVP.

Graph nature domain model не означает необходимость graph DB.

Typical traversal ограниченный и typed:

```text
changed symbol
 → IMPLEMENTS / ENFORCES
 → requirement / invariant
 → COVERED_BY
 → tests
```

Graph algorithms живут в Rust.

SQLite хранит данные.

---

# 17. Storage schema

Минимальные tables:

```text
repositories
commits
nodes
node_versions
edges
sources
evidence
edge_evidence
annotations
aliases
derivations
```

---

## repositories

Conceptually:

```sql
id
root_path
remote_url
created_at
```

---

## commits

```sql
id
repository_id
oid
parent_oid
authored_at
indexed_at
```

Unique:

```text
(repository_id, oid)
```

---

## nodes

Node хранит stable identity:

```sql
id
repository_id
kind
stable_key
created_commit
retired_commit
```

Unique:

```text
(repository_id, kind, stable_key)
```

---

## node_versions

```sql
id
node_id
valid_from
valid_to
name
content_hash
attributes_json
```

---

## edges

```sql
id
repository_id

src_node_id
dst_node_id
kind

epistemic_class
provenance_kind
confidence

status

valid_from
valid_to

producer
fingerprint
```

Statuses:

```text
active
stale
rejected
```

Indexes как минимум:

```text
(src_node_id, kind, valid_to)
(dst_node_id, kind, valid_to)
(kind, valid_to)
```

---

## sources

```text
id
repository_id
kind
uri
commit_id
author
timestamp
content_hash
metadata
```

Kinds:

```text
StaticAnalysis
Human
Documentation
LLMInference
Runtime
ExternalSystem
```

---

## evidence

```text
id
source_id
locator
excerpt_hash
strength
attributes
```

---

## edge_evidence

Many-to-many:

```text
edge_id
evidence_id
```

---

## annotations

Human decisions:

```text
confirm
reject
comment
```

Reject — тоже meaningful information.

Не предлагать человеку одну и ту же отвергнутую связь снова без новых evidence.

---

## aliases

Domain and symbol aliases.

---

## derivations

Критически важно для incremental invalidation.

Хранить:

```text
derived entity/relation
producer
source
input fingerprint/hash
```

Нужно иметь возможность определить:

> Какой analyzer создал этот edge и от какого input он зависит?

---

# 18. SQLite usage

Использовать:

- WAL;
- transactions;
- batch writes;
- prepared statements;
- необходимые adjacency indexes.

Не оптимизировать graph traversal заранее.

Если profiling позже покажет проблему:

```text
SQLite source of truth
+
in-memory Rust adjacency index
```

является предпочтительным первым шагом перед graph DB.

Neo4j и другие graph DB не добавлять без benchmark.

---

# 19. ctx-app

`ctx-app` — imperative orchestration layer.

Он реализует use cases:

```text
init
index
status
impact
explain
review
context
verify
```

Core logic должен оставаться в `ctx-core`.

---

# 20. Ports

Определяй traits только на meaningful IO boundaries.

Примерно:

```rust
trait GraphStore {
    ...
}

trait SourceReader {
    ...
}

trait GitRepository {
    ...
}

trait LanguageAnalyzer {
    ...
}
```

Traits должны быть минимальными под реальные use cases.

Не пытайся заранее абстрагировать абсолютно все инфраструктурные операции.

---

# 21. ctx-adapters

Здесь находятся concrete imperative adapters:

```text
sqlite/
git/
tree_sitter/
business_context/
```

Для MVP поддержать один programming language.

Если язык заранее не выбран существующим repository, предпочесть Python как первый vertical slice, но не hardcode assumptions в core.

---

# 22. Normalized Code IR

Tree-sitter adapter не должен напрямую создавать SQLite records.

Он возвращает normalized IR.

Например:

```rust
struct FileAnalysis {
    file: AnalyzedFile,
    symbols: Vec<SymbolDefinition>,
    references: Vec<SymbolReference>,
    calls: Vec<CallSite>,
    database_accesses: Vec<DatabaseAccess>,
    tests: Vec<TestDefinition>,
}
```

Normalized IR принадлежит core boundary.

Tree-sitter-specific node types не должны вытекать наружу parser adapter.

---

# 23. Incremental indexing

Не реализовывай incremental AST parsing между commits.

На MVP:

```text
changed file
→ full parse changed file
```

Repository indexing должен быть incremental.

Algorithm:

```text
previous indexed commit
        ↓
git diff --name-status
        ↓
changed / added / deleted / renamed files
        ↓
parse affected current files
        ↓
normalized IR
        ↓
match previous/current entities
        ↓
calculate IndexPlan
        ↓
close obsolete versions
        ↓
create new versions
        ↓
invalidate derived structural relations
        ↓
recompute affected relations
        ↓
re-resolve affected references
        ↓
mark affected semantic links stale when appropriate
```

---

# 24. Make IndexPlan pure

Очень желательно иметь pure representation:

```rust
struct IndexPlan {
    nodes_to_create: ...,
    nodes_to_version: ...,
    nodes_to_retire: ...,
    edges_to_close: ...,
    edges_to_create: ...,
    relations_to_mark_stale: ...,
}
```

Flow:

```text
old snapshot + new FileAnalysis + GitDiff
              ↓
       pure planner
              ↓
          IndexPlan
              ↓
      SQLite transaction
```

Это делает indexing testable без database.

---

# 25. Symbol identity / rename detection

Sequential strategy:

1. exact canonical symbol path;
2. parent + name + signature;
3. structural fingerprint;
4. git rename/move evidence;
5. normalized AST similarity.

Line numbers не являются identity.

При высокой уверенности сохраняй stable NodeId и создавай новый `node_version`.

Не создавай sophisticated universal entity identity algorithm заранее.

---

# 26. Structural edge invalidation

Derived structural relation должна знать producer/input.

Например:

```text
A CALLS B

producer = python_call_resolver
source = src/a.py
input_hash = ...
```

Если `src/a.py` изменился:

```text
invalidate relations owned by analyzer result for src/a.py
recompute them
```

Не сканировать весь graph.

---

# 27. Semantic edge invalidation

Semantic relationship нельзя просто удалять при изменении symbol.

Например human подтвердил:

```text
cancel() ENFORCES INV-SUB-003
```

Если body `cancel()` существенно изменился:

```text
relation.status = stale
```

или:

```text
needs verification
```

если введёшь отдельный state.

Причина staleness должна сохраняться.

---

# 28. Business context

Business context находится в Git:

```text
.context/
├── features/
├── requirements/
├── invariants/
└── decisions/
```

Не создавать UseCase и BusinessRule как отдельные сущности, пока не доказано, что движку требуется различное поведение.

Requirement может иметь:

```yaml
category: use-case
```

или:

```yaml
category: business-rule
```

---

# 29. Example requirement

```yaml
id: REQ-SUB-014
type: requirement
feature: FEAT-SUBSCRIPTIONS
status: active

statement: >
  When a user cancels a paid subscription,
  access must remain active until paid_until.

implementation:
  - symbol: billing.subscription.SubscriptionService.cancel
```

Explicit links — самый ценный semantic signal MVP.

---

# 30. Entity resolution

Строго разделяй:

```text
Code Resolution
```

и:

```text
Semantic Resolution
```

Это разные задачи.

---

# 31. Code resolution

Prefer deterministic mechanisms:

1. exact identifier;
2. imports;
3. namespace/module;
4. parent symbol;
5. available type information;
6. lexical fallback.

LLM для code reference resolution не использовать без исключительной причины.

---

# 32. Semantic resolution pipeline

Порядок:

1. explicit annotations/mappings;
2. existing human-verified mappings;
3. aliases;
4. canonical symbol/path similarity;
5. lexical evidence;
6. graph evidence;
7. test correlation;
8. optional embeddings;
9. optional LLM reranking;
10. human verification.

---

# 33. Semantic scoring

Score должен объясняться через отдельные signals.

Не возвращай opaque floating number без breakdown.

Пример:

```rust
struct ResolutionScore {
    explicit: Score,
    alias: Score,
    lexical: Score,
    structural: Score,
    test_correlation: Score,
    semantic_similarity: Option<Score>,
}
```

Aggregate score может выглядеть примерно:

```text
0.35 explicit/alias
0.20 lexical
0.20 structural graph evidence
0.10 test correlation
0.10 semantic similarity
0.05 locality
```

Но weights должны быть constants/configuration и тестироваться на evaluation corpus.

Не относись к score как к calibrated probability.

---

# 34. Context Compiler

Input:

```text
task
+
optional diff
+
optional files
+
optional symbols
+
token budget
```

Output:

```text
bounded ContextPack
```

---

## Seed detection

Из diff:

```text
changed symbols
changed endpoints
changed DB access
changed tests
```

Из task:

```text
stable IDs
domain terms
symbol names
business concepts
```

---

## Traversal

Не использовать blind BFS:

```text
neighbors(depth = 4)
```

Использовать typed traversal policies.

Например bug fix:

```text
changed symbol
→ ENFORCES invariant
→ IMPLEMENTS requirement
→ COVERED_BY test
→ selected callers/callees
```

Refactor может иметь другую policy.

---

# 35. Context ranking

Candidate utility должна учитывать:

```text
task relevance
relationship type
relationship confidence
provenance trust
graph distance
freshness
ownership/locality
token cost
```

Conceptual:

```text
utility =
    task_relevance
  + relationship_prior
  + evidence_quality
  + proximity
  + freshness
  + provenance_trust
```

Selection:

```text
value = utility / estimated_token_cost
```

Но не допускай, чтобы маленькие малополезные nodes вытеснили critical invariant только из-за размера.

Введи semantic priority tiers.

---

# 36. Context Pack priority

При уменьшении token budget сохранять в первую очередь:

1. changed behavior summary;
2. invariants;
3. requirements;
4. directly relevant implementation;
5. tests;
6. DB/external contracts;
7. adjacent implementation;
8. low-confidence context.

---

# 37. ctx review

Это killer feature.

Она важнее generic graph query.

Pipeline:

```text
git diff
   ↓
changed entities
   ↓
behavior classification
   ↓
semantic blast radius
   ↓
requirement/invariant impact
   ↓
test correlation
   ↓
confidence filtering
   ↓
evidence-backed findings
```

---

# 38. Behavioral change classification

Минимальные categories:

```rust
enum ChangeKind {
    FormattingOnly,
    Rename,
    RefactorLikely,
    BehaviorPotentiallyChanged,
    ContractChanged,
    Unknown,
}
```

Signals могут включать:

```text
condition changed
return changed
exception changed
DB write changed
external call changed
event changed
public API/signature changed
```

Не пытайся доказать arbitrary semantic equivalence.

Цель — отсеять очевидный noise.

---

# 39. Review findings

Не писать:

```text
Invariant violated.
```

если система этого не доказала.

Правильно:

```text
Change affects code responsible for enforcing INV-SUB-003.
```

или, если deterministic analysis действительно показывает риск:

```text
Potential violation:
subscription.status is now written before the paid_until guard.
```

Каждый finding обязан иметь:

```text
severity
confidence
changed entity
affected intent
reason
evidence
related tests
uncertainty
```

---

# 40. Review confidence

Используй conservative composition.

Например:

```text
confidence = min(
    behavior_change_confidence,
    semantic_relation_confidence,
    freshness_confidence,
    evidence_confidence
)
```

Не позволяй нескольким слабым assumptions искусственно превращаться в high confidence.

Default presentation:

```text
high     >= 0.85
medium   >= 0.65
low      hidden unless --verbose
```

Thresholds не считать окончательными — их нужно калибровать.

Главная product metric:

```text
precision of actionable findings
```

False positive значительно опаснее missed low-value finding.

---

# 41. ctx impact

Input:

```bash
ctx impact <file|symbol>
```

Output:

```text
changed/selected symbol

product intent:
  features
  requirements
  invariants

implementation:
  relevant callers/callees
  DB entities
  endpoints
  external systems

verification:
  tests

uncertainty:
  stale/inferred relationships
```

Bound traversal.

Не возвращать весь reachable graph.

---

# 42. ctx explain

Это first-class product capability.

Examples:

```bash
ctx explain REQ-SUB-014
```

или relation:

```bash
ctx explain \
  "billing.subscription.SubscriptionService.cancel -> REQ-SUB-014"
```

Output должен показывать:

```text
claim
claim class
status
confidence
validity
provenance
evidence chain
human confirmations/rejections
```

Explanation строится из stored evidence.

LLM не должен придумывать rationale после факта.

---

# 43. Human-in-the-loop

Implement later, после работающего explicit-link review.

Command:

```bash
ctx verify
```

UX:

```text
REQ-SUB-014

Possible implementation:
SubscriptionService.cancel

Evidence:
- identifier similarity
- reads paid_until
- writes subscriptions.status
- related test names

Confidence: 0.82

[y] accept
[n] reject
[s] skip
[e] explain
```

Human interaction должно минимизировать annotation workload.

Если система требует вручную размечать весь repository — architecture/product approach неверны.

---

# 44. Verification priority

Не сортировать только по confidence.

Предпочитать relations с высоким expected impact:

```text
semantic importance
× uncertainty
× change frequency
× usage by review/context
```

Invariant, через который проходят десятки PR, важнее редко используемого helper.

---

# 45. CLI

Первая API surface:

```bash
ctx init
ctx index
ctx status
ctx impact
ctx explain
ctx review
ctx context
ctx verify
ctx serve --mcp
```

Не строить generic query language на первом этапе.

Все meaningful commands должны поддерживать machine-readable output:

```bash
--json
```

Domain/use-case layer не должен знать, как выглядит terminal rendering.

---

# 46. MCP

MCP — thin adapter над теми же use cases.

Минимальные tools:

```text
get_context
get_impact
explain_relation
find_requirements
review_change
```

Не дублировать business logic между CLI и MCP.

Это важное требование переиспользования.

Architecture:

```text
              ctx-core
                 ↑
              ctx-app
             ↑       ↑
          ctx-cli   ctx-mcp
```

CLI и MCP должны вызывать одни и те же application use cases.

---

# 47. Error handling

Используй typed errors там, где caller способен осмысленно отличать причины.

Не делай один гигантский:

```rust
enum Error {
    Everything(String)
}
```

Также не создавай сотню micro-error enums.

Ошибки на boundaries могут добавлять context.

Production code не должен полагаться на `unwrap()` для обычных runtime situations.

`expect()` допустим только там, где invariant действительно доказан и сообщение объясняет его.

---

# 48. Observability

CLI должен иметь диагностический режим:

```bash
-v
-vv
```

или equivalent.

Полезно видеть:

```text
files reparsed
symbols changed
edges invalidated
edges recomputed
semantic links marked stale
index duration
```

Не добавлять полноценный telemetry platform.

---

# 49. Determinism

Одинаковый:

```text
repository state
+
configuration
```

без optional inference должен давать одинаковый graph/result.

Используй stable ordering там, где output может попасть в:

- snapshots;
- tests;
- code review;
- agent context.

Не позволяй HashMap iteration order влиять на CLI/MCP responses.

---

# 50. Configuration

Не строить огромную config system.

Достаточно:

```text
.ctx/config.toml
```

Минимально:

```toml
language = "python"

[paths]
include = ["src", "tests"]
exclude = ["generated", "vendor"]
```

Добавлять config options только когда появляется use case.

---

# 51. Generated/vendor code

Default exclude или low priority.

Не засорять semantic graph:

```text
vendor
generated
build
dist
target
virtualenv
```

Конкретный список зависит от языка.

---

# 52. Testing strategy

Functional Core должен иметь много дешёвых unit tests.

Adapters — integration tests.

CLI — несколько end-to-end fixture repositories.

Test pyramid примерно:

```text
many pure domain tests
        ↓
adapter integration tests
        ↓
few end-to-end repository scenarios
```

Не mock SQLite, если integration test с temporary SQLite проще и надёжнее.

Не mock Tree-sitter internals.

Тестируй boundary result.

---

# 53. Fixture repositories

Создай маленькие realistic fixtures.

Например subscriptions repository:

```text
subscription service
cancel()
stripe webhook
subscriptions table
paid_until
tests
.context requirements
.context invariants
```

Fixtures должны покрывать:

1. normal index;
2. function body change;
3. rename;
4. function move;
5. deleted symbol;
6. added call;
7. changed DB write;
8. stale semantic relation;
9. linked test;
10. unrelated refactor.

---

# 54. Evaluation mindset

Не оптимизировать только unit test coverage.

Ключевые product qualities:

```text
review precision
semantic link precision
incremental indexing correctness
stale edge correctness
context relevance
```

Создавай небольшой evaluation harness по мере появления соответствующего functionality.

---

# 55. Performance

Не оптимизировать без measurement.

Но избегать очевидных проблем:

- N+1 SQLite queries;
- transaction per edge;
- full repository rescan;
- whole graph BFS;
- repeated parsing одного файла;
- loading source contents без необходимости.

Batch reads/writes.

Measure before introducing caches.

---

# 56. Reusability

Переиспользование достигается хорошими boundaries, а не generic abstractions.

Компоненты, которые должны переиспользоваться:

```text
Normalized IR
Incremental Index Planner
Graph traversal policies
Semantic Resolver
Impact Engine
Context Compiler
Review Engine
Provenance model
```

CLI, MCP и будущий GitHub integration должны использовать один `ctx-app`.

Parser implementations должны выдавать одинаковый Normalized IR.

Storage не должен диктовать domain representation.

---

# 57. Prefer enums when domain is closed

Если набор вариантов принадлежит продукту:

```rust
RelationKind
NodeKind
ClaimClass
SourceKind
ChangeKind
```

предпочитай exhaustive enums.

Не заменяй их dynamic traits без необходимости.

Traits нужны там, где существуют настоящие external implementations:

```text
LanguageAnalyzer
GraphStore
GitRepository
optional inference provider
```

---

# 58. API design

Предпочитай APIs, которые выражают domain language.

Плохо:

```rust
store.get_edges(node, vec!["IMPLEMENTS".into()])
```

Core:

```rust
impact.analyze(ChangeSet)
```

или:

```rust
graph.outgoing(node, RelationFilter::semantic())
```

Stringly typed APIs оставлять только на serialization boundary.

---

# 59. Avoid leaking persistence IDs

SQLite row ID не должен быть domain identity.

Различай:

```text
database internal id
stable entity identity
version identity
```

Requirement IDs и canonical CodeSymbol identities должны переживать database rebuild.

---

# 60. No hidden LLM behavior

Весь MVP должен работать:

```bash
CTX_LLM=disabled
```

Если LLM позже подключён, его outputs должны содержать:

```text
provider/model
prompt/version
timestamp
input evidence references
confidence
```

LLM result всегда:

```text
INFERENCE
```

пока человек не подтвердил соответствующее утверждение.

---

# 61. Security/privacy

Никакой source code автоматически не отправляется наружу.

Любая future remote inference должна быть:

- explicit;
- opt-in;
- visible пользователю;
- отделена adapter boundary.

MVP не должен зависеть от неё.

---

# 62. Что сознательно НЕ строить

Не реализуй без отдельного требования:

```text
Neo4j
graph visualization UI
web UI
cloud backend
Jira
Linear
Confluence
RBAC
SSO
organization-wide graph
distributed indexing
runtime tracing
embedding database
generic vector store
generic graph query DSL
Cypher compatibility
automatic requirement generation
arbitrary invariant proving
generic plugin framework
multi-language support одновременно
```

---

# 63. Implementation milestones

Работай маленькими vertical slices.

Каждый milestone должен оставлять repository в полностью работающем состоянии.

После каждого milestone:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
```

должны проходить.

---

## M0 — Foundation

Создать workspace и core domain types.

Минимум:

```text
RepositoryId
CommitOid
NodeId
StableKey
NodeKind
RelationKind
ClaimClass
SourceKind
Confidence
Validity
Node
Edge
Evidence
```

Добавить SQLite migrations infrastructure.

Не строить graph functionality заранее.

Definition of Done:

```text
workspace compiles
migrations run
domain invariants unit-tested
temporary repository DB can be created
```

---

## M1 — Code indexing vertical slice

Реализовать:

```text
Git changed files
→ Tree-sitter parse
→ Normalized IR
→ CodeSymbol/File nodes
→ basic structural relations
→ SQLite
```

Первый язык — один.

CLI:

```bash
ctx init
ctx index
ctx status
```

Definition of Done:

можно проиндексировать fixture repository и повторный `ctx index` не пересобирает неизменённые files.

---

## M2 — Correct incremental indexing

Добавить:

```text
changed
added
deleted
renamed
moved symbols
```

Реализовать pure `IndexPlan`.

Добавить derivation ownership и edge invalidation.

Definition of Done:

fixture history подтверждает, что stale structural edges не остаются active.

---

## M3 — Business context + explicit semantic mappings

Парсить:

```text
.context/features
.context/requirements
.context/invariants
.context/decisions
```

Добавить explicit links:

```text
IMPLEMENTS
ENFORCES
COVERED_BY
```

С полной provenance.

CLI:

```bash
ctx explain
ctx impact
```

Definition of Done:

можно взять CodeSymbol и deterministic получить связанные Requirement/Invariant с evidence.

---

## M4 — ctx review

Сначала использовать только high-confidence/explicit semantic relations.

Реализовать:

```text
git diff
→ changed symbols
→ basic behavior classification
→ semantic impact
→ related tests
→ findings
```

CLI:

```bash
ctx review
ctx review --json
```

Definition of Done:

на fixtures unrelated refactor практически не создаёт semantic warnings, а изменение cancellation behavior показывает relevant invariant.

---

## M5 — Context Compiler

Реализовать:

```bash
ctx context "task"
```

С:

```text
seed extraction
typed traversal
ranking
token budget
ContextPack
```

Не использовать LLM.

Definition of Done:

Context Pack существенно меньше полного relevant directory и содержит нужный requirement/invariant/code/test chain.

---

## M6 — Semantic suggestions + verify

Только теперь добавить heuristic resolution.

Начать без embeddings и LLM:

```text
aliases
lexical signals
paths
graph evidence
tests
DB interactions
```

CLI:

```bash
ctx verify
```

Accepted/rejected decisions сохраняются.

---

## M7 — MCP

Добавить thin adapter:

```text
get_context
get_impact
explain_relation
find_requirements
review_change
```

Не менять core architecture ради MCP.

---

# 64. Working method

Для каждого нового feature:

1. Определи use case.
2. Определи pure domain decision.
3. Определи IO, который требуется use case.
4. Если boundary реально нужен — создай минимальный port.
5. Реализуй core tests.
6. Реализуй adapter.
7. Добавь integration test.
8. Добавь CLI/MCP surface только если feature готов.
9. Запусти fmt/clippy/tests.
10. Проверь, не появилось ли unnecessary abstraction.

---

# 65. Перед изменением архитектуры

Если считаешь, что нужно:

- новый crate;
- новый trait;
- новый storage abstraction;
- cache;
- graph DB;
- async runtime;
- background worker;
- generic framework;

сначала сформулируй конкретную проблему текущей реализации.

Не вводи abstraction только потому, что «может пригодиться».

Rule:

> duplication небольшого количества concrete code дешевле неправильной abstraction.

После появления второго реального use case abstraction можно выделить осознанно.

---

# 66. Quality bar

Код должен быть:

- idiomatic Rust;
- type-safe;
- deterministic;
- testable;
- simple;
- explicit about uncertainty;
- explicit about provenance;
- free from unnecessary inheritance-like patterns;
- free from hidden global state.

Предпочитать composition.

Не использовать interior mutability без реальной необходимости.

Минимизировать shared mutable state.

Pure transformations предпочитать mutation, когда это сохраняет читаемость.

---

# 67. Documentation

Публичные domain types и non-obvious algorithms документировать.

Не писать comments, которые повторяют код.

Документировать:

```text
why
invariant
trade-off
reason for unusual decision
```

Особенно хорошо документировать:

- identity strategy;
- validity semantics;
- stale relation semantics;
- confidence semantics;
- inference propagation rules.

---

# 68. Architectural decision rule

Если сталкиваешься с выбором:

```text
более generic
vs
проще и concrete
```

для MVP выбирай simpler concrete solution, если она не нарушает важную dependency boundary.

Если выбор:

```text
convenient IO-oriented implementation
vs
pure/testable domain decision
```

предпочитай Functional Core + Imperative Shell.

Если выбор:

```text
clever graph traversal
vs
typed bounded traversal
```

предпочитай typed bounded traversal.

Если выбор:

```text
high recall
vs
high precision
```

для `ctx review` предпочитай high precision.

---

# 69. Главные продуктовые риски, которые architecture должна учитывать

Всегда помнить:

1. semantic graph может стать stale;
2. automatic semantic links могут быть неправильными;
3. graph traversal может давать хуже context, чем search;
4. developers не будут поддерживать сотни YAML files;
5. review с false positives быстро перестанут читать;
6. LLM может создать убедительную, но ложную связь;
7. rename/move может ломать identity;
8. monorepo может сделать full rebuild неприемлемым;
9. generated code может загрязнить graph;
10. Context Pack может просто тратить больше tokens без улучшения agent performance.

Не скрывай эти проблемы за abstraction.

---

# 70. Success criteria

Архитектурно считай продукт успешным только если можно постепенно доказать:

### Review precision

High-confidence findings в основном действительно actionable.

### Semantic maintenance

Verified links переживают обычные refactors или корректно становятся stale.

### Incrementality

Обычное изменение нескольких файлов не приводит к repository-wide rebuild.

### Explainability

Для любого surfaced semantic finding существует deterministic evidence trail.

### Context quality

Context Pack даёт coding agent меньше и более релевантный input, чем обычное broad retrieval.

---

# 71. Когда останавливать плохое направление

Если feature требует:

- большого количества speculative abstractions;
- pervasive LLM inference;
- ручной annotation всей codebase;
- полного graph rebuild;
- сотен low-confidence relations;
- generic graph platform;

остановись и найди более узкий вариант.

Если graph не улучшает конкретный use case по сравнению с обычным search — используй search.

Graph не является целью сам по себе.

---

# 72. Первый vertical slice

Если repository ещё практически пустой, начинай именно с этой последовательности:

```text
1. Rust workspace
2. domain types
3. SQLite migrations
4. repository discovery
5. Git commit/diff adapter
6. Tree-sitter parser одного языка
7. Normalized IR
8. File + CodeSymbol persistence
9. basic structural relations
10. incremental re-index
11. .context Requirement + Invariant parsing
12. explicit semantic link
13. ctx impact
14. ctx explain
15. ctx review
```

Не переходи к semantic heuristics, embeddings, LLM или MCP до работающего пункта 15.

---

# 73. Перед каждым commit

Проверь:

```text
[ ] Есть ли более простое решение?
[ ] Осталась ли business logic вне IO adapters?
[ ] Не протёк ли SQLite/Tree-sitter/Git type в core?
[ ] Можно ли протестировать новое правило без filesystem/database?
[ ] Не появился ли primitive obsession?
[ ] Не появился ли trait только ради единственной implementation?
[ ] Не хранится ли inference как fact?
[ ] Есть ли provenance у новой semantic information?
[ ] Правильно ли invalidируется новый derived state?
[ ] Не создаёт ли изменение unnecessary graph noise?
[ ] Deterministic ли output?
[ ] Проходят ли fmt/clippy/tests?
```

---

# 74. Поведение coding-agent

Перед началом крупного изменения:

1. изучи существующий код;
2. сформулируй кратко текущие boundaries;
3. не переписывай существующий working code без причины;
4. реализуй минимальный complete vertical slice;
5. добавляй abstractions только когда concrete pressure уже существует.

Не спрашивай пользователя о мелких implementation choices, которые можно разумно решить локально.

Если architectural ambiguity не блокирует работу:

- выбери более простой reversible вариант;
- документируй решение;
- продолжай.

Если обнаружил противоречие между этим prompt и существующим working architecture:

- оцени intent обоих решений;
- предпочти минимальное изменение;
- явно опиши trade-off.

---

# 75. Главный критерий всех решений

`ctx` не должен быть впечатляющим graph engine.

Он должен быть инструментом, которому developer и coding agent могут доверять.

Поэтому порядок приоритетов:

```text
correctness
>
provenance
>
precision
>
maintainability
>
incrementality
>
performance
>
feature count
```

И основной engineering principle всего проекта:

> Keep the core deterministic, explicit and testable; keep side effects at the edges; store only claims whose origin and validity can be explained.
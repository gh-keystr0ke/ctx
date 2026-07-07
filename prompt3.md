# ctx — Product Requirements: External Knowledge Ingestion & Agent-Assisted Context Discovery

## 1. Цель спринта

Следующий этап `ctx` должен уменьшить необходимость вручную создавать product/business context.

Сегодня `ctx` умеет хранить и использовать:

- Features;
- Requirements;
- Invariants;
- Decisions;
- implementation mappings;
- tests;
- provenance;
- verified semantic relationships.

Но значительная часть этих знаний уже существует в истории разработки:

- Jira tickets;
- GitHub/GitLab issues;
- Pull Requests / Merge Requests;
- PR/MR descriptions;
- review discussions;
- commit messages;
- branch names;
- комментарии в коде;
- docstrings;
- документация.

Цель спринта:

> **ctx должен уметь находить существующее продуктовое знание в артефактах разработки, связывать его с реализацией и предлагать человеку проверяемые semantic candidates.**

AI-агенты могут использоваться для интерпретации неоднозначного материала, но не должны становиться источником истины.

---

# 2. Основной пользовательский результат

После подключения существующего проекта пользователь должен иметь возможность получить полезный business context без необходимости сначала вручную описать весь проект в `.context/`.

Пример:

В Jira существует:

> PAY-317  
> When a prepaid subscription is cancelled, access must remain active until `paid_until`.

В GitLab:

> MR !842 — Fix subscription cancellation

В review comment:

> We must not revoke an already paid entitlement immediately.

MR изменяет:

```text
SubscriptionService.cancel
```

После ingestion `ctx` должен быть способен предложить:

```text
Candidate Requirement

Paid subscription cancellation must preserve access
until the prepaid period expires.

Evidence:
- Jira PAY-317
- MR !842
- review comment
- SubscriptionService.cancel changed by MR !842

Possible implementation:
- billing.subscription.SubscriptionService.cancel

Possible tests:
- test_cancel_preserves_paid_entitlement
```

Это пока `INFERENCE`, а не подтверждённое требование.

После проверки пользователем оно может стать `ASSERTION`.

---

# 3. Product principles

## PR-P01 — Deterministic evidence first

Если связь может быть установлена достоверно без AI, она должна устанавливаться без AI.

Примеры:

```text
MR !842 contains commit abc123
```

может быть FACT.

```text
MR !842 changed SubscriptionService.cancel
```

может быть FACT.

```text
branch feature/PAY-317-cancellation contains PAY-317
```

может быть deterministic evidence.

Но:

```text
MR !842 implements PAY-317
```

может требовать semantic inference.

---

## PR-P02 — AI никогда не превращает предположение в факт

Результат AI-анализа должен сохраняться как inference/candidate с provenance.

AI должен иметь право ответить:

```text
insufficient_evidence
```

или:

```text
relevant: false
```

Отсутствие результата предпочтительнее выдуманного product knowledge.

---

## PR-P03 — Business knowledge должно быть связано с реализацией

Активная Feature, Requirement, Invariant или Decision должна иметь **ограниченный и объяснимый путь к implementation artifact**.

Не обязательно напрямую.

Правильный вариант:

```text
Feature
  ↓
Requirement
  ↓
Invariant
  ↓
SubscriptionService.cancel
  ↓
subscriptions
  ↓
test_cancel_preserves_access
```

Не требуется:

```text
Feature → каждая функция подсистемы
```

Широкие mappings не должны превращать Feature или Decision в semantic hub.

---

## PR-P04 — Provenance обязателен

Пользователь должен иметь возможность спросить:

```bash
ctx explain REQ-SUB-014
```

и понять:

- откуда появилось утверждение;
- какой artifact был источником;
- какой текст послужил evidence;
- какой агент сделал inference;
- какие code entities были связаны;
- подтверждал ли это человек.

---

## PR-P05 — AI provider не является частью ontology

Claude Code, Codex, Antigravity CLI и будущие агенты должны рассматриваться как заменяемые semantic analyzers.

Knowledge graph принадлежит `ctx`, а не конкретной модели.

---

# 4. External artifacts

## PR-EXT-001 — External artifacts должны быть first-class source material

`ctx` должен уметь импортировать артефакты разработки минимум следующих типов:

### MUST

- GitHub Issues;
- GitHub Pull Requests;
- GitLab Issues;
- GitLab Merge Requests;
- commit messages;
- branch names;
- PR/MR comments и review comments;
- code comments;
- docstrings.

### SHOULD

- Jira Issues;
- linked Jira discussions/comments;
- repository documentation.

Отсутствие конкретного provider integration не должно требовать изменения semantic core.

---

## PR-EXT-002 — Raw artifact должен сохраняться отдельно от извлечённого знания

Imported artifact не должен автоматически становиться Feature/Requirement/Invariant.

Например:

```text
Artifact:
GitLab MR !842

Title:
Fix cancellation handling

Body:
PAY-317. Users currently lose access immediately.
```

Это source artifact.

Отдельно может появиться:

```text
Candidate Requirement:
Paid users retain access after cancellation until paid_until.
```

---

## PR-EXT-003 — Artifact должен иметь собственную provenance identity

Для каждого внешнего артефакта необходимо сохранять минимум:

```text
provider
kind
external identifier
repository/project
title
content
author if available
created/updated timestamps if available
source locator / URL
ingestion timestamp/version
```

Повторный sync не должен создавать логически новый artifact, если это тот же объект внешней системы.

---

# 5. Связи между Issue / MR / Commit / Code

## PR-LINK-001 — ctx не должен предполагать единый workflow команды

Нельзя требовать, чтобы ticket ID находился только:

- в commit message;
- только в MR title;
- только в branch name;
- только в Jira backlink.

На практике могут встречаться:

```text
branch:
feature/PAY-317-subscription

commit:
fix cancellation behavior

MR title:
Preserve prepaid entitlement

MR body:
Fixes PAY-317

review comment:
This was originally reported in PAY-299
```

`ctx` должен использовать доступный набор evidence.

---

## PR-LINK-002 — Deterministic references должны извлекаться независимо от AI

Примеры:

```text
PAY-317
ABC-123
#482
!918
https://jira.example/.../PAY-317
```

Если они явно присутствуют в artifact, это должно сохраняться как evidence.

---

## PR-LINK-003 — AI может определять semantic relationship между артефактами

AI можно использовать, если наличие идентификатора не доказывает характер связи.

Пример:

```text
MR body:
Related to PAY-317
```

Нельзя автоматически считать это:

```text
MR IMPLEMENTS PAY-317
```

Агент может выдать:

```text
candidate_relation:
MR !842 -> PAY-317

relation:
implements

evidence:
- MR body
- overlapping description
- changed implementation

certainty:
supported
```

или:

```text
insufficient_evidence
```

---

## PR-LINK-004 — Агент не должен изобретать отсутствующие ticket IDs

Если во входном neighborhood нет `PAY-317`, агент не должен самостоятельно заключать:

```text
this is probably PAY-317
```

Допустима semantic связь только с реально существующим доступным artifact.

---

# 6. Artifact neighborhood для AI

## PR-AI-001 — Агент должен получать связный контекст, а не отдельную строку

Вместо:

```text
Analyze commit message:
"fix cancel"
```

агент должен, насколько возможно, получать:

```text
commit
branch
PR/MR title
PR/MR description
comments
review comments
linked issues
changed files
changed symbols
nearby tests
existing ctx relationships
```

Пример:

```text
Artifact neighborhood

Issue:
PAY-317 — Cancellation removes prepaid access

MR:
!842 — Fix cancellation

Branch:
feature/PAY-317

Commit:
fix cancellation behavior

Changed symbols:
SubscriptionService.cancel
StripeWebhookHandler.handle_subscription_update

Tests:
test_cancel_preserves_access
```

Такой input значительно сильнее отдельного commit message.

---

# 7. Agent-assisted knowledge extraction

## PR-AI-002 — AI должен классифицировать полезность source material

Для каждого artifact/neighborhood агент должен иметь возможность вернуть:

```text
relevant
not relevant
insufficient evidence
```

Пример бесполезного материала:

```text
MR:
Update dependencies
```

Результат:

```text
relevant: false
knowledge: []
```

---

## PR-AI-003 — Агент должен извлекать typed candidates

Минимально поддерживаемые candidate types:

- Feature;
- Requirement;
- Invariant;
- Decision.

Пример:

```json
{
  "type": "Invariant",
  "statement": "Already-paid entitlement must not be revoked before paid_until.",
  "evidence": [
    "PAY-317 description",
    "MR !842 review comment"
  ]
}
```

---

## PR-AI-004 — Агент должен отделять evidence от собственного reasoning

Недопустимо:

```text
Invariant:
Cancellation probably should be safe because that is normal SaaS behavior.
```

Допустимо:

```text
Invariant:
Already-paid entitlement must remain active.

Evidence:
Review comment:
"Do not revoke an already paid entitlement."
```

---

## PR-AI-005 — Agent inference должен содержать provenance

Необходимо знать минимум:

```text
agent/provider
agent version/model when available
input artifact IDs
output candidate
time
producer fingerprint/configuration
```

---

# 8. Поддержка разных AI agents

## PR-AGENT-001 — Semantic analysis должен поддерживать interchangeable agents

Минимальная цель — возможность использовать внешние CLI agents, например:

```text
Claude Code
Codex
Antigravity CLI
```

Пример UX, точный CLI не является обязательным:

```bash
ctx enrich --agent codex
ctx enrich --agent claude
ctx enrich --agent antigravity
```

или:

```bash
ctx ingest github --agent codex
```

---

## PR-AGENT-002 — ctx должен оставаться полезным без настроенного AI agent

Без AI должны продолжать работать:

- code indexing;
- deterministic artifact ingestion;
- exact references;
- Git relationships;
- existing `.context`;
- impact;
- review;
- explain;
- Context Pack.

AI расширяет semantic coverage, но не является mandatory dependency.

---

## PR-AGENT-003 — Агент должен работать через bounded input

Нельзя без необходимости отправлять агенту:

```text
entire repository
entire Jira project
all PR history
```

Предпочтительный unit работы:

```text
one artifact neighborhood
```

или:

```text
one candidate + local graph neighborhood
```

---

## PR-AGENT-004 — Multi-agent agreement может использоваться как ranking signal

SHOULD, не MUST.

Например:

```text
Claude:
Invariant X

Codex:
Invariant X

Antigravity:
insufficient evidence
```

Можно сохранить:

```text
agreement: 2 independent analyzers
```

Но это не означает:

```text
truth probability = 66%
```

Согласие моделей — только дополнительный ranking signal.

---

# 9. Извлечение знания из комментариев и docstrings

## PR-CODEDOC-001 — Code comments и docstrings должны рассматриваться как source artifacts

Пример:

```python
def cancel():
    # Keep access until paid_until because the current period
    # has already been paid for.
```

Из этого может появиться Candidate Invariant:

```text
Paid entitlement must remain active until paid_until.
```

Source evidence:

```text
billing/subscription.py
SubscriptionService.cancel
lines 41-42
```

---

## PR-CODEDOC-002 — Комментарий должен по возможности привязываться к ближайшему code entity

Например:

```text
comment
  ↓ DISCUSSES
SubscriptionService.cancel
```

А не только:

```text
comment → file
```

если доступен более точный symbol locator.

---

## PR-CODEDOC-003 — Комментарий не считается более истинным, чем код

Комментарий может быть устаревшим.

Поэтому извлечённое из него product knowledge должно оставаться evidence/inference до подтверждения или corroboration.

---

# 10. Поиск implementation mappings для business entities

## PR-MAP-001 — Для каждого candidate business entity ctx должен пытаться найти implementation anchors

После extraction Requirement:

```text
Paid users retain access until paid_until.
```

система должна попытаться найти:

```text
implementation candidates:
- SubscriptionService.cancel
- StripeWebhookHandler.handle_subscription_update

data:
- subscriptions.paid_until

tests:
- test_cancel_preserves_access
```

---

## PR-MAP-002 — Implementation mapping тоже является отдельным inference

Из того, что Requirement и функция появились в одном MR, не следует автоматически:

```text
Function IMPLEMENTS Requirement
```

Это evidence для candidate relationship.

---

## PR-MAP-003 — Business entity без implementation path ухудшает graph health

После принятия business entity система должна проверять существование bounded path к implementation artifact.

Пример состояния:

```text
REQ-PAY-317
No implementation path found.

Health:
needs_mappings
```

---

## PR-MAP-004 — Путь может быть непрямым

Valid:

```text
Feature
  ↓
Requirement
  ↓
Invariant
  ↓
CodeSymbol
```

Valid:

```text
Decision
  ↓
Requirement
  ↓
API
  ↓
CodeSymbol
```

Valid:

```text
Requirement
  ↓
DbEntity
```

Business entity не обязана иметь direct edge на десятки symbols.

---

# 11. Что считается implementation artifact

MUST учитывать уже существующие:

- Code Symbol;
- Test;
- DB Entity.

Архитектура не должна запрещать дальнейшее расширение:

- API endpoint;
- event;
- queue/topic;
- external integration;
- configuration;
- feature flag;
- database migration/schema;
- infrastructure resource.

Пример:

```text
Invariant:
Payment event processing must be idempotent.

Implementation path:
Invariant
  ↓
WebhookHandler.process
  ↓
webhook_events
```

---

# 12. Human verification

## PR-VERIFY-001 — Новое semantic knowledge должно проходить через существующую verification model

Пример:

```bash
ctx verify
```

Пользователь видит:

```text
Candidate Invariant

Already-paid entitlement must remain active until paid_until.

Evidence:
1. Jira PAY-317
2. MR !842 review comment
3. SubscriptionService.cancel changed in MR

Implementation candidates:
- SubscriptionService.cancel
- test_cancel_preserves_access

Actions:
accept / edit / reject / skip / explain
```

---

## PR-VERIFY-002 — Accept не должен уничтожать original inference

Необходимо сохранить цепочку:

```text
External artifact
    ↓
Agent inference
    ↓
Human verification
    ↓
Assertion
```

Чтобы `ctx explain` мог восстановить происхождение знания.

---

# 13. Incremental semantic discovery

## PR-INCR-001 — Повторный ingestion должен искать новое знание, а не заново анализировать всё

Если ранее были обработаны:

```text
Issues 1..1000
MR 1..500
```

а появился:

```text
MR 501
```

система должна преимущественно обработать новый/изменённый neighborhood.

---

## PR-INCR-002 — Агенту должен передаваться существующий graph context

Важный вопрос агенту:

> Есть ли здесь новое product knowledge, которого ещё нет в графе?

Пример:

В графе уже существует:

```text
REQ-17:
Cancellation preserves access until paid_until.
```

Новый MR говорит то же самое.

Желаемое поведение:

```text
new entity: no

additional evidence for REQ-17:
MR !932
```

а не создание:

```text
REQ-94:
Cancellation should preserve paid access.
```

---

# 14. Short-name symbol lookup

## PR-LOOKUP-001 — Query CLI должен принимать short symbol name

Пользователь должен иметь возможность написать:

```bash
ctx impact Replication
```

вместо обязательного:

```bash
ctx impact internal.logic.manager.Replication
```

---

## PR-LOOKUP-002 — Несколько exact short-name matches не являются ошибкой

Если существуют:

```text
internal.logic.manager.Replication
storage.replication.Replication
tests.fixtures.Replication
```

то:

```bash
ctx impact Replication
```

должен обработать все три результата.

Пример text output:

```text
3 symbols matched "Replication"

[1/3]
internal.logic.manager.Replication
kind: Struct
language: Rust

Requirements:
...

Invariants:
...


[2/3]
storage.replication.Replication
kind: Trait
language: Rust

Requirements:
...


[3/3]
tests.fixtures.Replication
kind: Class
language: Python

...
```

---

## PR-LOOKUP-003 — Результаты ambiguous lookup должны анализироваться независимо

Недопустимо объединять найденные symbols в один semantic traversal.

Семантика должна быть эквивалентна:

```bash
ctx impact internal.logic.manager.Replication
ctx impact storage.replication.Replication
ctx impact tests.fixtures.Replication
```

с последующей агрегацией output.

Это защищает от semantic leakage между независимыми graph neighborhoods.

---

## PR-LOOKUP-004 — JSON должен сохранять границы результатов

Пример:

```json
{
  "query": "Replication",
  "matches": [
    {
      "symbol": "internal.logic.manager.Replication",
      "result": {}
    },
    {
      "symbol": "storage.replication.Replication",
      "result": {}
    }
  ]
}
```

Не:

```json
{
  "combinedImpact": {}
}
```

---

## PR-LOOKUP-005 — Exact identity остаётся поддерживаемым

Пользователь всё ещё может использовать:

```text
fully qualified canonical path
```

или:

```text
language-qualified stable key
```

когда нужна однозначность.

Short name является дополнительным human interface, а не новой identity model.

---

## PR-LOOKUP-006 — Persistent mappings должны оставаться строгими

Для `.context`:

```yaml
implementation:
  - Replication
```

если найдено несколько symbols, это должно быть ошибкой/неразрешённым mapping.

Query:

```text
Replication → 0..N
```

Persistent assertion:

```text
Replication → exactly 1
```

---

## PR-LOOKUP-007 — Short-name lookup должен работать для основных query workflows

MUST:

```text
ctx impact
ctx context --symbol
```

SHOULD:

```text
ctx explain
MCP symbol-oriented queries
```

Дополнительно полезен discovery UX:

```bash
ctx find Replication
```

Пример:

```text
3 symbols found

rust    struct    internal.logic.manager.Replication
rust    trait     storage.replication.Replication
python  class     tests.fixtures.Replication
```

Точное имя команды может быть выбрано при техническом планировании.

---

# 15. Explicit seeds и Context Pack

## PR-CONTEXT-001 — Несколько short-name matches являются несколькими explicit roots

Пример:

```bash
ctx context "investigate replication bug" --symbol Replication
```

Если найдено три symbols:

```text
3 explicit roots
```

Они должны оставаться отдельными root groups.

---

## PR-CONTEXT-002 — Explicit symbol selection не должен запускать unrelated lexical roots

Если пользователь явно указал:

```text
--symbol Replication
```

Context Pack не должен дополнительно выбирать unrelated symbols только из-за lexical similarity.

---

# 16. Explainability

## PR-EXPLAIN-001 — Любой extracted candidate должен быть объясним

Пользователь должен иметь возможность узнать:

```text
Why does ctx think this requirement exists?
```

Ответ:

```text
Candidate REQ-42

Derived from:
- Jira PAY-317, description paragraph 2
- GitLab MR !842, description
- review comment 18392

Related implementation:
- SubscriptionService.cancel

Why related:
- symbol changed in MR !842
- MR explicitly references PAY-317

Inference producer:
- codex
```

---

# 17. Failure cases

## FR-01 — Hallucinated requirement

Artifact:

```text
Fix cancellation
```

Недопустимый output:

```text
Requirement:
Users must have 30-day cancellation protection.
```

Такого evidence нет.

Правильный результат:

```text
insufficient_evidence
```

---

## FR-02 — Incidental ticket mention

MR:

```text
PAY-317 is related, but this MR only updates logging.
```

Нельзя автоматически делать:

```text
MR IMPLEMENTS PAY-317
```

---

## FR-03 — Semantic hub

Feature:

```text
Payments
```

не должна автоматически связываться напрямую со всеми 150 payment functions.

Предпочтительно:

```text
Feature
  ↓
specific Requirements
  ↓
specific implementation
```

---

## FR-04 — Ambiguous symbol lookup

```bash
ctx impact Manager
```

находит 12 symbols.

Это не ошибка.

Но данные должны быть показаны как 12 независимых результатов, а не как один огромный graph neighborhood.

---

## FR-05 — Ambiguous persistent mapping

```yaml
implementation:
  - Manager
```

находит 12 symbols.

Это ошибка mapping и требует уточнения.

---

## FR-06 — Stale comment

Комментарий говорит:

```text
Always write status=inactive.
```

текущий код этого больше не делает.

Комментарий остаётся historical evidence и не должен автоматически переопределять текущую implementation truth.

---

# 18. Sprint priority

## MUST HAVE

### External evidence

- normalized external artifact representation;
- Git commit messages;
- branch names;
- GitHub/GitLab Issue + PR/MR ingestion хотя бы для одного end-to-end provider;
- PR/MR comments/reviews;
- code comments/docstrings;
- provenance для каждого artifact.

### Linking

- deterministic identifier/reference extraction;
- commit ↔ MR/PR ↔ changed code relationships;
- artifact ↔ code evidence;
- отсутствие предположений о едином расположении ticket ID.

### AI

- interchangeable agent boundary;
- минимум один реально работающий CLI agent integration;
- relevant / irrelevant / insufficient-evidence outcome;
- typed semantic candidate extraction;
- implementation candidate discovery;
- provenance всех agent outputs.

### Knowledge integrity

- candidates не становятся assertions автоматически;
- human verification;
- accepted business entities должны иметь implementation path либо явно сообщаться как unmapped;
- duplicate/new-knowledge detection хотя бы на базовом уровне.

### CLI UX

- exact short-name lookup;
- multiple matches without ambiguity error;
- isolated result processing;
- JSON preserves per-match boundaries;
- strict persistent mappings remain unchanged.

---

## SHOULD HAVE

- Jira integration;
- несколько AI providers;
- multi-agent agreement signal;
- `ctx find <symbol>`;
- filters вроде:

```bash
ctx impact Replication --language rust
ctx impact Replication --kind struct
```

- incremental external sync;
- additional evidence attachment to existing Requirements вместо duplicate entity creation.

---

## NOT REQUIRED IN THIS SPRINT

- fully autonomous creation of trusted business context;
- automatic acceptance of AI-generated Requirements;
- embeddings/vector DB as mandatory infrastructure;
- cloud service;
- web UI;
- processing an organization's entire Jira history in one agent prompt;
- proving semantic correctness from source comments;
- forcing teams to adopt ticket/branch/commit naming conventions;
- generic integrations platform.

---

# 19. End-to-end acceptance scenario

Исходный проект не имеет `.context` документов.

Git/Jira содержат:

```text
Jira PAY-317:
A cancelled prepaid subscription must remain usable until paid_until.
```

```text
Branch:
feature/PAY-317-cancel
```

```text
MR !842:
Fix cancellation semantics
```

```text
Review:
Do not revoke already-paid entitlement immediately.
```

MR изменяет:

```text
SubscriptionService.cancel
```

и:

```text
test_cancel_preserves_access
```

### Expected flow

Пользователь запускает external ingestion.

`ctx` сохраняет:

```text
Jira PAY-317
MR !842
commit(s)
review comment
branch
```

и deterministic relationships между ними.

Agent получает bounded neighborhood.

Agent предлагает:

```text
Requirement:
Cancellation must preserve already-paid access until paid_until.

Implementation candidates:
- SubscriptionService.cancel

Tests:
- test_cancel_preserves_access
```

Candidate имеет provenance.

Пользователь подтверждает candidate.

После этого:

```bash
ctx impact SubscriptionService.cancel
```

возвращает Requirement.

Также:

```bash
ctx impact cancel
```

работает по short name.

Если `cancel` существует в трёх namespaces, CLI показывает три независимых результата.

После изменения `SubscriptionService.cancel`:

```bash
ctx review
```

способен показать потенциальное влияние на подтверждённый Requirement.

И:

```bash
ctx explain REQ-...
```

показывает полный путь:

```text
Requirement
← Human verification
← Agent inference
← Jira + MR + review evidence
→ SubscriptionService.cancel
→ linked test
```

---

# 20. Definition of Done

Sprint считается успешным, если можно взять репозиторий с минимальным или отсутствующим `.context`, использовать реальные development artifacts и продемонстрировать следующий journey:

```text
GitHub/GitLab/Jira/Git/code comments
        ↓
raw local artifacts
        ↓
deterministic relationships
        ↓
bounded AI analysis
        ↓
business knowledge candidates
        ↓
implementation candidates
        ↓
human verification
        ↓
evidence-backed ctx graph
        ↓
impact / review / context / explain
```

При этом:

1. AI не требуется для deterministic functionality.
2. AI-generated knowledge никогда не становится FACT.
3. Любое inference имеет provenance.
4. Неоднозначность должна быть видимой, а не скрытой.
5. Business knowledge должно иметь путь к implementation.
6. Short-name lookup улучшает UX, но не ослабляет identity или persistent mappings.
7. Несколько найденных symbols не смешиваются в один semantic neighborhood.
8. Система предпочитает `insufficient_evidence` ложному product knowledge.
9. Существующие гарантии `ctx` по bounded traversal, precision и explainability не должны быть ослаблены.
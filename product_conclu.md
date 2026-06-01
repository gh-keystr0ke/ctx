# Product Context Prompt — ctx

Ты работаешь над продуктом **ctx**.

Этот документ описывает не техническую архитектуру реализации, а **смысл продукта, его пользователей, бизнес-проблему, основные use cases, ограничения и критерии ценности**.

Любое техническое решение должно проверяться не только на корректность архитектуры, но и на соответствие продуктовой цели.

---

# 1. Product vision

`ctx` — persistent context layer для software development и coding agents.

Главная идея:

> Coding agent должен понимать не только как работает код, но и зачем этот код существует.

Сегодня coding agents и code intelligence системы хорошо понимают структуру codebase:

```text
function A calls function B
module X imports module Y
symbol Z is defined in file F
service A writes table B
```

Но они значительно хуже понимают:

```text
почему эта функция существует;

какой product requirement она реализует;

какое business invariant она должна сохранять;

какой пользовательский сценарий сломается при её изменении;

какое решение в архитектуре объясняет существующее поведение;

какой business contract фактически меняется в PR.
```

`ctx` должен создать постоянно поддерживаемый слой контекста между:

```text
Product intent
       ↕
Business constraints
       ↕
Software architecture
       ↕
Implementation
       ↕
Tests
```

---

# 2. Главная проблема

Большинство крупных codebase содержит важное знание в нескольких несовместимых формах:

```text
code
tests
documentation
tickets
ADRs
comments
tribal knowledge
mental model senior engineers
```

Когда разработчик или coding agent меняет код, ему приходится самостоятельно реконструировать:

> Что этот код означает для продукта?

Это дорого, медленно и ненадёжно.

Особенно при работе с legacy systems или сложной бизнес-логикой.

---

# 3. Fundamental product problem

Проблема `ctx` НЕ является:

> «разработчикам сложно найти код».

Эту проблему уже неплохо решают:

- IDE;
- grep;
- code search;
- language servers;
- code graphs;
- semantic search;
- RAG;
- coding agents.

Настоящая проблема:

> **Разработчику сложно понять business impact изменения кода и восстановить implicit product contract, который этот код реализует.**

---

# 4. Example

Рассмотрим subscription system.

Business intent:

```text
Feature:
Subscription cancellation
```

Use case:

```text
User cancels subscription at period end.
```

Requirement:

```text
When a paid user cancels,
the subscription must remain usable until paid_until.
```

Invariant:

```text
Paid entitlement must never be revoked
before paid_until.
```

Implementation:

```text
SubscriptionService.cancel()
```

Data:

```text
subscriptions.status
subscriptions.paid_until
```

Integration:

```text
StripeWebhookHandler
```

Tests:

```text
test_cancel_keeps_access_until_paid_until
```

Для обычного code graph эти объекты практически независимы.

Для `ctx` они представляют одну связанную часть product behavior.

---

# 5. Product promise

Главное обещание продукта:

> **Know what a code change means to the business before you merge it.**

Для coding agents:

> **Give coding agents the context explaining why the code exists.**

Не продавать продукт как:

```text
knowledge graph
code graph
RAG
Graphify alternative
AI documentation tool
```

Это implementation details или соседние категории.

---

# 6. Primary users

## Developer

Разработчик:

- исправляет bug;
- реализует feature;
- меняет legacy code;
- делает refactoring;
- изучает незнакомый subsystem.

Его вопрос:

> Что мне нельзя случайно сломать?

---

## Code reviewer

Reviewer видит PR, который может менять значительную часть системы.

Его вопросы:

```text
Какой функционал реально изменён?

Какие требования затронуты?

Какие business invariants должны оставаться истинными?

Какие edge cases были раньше?

Достаточно ли тестов?

Изменился ли product contract?
```

Это один из самых ценных пользователей `ctx`.

---

## AI coding agent

Например:

```text
Claude Code
Codex
Cursor
другие coding agents
```

Проблема agent:

он способен прочитать много кода, но не знает заранее:

```text
что важно;
что является контрактом;
каким источникам доверять;
какой context stale;
что является фактом;
что является inference.
```

`ctx` должен предоставить компактный, evidence-backed Context Pack.

---

## Senior engineer / domain expert

Senior engineer обычно хранит значительную часть implicit context.

Он нужен `ctx` не как постоянный annotator, а как источник редких high-value confirmations.

Например:

```text
Да, этот code path действительно отвечает
за INV-PAYMENTS-018.
```

После подтверждения это знание становится reusable для:

```text
developers
reviewers
agents
future PRs
```

---

# 7. ICP — Ideal Customer Profile

Наиболее подходящий клиент:

```text
30–300 инженеров
```

с одним или несколькими зрелыми repositories.

Особенно если присутствуют:

- legacy code;
- сложная business logic;
- много state transitions;
- высокий cost ошибок;
- несколько интеграций;
- большая история продукта;
- знания концентрируются у senior engineers;
- coding agents активно используются.

---

# 8. Наиболее подходящие индустрии

Высокий potential:

```text
fintech
payments
insurance
healthcare workflows
ERP
billing
subscriptions
marketplaces
B2B SaaS
compliance-heavy systems
```

Ниже ценность для:

```text
маленького CRUD application
простого marketing site
маленького greenfield проекта
продукта почти без business invariants
```

Не пытаться продавать `ctx` всем software teams.

---

# 9. Buyer

Основные потенциальные buyers:

```text
VP Engineering
Director of Engineering
CTO
Head of Developer Productivity
Platform Engineering Lead
```

Иногда:

```text
Engineering Manager
Principal Engineer
```

---

# 10. Что покупает buyer

Buyer покупает не graph.

Он покупает снижение риска и стоимости изменений.

Ценность должна выражаться примерно так:

```text
меньше regression bugs;

быстрее PR review;

меньше зависимости от tribal knowledge;

меньше времени на исследование незнакомого codebase;

лучшие результаты AI coding agents;

меньше контекста нужно передавать агентам;

быстрее onboarding инженеров.
```

---

# 11. Core product jobs-to-be-done

## Job 1 — Understand impact before changing code

Developer хочет изменить:

```text
SubscriptionService.cancel()
```

Он должен получить:

```text
какие features затронуты;
какие requirements;
какие invariants;
какие APIs;
какие DB entities;
какие integrations;
какие tests.
```

---

## Job 2 — Understand a feature from intent to implementation

Developer или agent спрашивает:

```bash
ctx feature subscription-cancellation
```

Результат:

```text
Feature
→ Requirements
→ Invariants
→ Relevant implementation
→ Data
→ External systems
→ Tests
```

Не нужно возвращать всё вокруг feature.

Нужен минимальный coherent context.

---

## Job 3 — Review change in terms of product behavior

Это основной commercial wedge.

Input:

```bash
ctx review
```

или git diff.

Output должен отвечать:

```text
Что реально изменилось?

Какой product behavior потенциально изменился?

Какие requirements связаны с этим кодом?

Какие invariants могут быть затронуты?

Какие тесты должны защищать это поведение?

Меняется ли фактический contract без изменения requirement?

Какие conclusions uncertain?
```

---

## Job 4 — Compile context for coding agent

Пользователь говорит агенту:

```text
Fix cancellation so we don't revoke access
when Stripe webhook arrives late.
```

Обычный agent начинает искать:

```text
Stripe
cancel
subscription
access
webhook
```

и читает много случайных файлов.

`ctx` должен подготовить компактный Context Pack:

```text
relevant feature
requirements
invariants
implementation
DB state
integrations
tests
known edge cases
relevant decisions
```

---

## Job 5 — Explain why a relationship exists

Пользователь должен иметь возможность спросить:

```text
Почему ctx считает,
что SubscriptionService.cancel
реализует REQ-SUB-014?
```

Ответ должен ссылаться на evidence.

Это важная часть trust model.

---

# 12. Killer feature

Если приходится выбирать только одну feature:

```bash
ctx review
```

Она должна стать product wedge.

Почему:

- появляется прямо в existing workflow;
- не требует изменения привычек разработчика;
- value можно получить на каждом PR;
- результат можно измерять;
- feedback появляется быстро;
- легко понять, был ли finding полезным;
- semantic graph постепенно становится лучше через usage.

---

# 13. What excellent ctx review looks like

Хороший finding:

```text
HIGH — Cancellation entitlement behavior changed

Changed:
SubscriptionService.cancel()

Affected invariant:
INV-SUB-003

Paid entitlement must remain active until paid_until.

Why this is relevant:
SubscriptionService.cancel() is a human-verified
enforcer of INV-SUB-003.

Detected change:
The guard checking paid_until moved after
subscription.status is set to inactive.

Related test:
test_cancel_keeps_access_until_paid_until

Test was not modified.

Suggested reviewer action:
Verify that early cancellation still preserves
paid entitlement.
```

---

# 14. What bad ctx review looks like

Плохо:

```text
This change may affect subscriptions.
```

или:

```text
Potentially affected files:
37
```

или:

```text
This change may violate business rules.
```

без evidence.

Это noise.

`ctx` должен предпочитать молчание слабому finding.

---

# 15. Precision over recall

Для review продукта:

```text
false positive
```

часто хуже:

```text
missed weak relation
```

Потому что пользователь быстро учится игнорировать noisy bot.

Поэтому основной принцип:

> Surface fewer findings with stronger evidence.

Low-confidence information может быть доступна через:

```text
--verbose
```

но не должна доминировать основной review.

---

# 16. Existing alternatives

Не нужно утверждать, что конкуренты бесполезны.

Они решают другие части проблемы.

## Code search

Хорошо отвечает:

```text
где находится код?
```

Плохо:

```text
какой business contract этот код реализует?
```

---

## Code graph

Хорошо отвечает:

```text
что зависит от чего технически?
```

Плохо:

```text
почему эта dependency важна продукту?
```

---

## RAG

Хорошо отвечает:

```text
какие документы/файлы похожи на запрос?
```

Но retrieval similarity не равна truth.

RAG обычно не знает:

```text
статус requirement;
commit validity;
кто подтвердил связь;
stale ли она;
fact это или inference.
```

---

## Documentation

Документ может описывать правильный behavior.

Но проблема:

```text
documentation ↔ implementation
```

обычно не имеет explicit maintained relationship.

---

## General AI code review

Может обнаруживать:

```text
bugs
style issues
security issues
common programming mistakes
```

`ctx` должен специализироваться на другом:

> product-specific semantic impact.

---

# 17. Unique product idea

Ценность появляется не просто от graph.

Ключевая сущность:

```text
verified relationship
between product intent
and implementation
```

с:

```text
provenance
validity
confidence
history
evidence
```

Например:

```text
SubscriptionService.cancel
    ENFORCES
INV-SUB-003
```

важно не само по себе.

Важно:

```text
кто это сказал;
почему;
для какого состояния codebase;
насколько этому можно доверять;
изменился ли implementation после подтверждения.
```

---

# 18. Provenance is product functionality

Provenance — не internal technical detail.

Это часть пользовательской ценности.

Пользователь должен видеть разницу между:

```text
StaticAnalysis
Human
Documentation
LLMInference
Runtime
ExternalSystem
```

И между:

```text
FACT
ASSERTION
INFERENCE
```

---

# 19. Trust hierarchy

`ctx` не должен создавать иллюзию certainty.

Пример:

```text
FACT

cancel() writes subscriptions.status

Source:
StaticAnalysis

Confidence:
1.0
```

Отдельно:

```text
ASSERTION

cancel() ENFORCES INV-SUB-003

Source:
Human verification
```

И отдельно:

```text
INFERENCE

StripeWebhookHandler may also enforce INV-SUB-003

Evidence:
...
Confidence:
0.73
```

Это принципиально разные statements.

---

# 20. Never silently convert inference into truth

LLM или heuristic может предложить:

```text
CodeSymbol IMPLEMENTS Requirement
```

Но это только candidate.

Без подтверждения:

```text
INFERENCE
```

не превращается автоматически в:

```text
FACT
```

или:

```text
ASSERTION
```

---

# 21. Business context

Часть product knowledge хранится рядом с кодом:

```text
.context/
```

Минимально:

```text
features/
requirements/
invariants/
decisions/
```

Business context должен:

- жить в Git;
- versionироваться вместе с кодом;
- участвовать в code review;
- иметь stable IDs;
- сохранять history.

---

# 22. Business context must remain small

Пользователь не должен сначала документировать весь продукт.

Неправильный onboarding:

```text
Опишите 500 requirements,
200 use cases
и 1000 mappings,
после этого ctx станет полезен.
```

Правильный onboarding:

```text
ctx индексирует существующий repository;

несколько high-value invariants
или requirements добавляются вручную;

система постепенно предлагает semantic links;

полезность появляется быстро;

verified knowledge накапливается естественно.
```

---

# 23. Focus on high-value business context

Не все business knowledge одинаково важно.

Особенно ценны:

```text
invariants
non-obvious requirements
edge cases
external contracts
architectural decisions
```

Например:

```text
User email must be unique.
```

может быть очевидно из unique DB constraint.

Но:

```text
Cancellation requested after invoice payment
must retain entitlement until paid_until
even if Stripe events arrive out of order.
```

— значительно более ценный context.

---

# 24. Human-in-the-loop philosophy

Человек не должен быть data annotator.

Система должна делать большую часть работы сама.

Человек нужен для редких high-value решений:

```text
confirm semantic relationship;
reject incorrect relationship;
confirm important invariant;
resolve ambiguity.
```

После одного подтверждения knowledge должен многократно переиспользоваться.

---

# 25. Semantic verification UX

Хороший interaction:

```text
Possible relation:

REQ-SUB-014
→ SubscriptionService.cancel()

Evidence:

- explicit terms: cancel, subscription
- symbol reads paid_until
- symbol writes subscription.status
- related test contains paid_until

Confidence: 0.82

Accept / Reject / Skip / Explain
```

Плохой interaction:

```text
Please classify 700 functions.
```

---

# 26. Product flywheel

Возможный long-term flywheel:

```text
code indexed
    ↓
semantic suggestions
    ↓
developers verify useful links
    ↓
review quality improves
    ↓
review generates feedback
    ↓
organization-specific context improves
    ↓
agent context improves
```

Главный accumulating asset:

> verified organization-specific mappings between intent and implementation.

---

# 27. Context Pack

Context Pack должен быть product primitive.

Input:

```text
task
+
optional diff
+
optional files
```

Output:

```text
bounded relevant context
```

Он должен оптимизировать:

> Maximum useful information per token.

---

# 28. Context Pack is not file retrieval

Плохой Context Pack:

```text
15 full source files
```

Хороший:

```text
Task

Behavioral constraints

Relevant requirements

Relevant invariants

Changed/relevant symbols

Relevant DB state

External contracts

Tests

Known uncertainties

Evidence
```

Source snippets включаются только там, где нужны.

---

# 29. Main Context Pack users

Первично:

```text
coding agents
```

Но тот же механизм должен использоваться:

```text
ctx impact
ctx review
future IDE integration
future GitHub integration
```

Не строить отдельную retrieval logic для каждого frontend.

---

# 30. Requirement drift

Одна потенциально сильная capability:

система знает:

```text
implementation changed
```

и знает:

```text
implementation ↔ requirement
```

но requirement не изменился.

Это может означать:

```text
intentional implementation change
```

или:

```text
product contract changed but requirement was forgotten
```

`ctx` должен сообщать:

```text
Possible requirement drift
```

а не утверждать:

```text
Requirement is wrong.
```

---

# 31. Staleness as first-class concept

Связь может быть правильной сегодня и неверной после refactor.

Например:

```text
cancel() ENFORCES INV-SUB-003
```

была human-verified.

После серьёзной смены body:

она не должна бесшумно оставаться fully trusted.

Возможный state:

```text
stale / needs verification
```

Пользователь должен понимать причину.

---

# 32. Temporal context

`ctx` должен постепенно уметь отвечать не только:

```text
что связано сейчас?
```

но концептуально хранить:

```text
когда связь была создана;
для какого commit она была valid;
когда implementation изменился;
когда relation была проверена;
когда requirement superseded старый.
```

Это важная часть долгосрочного преимущества продукта.

---

# 33. Local-first

Local-first — не только technical constraint, но и product positioning.

Codebase часто содержит:

```text
proprietary source code
trade secrets
security-sensitive implementation
regulated data flows
```

Основной продукт должен работать без отправки source code наружу.

---

# 34. LLM is optional

Без LLM должны работать:

```text
indexing
code graph
explicit business context
explicit semantic links
impact analysis
explain
basic review
context compilation
```

LLM может улучшать:

```text
candidate generation
semantic reranking
summarization
behavior explanation
```

но не является фундаментом correctness.

---

# 35. Product positioning

Предпочтительные варианты:

> Persistent context layer for coding agents.

> Your coding agents know why the code exists.

> Know what a code change means to the business before you merge it.

Не использовать в качестве primary positioning:

```text
AI knowledge graph
code graph platform
Graphify competitor
RAG for source code
```

---

# 36. Commercial wedge

Первый коммерчески понятный workflow:

```text
PR
 ↓
ctx review
 ↓
semantic impact findings
```

Почему:

- существует понятный trigger;
- существует конкретный user;
- результат короткий;
- можно измерять feedback;
- есть natural team-level integration;
- есть budget у engineering organization;
- value повторяется каждый день.

---

# 37. Future free/local tier

Potential Local/Free:

```text
local code indexing
local graph
.context business context
CLI
MCP
basic impact
basic review
```

Это полезный самостоятельный продукт.

---

# 38. Future Team / Enterprise value

Не реализовывать сейчас, но понимать direction:

```text
shared verified knowledge
GitHub/GitLab checks
ownership
requirement drift
history
Jira/Linear/Confluence imports
audit trail
RBAC
SSO
self-hosted/VPC
policies
```

Enterprise value появляется из shared organizational context, а не только из большого graph.

---

# 39. What NOT to become

Не превращать `ctx` в:

## Documentation system

Мы не хотим заставлять команду описывать всю систему вручную.

## Ticketing system

Jira/Linear останутся source systems.

## Generic graph database

Пользователь не покупает graph.

## Generic RAG platform

Retrieval — компонент, не продукт.

## Generic AI reviewer

Наша специализация — business/product semantic impact.

## Architecture visualization tool

Graph UI может быть полезен позже, но это не core value.

---

# 40. Most important product risks

## Risk 1 — Nobody maintains business context

Если приходится постоянно обновлять YAML вручную, adoption будет низким.

Поэтому context должен быть:

```text
small
high-value
reviewable
incrementally maintained
```

---

## Risk 2 — Semantic links become stale

Stale trusted context хуже отсутствующего context.

Нужны:

```text
validity
invalidation
verification
history
```

---

## Risk 3 — Too many false positives

Если `ctx review` постоянно предупреждает:

```text
maybe something changed
```

его отключат.

High precision важнее high recall.

---

## Risk 4 — Graph worse than search

Не использовать graph только потому, что он существует.

Для:

```text
Find Stripe invoice code
```

search может быть лучшим tool.

Graph использовать там, где есть meaningful relationships:

```text
requirement
invariant
verified implementation relation
temporal validity
```

---

## Risk 5 — Agent ignores context

Context Pack может быть technically correct, но слишком большим или нерелевантным.

Нужно измерять реальное improvement agent behavior.

---

## Risk 6 — Product requires large upfront setup

Time-to-first-value должен быть коротким.

Пользователь должен получить что-то полезное после:

```text
ctx init
ctx index
```

и нескольких high-value semantic mappings.

---

# 41. Success metrics

Не использовать vanity metrics вроде:

```text
number of graph nodes
number of edges
number of indexed repositories
```

Они почти ничего не говорят о value.

---

## Review precision

Процент surfaced findings, которые developer считает полезными.

Это одна из главных метрик.

---

## Time to impact understanding

Сколько времени developer тратит, чтобы понять:

```text
что затрагивает change?
```

с `ctx` и без него.

---

## Context efficiency

Для coding agent:

```text
relevant tokens / total context tokens
```

или proxy:

```text
number of unnecessary file reads
```

---

## Agent task success

Сравнивать:

```text
agent + normal repository tools
```

с:

```text
agent + ctx Context Pack
```

по:

```text
task success
tests passing
number of attempts
requirement violations
tokens
```

---

## Semantic maintenance cost

Сколько human interventions требуется, чтобы context оставался полезным.

Если maintenance растёт примерно вместе с количеством symbols — модель плохая.

---

# 42. Product quality hierarchy

При конфликте приоритетов:

```text
trust
>
precision
>
usefulness
>
coverage
>
feature count
```

Не увеличивать coverage ценой доверия.

---

# 43. Decision filter for every feature

Перед реализацией feature задай:

### Question 1

Помогает ли она coding agent или developer понять business impact изменения?

### Question 2

Помогает ли она поддерживать trustworthy intent ↔ implementation relationship?

### Question 3

Улучшает ли она `ctx review` или Context Pack?

### Question 4

Можно ли доказать её value на реальном repository?

Если почти везде ответ «нет» — скорее всего feature не относится к MVP.

---

# 44. MVP product scope

MVP должен доказать одну фундаментальную гипотезу:

> Если у нас есть небольшой набор trustworthy semantic relationships между product intent и кодом, можем ли мы сделать code review и agent context заметно лучше?

MVP не должен доказывать:

```text
что можно автоматически описать весь бизнес;

что можно построить идеальный enterprise knowledge graph;

что LLM способен понять любой repository;

что можно заменить всю engineering documentation.
```

---

# 45. MVP MUST HAVE

```text
local repository indexing

code symbols

basic structural relationships

Git-aware incremental updates

Feature / Requirement / Invariant / Decision

explicit semantic relationships

provenance

validity / stale relationships

ctx impact

ctx explain

ctx review

bounded Context Pack
```

---

# 46. MVP SHOULD HAVE

После работающего core:

```text
semantic relation suggestions
verification flow
rename handling improvements
MCP
basic agent integration
```

---

# 47. NOT MVP

Не строить сейчас:

```text
web UI
graph visualization
enterprise cloud
Neo4j
Jira integration
Linear integration
Confluence integration
RBAC
SSO
analytics
multi-repo organization graph
automatic requirement generation
full business ontology
runtime graph
distributed infrastructure
generic graph query language
```

---

# 48. Product milestone order

Предпочтительный порядок проверки гипотез:

## P1

Можно ли построить достаточно точный structural representation code change?

## P2

Полезна ли ручная explicit связь:

```text
code ↔ requirement/invariant
```

для impact analysis?

## P3

Делает ли эта связь `ctx review` полезным?

## P4

Делает ли тот же knowledge Context Pack полезным для coding agent?

## P5

Можно ли дешёво предлагать semantic links автоматически?

## P6

Можно ли поддерживать graph актуальным без большого human maintenance?

---

# 49. Important experimental mindset

Перед сложной automation сначала проверить идеальный вариант вручную.

Например:

до разработки automatic semantic resolution:

1. взять реальные historical PRs;
2. вручную создать correct semantic mappings;
3. посмотреть, насколько хорош `ctx review`.

Если review бесполезен даже при perfect mappings:

> automatic graph construction не надо строить.

Это фундаментальный product principle.

---

# 50. First evaluation corpus

Желательно иметь repository/history, где известны:

```text
несколько meaningful feature changes;
несколько regressions;
несколько refactors;
несколько bug fixes;
несколько business-rule changes.
```

Для каждого PR вручную зафиксировать:

```text
actual affected behavior
actual requirements
actual invariants
expected review findings
```

Использовать это как product benchmark.

---

# 51. Five critical experiments

## Experiment 1 — Perfect graph review

Manual semantic links + historical PRs.

Проверить:

> полезен ли `ctx review`, если mappings идеальны?

---

## Experiment 2 — Behavioral change classification

Можно ли достаточно хорошо отличить:

```text
refactor
```

от:

```text
possible behavior change
```

чтобы review не шумел?

---

## Experiment 3 — Semantic candidate quality

Можно ли по:

```text
identifiers
paths
tests
DB interactions
structural graph
```

получать хорошие top-3 candidates без LLM?

---

## Experiment 4 — Context Pack value

Сравнить coding agent:

```text
without ctx
vs
with ctx
```

---

## Experiment 5 — Semantic maintenance

Прогнать verified mappings через историю repository и измерить:

```text
сколько survives automatically;
сколько correctly becomes stale;
сколько silently becomes wrong.
```

---

# 52. Kill criteria

Проект нужно серьёзно пересмотреть или закрыть, если после нескольких итераций:

## Review is noisy

High-confidence findings регулярно нерелевантны.

## Perfect mappings do not help

Даже вручную созданный semantic graph не улучшает review.

## Context Pack does not improve agents

Agent получает больше context, но не становится эффективнее.

## Semantic maintenance is expensive

Developers вынуждены постоянно чинить mappings.

## Time-to-value is too high

Полезность появляется только после большой ручной документации.

## Search solves almost everything

Если обычный search/RAG стабильно даёт тот же результат дешевле, graph не оправдан.

---

# 53. Long-term moat

Не считать moat:

```text
Rust
Tree-sitter
SQLite
MCP
embeddings
graph schema
```

Potential moat:

```text
verified organization-specific semantic mappings;

history этих mappings;

temporal validity;

provenance and trust;

feedback от real PR workflows;

quality semantic impact analysis;

organization-specific context accumulated over time.
```

---

# 54. Mental model for product decisions

Представляй `ctx` не как database.

Представляй его как **organizational memory for software behavior**.

Code отвечает:

```text
HOW
```

Product context отвечает:

```text
WHY
```

Git отвечает:

```text
WHEN IT CHANGED
```

Provenance отвечает:

```text
WHY WE BELIEVE THIS
```

`ctx` соединяет эти четыре вещи.

---

# 55. North-star interaction

Идеальный interaction через несколько итераций продукта:

Developer:

```text
Fix cancellation so access isn't revoked
when Stripe's webhook arrives late.
```

Agent вызывает `ctx`.

Получает:

```text
Feature:
Subscription cancellation

Requirement:
REQ-SUB-014

When a paid user cancels,
access remains available until paid_until.

Invariant:
INV-SUB-003

Paid entitlement must never terminate
before paid_until.

Relevant code:
SubscriptionService.cancel()
StripeWebhookHandler.handle_subscription_update()

Data:
subscriptions.status
subscriptions.paid_until

Tests:
test_cancel_keeps_access_until_paid_until
test_late_webhook_does_not_revoke_entitlement

Decision:
Stripe events may arrive out of order.

Uncertainty:
StripeWebhookHandler → INV-SUB-003
is inferred, confidence 0.74.
```

После изменения:

```bash id="d1b632"
ctx review
```

возвращает только несколько high-value findings.

Это то состояние продукта, к которому нужно двигаться.

---

# 56. Final product rule

При любом сомнении помни:

> `ctx` не пытается знать всё о codebase.

Он пытается знать **небольшое количество вещей, которые критически важно не забыть при изменении codebase**, и уметь объяснить, почему им можно доверять.

Поэтому главная продуктовая формула:

```text
Product intent
+
Implementation
+
Temporal validity
+
Evidence
=
Trusted change context
```
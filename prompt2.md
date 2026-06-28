# Mission

Ты продолжаешь разработку `ctx`.

К моменту начала этой работы предыдущие агенты должны были завершить:

1. first-class поддержку Go через существующую pluggable language architecture;
2. чтение и анализ DB schema из Goose migrations;
3. чтение и анализ SQLAlchemy models.

Не считай это автоматически корректно завершённым. Сначала изучи:

- `worklog.md`;
- текущий `prompt.md`;
- `product_conclu.md`;
- `eng_conclu.md`;
- architecture/docs;
- `.context/`;
- существующий `ctx-eval`;
- фактический код и тесты.

Проверь реальное состояние репозитория и продолжай от него. Не переписывай уже работающие подсистемы без необходимости.

Главная задача этого scope:

> Превратить отдельно проиндексированные DB schema, ORM models, migrations и database interactions в единую evidence-backed модель persistent state, которую `ctx impact`, `ctx review`, `ctx explain` и Context Pack реально используют для понимания смысла изменений.

Нам не нужен ещё один набор парсеров ради coverage.

Нам нужно приблизиться к цепочке:

```text
Feature / Requirement / Invariant
        ↓
Implementation
        ↓
DB interaction
        ↓
Table.Column
        ↑
ORM model
        ↑
Migration history
        ↓
Tests
```

и уметь объяснить каждую связь.

---

# 1. Сначала зафиксируй baseline

Перед изменениями:

- запусти существующие release gates;
- запусти весь `ctx-eval`;
- проверь `ctx status`;
- проиндексируй текущий repository;
- проверь повторный index как no-op;
- зафиксируй текущую структуру DB-schema IR, если она уже реализована предыдущим агентом;
- проверь Go support и DB-schema support через реальные fixture/e2e сценарии.

Если предыдущая реализация Go/Goose/SQLAlchemy имеет архитектурные или correctness-проблемы, исправь их как prerequisite, но не превращай этот scope в очередную перепись parser layer.

Запиши baseline в `worklog.md`.

---

# 2. Unified persistent-state model

Goose migrations, SQLAlchemy models и runtime/static SQL interactions не должны существовать как три несвязанных мира.

Core не должен знать про `GooseTable` или `SQLAlchemyTable`.

Нужна language/framework-neutral модель persistent state.

Минимально система должна уметь представлять:

- database;
- schema/namespace, если источник его поддерживает;
- table;
- column;
- primary key;
- foreign key;
- unique constraint;
- check constraint;
- index;
- nullable/non-null;
- type;
- default;
- relation между ORM field/model и physical schema entity;
- provenance каждого утверждения.

Не добавляй сущность только потому, что её можно распарсить. Добавляй её, если она участвует в impact/review/context reasoning.

Сохраняй distinction между:

```text
physical schema
declarative application model
migration/history
runtime/static data access
```

Они могут не совпадать.

Такое расхождение — информация, а не ошибка, которую resolver должен молча скрыть.

---

# 3. Identity и entity resolution

Это центральная часть scope.

Нужно детерминированно и консервативно связать представления одной DB-сущности из разных источников.

Например:

```text
Goose:
subscriptions.paid_until

SQLAlchemy:
Subscription.paid_until

SQL access:
UPDATE subscriptions SET paid_until = ...

Code:
SubscriptionService.cancel
```

должны при достаточном evidence вести к одной physical schema identity.

Спроектируй resolver как pure deterministic core.

Предпочтительный порядок сигналов:

1. exact qualified database/schema/table/column identity;
2. explicit table/column declarations из ORM metadata;
3. exact normalized identifiers;
4. framework-defined deterministic naming rules, только если они действительно однозначны;
5. conservative unresolved state.

Не использовать fuzzy/LLM matching для создания FACT edges.

Если соответствие неоднозначно:

```text
UNKNOWN / unresolved
```

лучше, чем неправильная связь.

Не разрешай совпадение только по имени колонки вроде `id`, `status`, `created_at`.

Identity должна быть repository-scoped и стабильной при повторном индексировании.

Добавь regression tests на collision cases.

---

# 4. Provenance и epistemic boundaries

Не ломай существующую модель:

```text
FACT
ASSERTION
INFERENCE
```

Каждый schema/data edge должен объясняться.

Для каждого отношения должна быть возможность определить:

- producer;
- source;
- file/location;
- commit validity;
- normalization/analyzer version;
- confidence;
- evidence;
- stale/current state.

Примеры:

```text
SQLAlchemy model declares mapping to subscriptions
```

может быть deterministic FACT, если metadata статически однозначна.

```text
ORM field X corresponds to physical column Y
```

может быть FACT только если это детерминированно следует из декларации.

```text
These similarly named fields probably represent the same concept
```

не должно незаметно становиться FACT.

Никакого automatic inference promotion.

---

# 5. Migration semantics

Goose migration — это не просто альтернативный способ описать current schema.

Migration primarily говорит:

> какое изменение schema было объявлено в истории.

ORM model говорит:

> какую schema/application representation код ожидает сейчас.

Не теряй это различие.

Если текущая Goose implementation уже реконструирует schema snapshot — проверь correctness и provenance.

Минимально нужно уметь увидеть значимые schema transitions:

- table added/dropped/renamed;
- column added/dropped/renamed;
- type changed;
- nullable changed;
- default changed;
- PK/FK changed;
- unique constraint changed;
- check constraint changed;
- index changed.

Не пытайся полностью интерпретировать произвольный SQL.

Unsupported/dynamic/ambiguous migration должна оставаться unknown с понятной диагностикой.

---

# 6. ORM ↔ physical schema reconciliation

Добавь reconciliation между SQLAlchemy declarative model и physical schema.

Но это не должно быть:

```text
ORM wins
```

или

```text
migration wins
```

Система должна уметь представить divergence.

Например:

```text
ORM expects column users.email
migration-derived schema does not contain users.email
```

или:

```text
migration adds subscriptions.grace_period
ORM representation has no corresponding field
```

Это evidence-backed diagnostic.

Разделяй:

- proven mismatch;
- unresolved mapping;
- unsupported construct.

Не называй unresolved mapping schema drift.

---

# 7. Connect schema to code interactions

У `ctx` уже существует language-neutral DB access model с `ReadsFrom` / `WritesTo`.

Расширь связь до уровня schema entities там, где статический анализ это реально позволяет.

Желаемая модель:

```text
CodeSymbol
   ↓ WRITES_TO
Table
   ↓
Column(s)
```

Если SQL позволяет надёжно определить конкретные columns — сохраняй column-level interaction.

Если известна только table — сохраняй table-level FACT и не выдумывай columns.

Если Go analyzer после предыдущего scope ещё не производит DB interactions, добавь узкую deterministic поддержку распространённых literal SQL execution paths через существующий DB-access IR.

Не строй универсальный Go ORM analyzer.

Не распознавай динамический SQL как точный FACT.

---

# 8. Schema-aware impact

После реализации следующий запрос должен давать полезный результат:

```bash
ctx impact subscriptions.paid_until
```

или эквивалентный supported seed.

Он должен позволять найти bounded neighborhood:

```text
column
→ table
→ readers/writers
→ requirements/invariants
→ direct tests
```

И наоборот:

```text
Requirement
→ implementation
→ DB interaction
→ relevant table/column
```

Не ломай существующие traversal boundaries.

Особенно защити уже найденные классы ошибок:

- semantic hub explosion;
- shared-test bridging;
- inference amplification;
- stale-edge propagation;
- lexical roots leaking past explicit seed;
- unrelated schema entities consuming Context Pack budget.

Schema edges не должны превращать одну популярную таблицу вроде `users` в мост ко всему продукту.

---

# 9. Schema-aware `ctx explain`

Пользователь должен иметь возможность понять:

> Почему ctx считает, что этот код зависит от этой таблицы/колонки?

или:

> Почему SQLAlchemy field связан с этим physical column?

Ответ должен строиться только из сохранённых facts/evidence.

Пример ожидаемой объяснимости:

```text
SubscriptionService.cancel
→ WRITES_TO subscriptions.paid_until
  source: static SQL analysis
  file: ...
  lines: ...
  commit: ...

Subscription.paid_until
→ MAPS_TO subscriptions.paid_until
  source: SQLAlchemy declarative mapping
  file: ...
  lines: ...
```

Не генерируй rationale, которого нет в graph/evidence.

---

# 10. Schema-aware Context Pack

DB schema должна стать полезной частью Context Pack, но не занимать его целиком.

Для задачи вокруг cancellation Context Pack должен иметь шанс содержать:

```text
REQ-SUB-...
INV-SUB-...
SubscriptionService.cancel
subscriptions.status
subscriptions.paid_until
relevant migration/schema evidence
related test
```

а не:

- всю таблицу;
- все migrations;
- все ORM models;
- все consumers популярной таблицы.

Сохраняй hard token budget.

Добавь prioritization:

1. business invariants / requirements;
2. directly affected schema entities;
3. directly interacting implementation;
4. direct tests;
5. migration/ORM evidence;
6. secondary adjacency.

Полный migration body не должен попадать в Context Pack, если достаточно locator + normalized schema change.

---

# 11. Schema-aware review — главный продуктовый результат

Самая важная часть scope.

`ctx review` должен использовать schema changes как deterministic signals, но оставаться conservative.

Нужны сценарии минимум для:

### destructive / contract-relevant changes

- mapped column removed;
- mapped column renamed;
- nullable → non-null;
- type changed;
- FK target changed;
- unique constraint removed/added;
- business-relevant check constraint changed.

### drift

- ORM changed but migration/schema representation did not;
- migration schema changed but ORM still expects old representation;
- code still reads/writes removed or renamed schema entity.

### precision

- unrelated table changed → no unrelated business warning;
- index-only change → не объявлять product behavior violation без evidence;
- migration formatting/comment change → no finding;
- unresolved ORM mapping → uncertainty, не proven drift.

Review findings должны чётко разделять:

```text
observed deterministic schema change
```

и

```text
potential product/requirement impact
```

Нельзя превращать технический schema diff в доказанное нарушение requirement без существующего semantic evidence.

Precision over recall остаётся protected invariant.

---

# 12. Evaluation-first implementation

До или одновременно с production logic добавь ground-truth cases в `ctx-eval`.

Не ограничивайся unit tests parser'ов.

Нужны end-to-end cases минимум:

1. mapped column removed;
2. mapped column renamed;
3. nullable becomes non-null;
4. FK changed;
5. unique constraint changed;
6. ORM changed without matching migration/schema;
7. migration/schema changed without matching ORM;
8. code interaction references removed schema entity;
9. business-critical mapped field changed;
10. unrelated schema change;
11. formatting/comment-only migration change;
12. ambiguous ORM mapping stays unresolved;
13. shared table does not bridge unrelated requirements;
14. explicit schema seed does not pull unrelated lexical roots;
15. same entity observed through migration + ORM + DB interaction resolves consistently.

Каждый case должен проверять не только recall, но где применимо:

- precision;
- classification;
- bounded traversal;
- provenance;
- budget;
- stale behavior.

Если implementation проходит unit tests, но проваливает corpus — работа не закончена.

Если новый eval case обнаруживает старый architectural defect, исправь root cause и добавь focused regression рядом с core logic, как это уже делалось в проекте.

---

# 13. Functional core / imperative shell

Соблюдай существующий architectural direction.

Core logic должна по возможности быть pure:

- identity;
- normalization;
- schema diff;
- entity resolution;
- reconciliation;
- traversal decisions;
- review classification;
- ranking.

Imperative shell:

- filesystem;
- Git;
- Tree-sitter;
- SQLite;
- CLI;
- MCP.

Adapters переводят framework/language-specific representation в normalized IR.

Core не должен импортировать concept уровня SQLAlchemy/Goose.

Следуй SOLID и здравому смыслу, но не создавай abstractions ради abstractions.

Предпочитай маленькие composable types/functions большим service objects.

Не дублируй одинаковую reconciliation/traversal logic в:

- CLI;
- MCP;
- review;
- impact;
- Context Pack.

Public use cases должны использовать одну core policy.

---

# 14. Incrementality и temporal correctness

Schema analysis должна соблюдать те же гарантии, что code indexing:

- Git commit validity;
- atomic update;
- analyzer-version invalidation;
- no duplicate current identities/edges;
- deleted entities retire correctly;
- same-HEAD analyzer upgrade safe;
- unchanged input produces no-op;
- changed migration/model invalidates только затронутую derived knowledge;
- stale semantic assertions не реактивируются автоматически.

Не делай full schema rebuild на каждое изменение, если существующая architecture позволяет корректный incremental path.

Но correctness важнее micro-optimization.

---

# 15. Status / diagnostics

`ctx status` должен сообщать actionable problems, если schema layer не согласован.

Например:

- unresolved schema mappings;
- proven ORM/schema divergence;
- unsupported schema constructs;
- stale schema-derived relationships.

Не превращай status обратно в vanity counters.

Если система не уверена — это должно быть видно.

---

# 16. Dogfooding

После fixture/eval tests используй `ctx` против собственного repository там, где это применимо.

Если repository самого ctx не содержит достаточно реалистичного SQLAlchemy/Goose workload, не делай вид, что dogfooding доказал correctness.

Создай компактный realistic fixture/repository history.

Особенно нужен scenario, где одна продуктовая цепочка проходит:

```text
Requirement / Invariant
→ Python or Go implementation
→ DB interaction
→ physical schema
→ SQLAlchemy or Goose representation
→ tests
```

и затем schema change проходит через `ctx review`.

---

# 17. Что НЕ делать

В этом scope запрещено без доказанной необходимости:

- добавлять Java/TypeScript/Zig/ещё языки;
- добавлять новые ORM/framework parsers кроме того, что нужно для завершения SQLAlchemy/Goose integration;
- добавлять Neo4j/graph DB;
- добавлять network dependency;
- делать LLM обязательным для indexing/resolution;
- строить generic SQL engine;
- строить runtime tracing;
- автоматически создавать semantic business links на основании похожих имён;
- повышать recall ценой большого количества review false positives;
- бесконтрольно расширять ontology;
- переписывать SQLite storage только ради красоты;
- менять public CLI/API без необходимости.

Наша цель — depth, не breadth.

---

# 18. External product validation boundary

После завершения этого scope локальная инженерная система станет глубже, но это всё ещё не доказывает product-market value.

Не выдумывай результаты:

- historical PR precision;
- human review speed;
- agent task success;
- token savings;
- maintenance cost.

Если реального внешнего corpus нет — явно оставь эти метрики `not evaluated`.

Можно улучшить tooling для будущего historical-PR evaluation, если это естественно следует из работы, но не заменяй реальные эксперименты synthetic numbers.

Следующим крупным направлением после этого scope должна стать именно external product validation, а не новый parser.

---

# 19. Release gates

Перед завершением:

- `cargo fmt --all -- --check`;
- strict locked workspace Clippy со всеми targets/features и `-D warnings`;
- полный workspace test suite;
- полный `ctx-eval`;
- release build;
- CLI smoke/e2e;
- MCP regression;
- Docker Compose validation;
- SQLite integrity checks;
- current graph integrity checks;
- clean second index / deterministic no-op;
- relevant Context Pack hard-budget tests;
- `ctx status` без скрытых unresolved/stale проблем либо с честно документированными ожидаемыми diagnostics.

Если Docker/network environment мешает внешней загрузке image — зафиксируй это честно, не выдавай incomplete check за passed.

---

# 20. Definition of Done

Scope закончен только если на репрезентативном fixture можно доказать следующую цепочку:

```text
Business Requirement / Invariant
        ↓
Code symbol
        ↓
READS_FROM / WRITES_TO
        ↓
Physical table / column
        ↑
SQLAlchemy model
        ↑
Goose migration/schema history
```

и:

1. каждая deterministic связь имеет provenance;
2. ambiguous mapping остаётся unresolved;
3. `ctx explain` объясняет цепочку без hallucinated rationale;
4. `ctx impact` возвращает bounded relevant neighborhood;
5. Context Pack включает релевантный persistent state внутри token budget;
6. `ctx review` замечает meaningful schema contract changes;
7. unrelated schema changes не создают unrelated business findings;
8. stale/invalidated relationships ведут себя по существующим epistemic rules;
9. все новые scenarios закреплены в `ctx-eval`;
10. старый evaluation corpus не деградировал;
11. Go/Python/Rust behavior и существующие public use cases не сломаны;
12. worklog и architecture/docs описывают фактически реализованное поведение, а не намерения.

В конце обнови `worklog.md`:

- что реализовано;
- какие defects обнаружил evaluation/dogfooding;
- какие architectural decisions приняты;
- какие ограничения остались;
- результаты всех gates;
- честный следующий приоритет.

Следующий приоритет не должен автоматически быть «добавить ещё parser».

Если schema-aware reasoning уже доказан локальным corpus, рекомендуемый следующий шаг:

> собрать ground-truth historical PR corpus на реальных repositories и измерить review precision / recall, impact usefulness и Context Pack agent-task effectiveness.
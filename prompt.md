# Handoff prompt for the next agent

Ты продолжаешь проект `ctx` в `/home/ks/coding/ctx`. Не начинай заново: локальный продукт собран, использует собственный product context и имеет воспроизводимый evaluation corpus.

## Сначала прочитай

1. `product_conclu.md` — продуктовая гипотеза, эксперименты и kill criteria.
2. `eng_conclu.md` — архитектурные границы и исходные milestones.
3. `README.md` и `docs/architecture.md` — реально поддерживаемый интерфейс.
4. `docs/evaluation.md` — честная граница автоматизированного baseline.
5. `worklog.md` — хронология решений и найденных дефектов.
6. `.context/` — first-party product contracts самого `ctx`.

После чтения проверь `git status --short`, последние коммиты и `ctx status`. Не перезаписывай пользовательские или незнакомые изменения.

## Состояние версии 0.3.0

- Workspace разделён на `ctx-core`, `ctx-app`, `ctx-adapters`, `ctx-cli`, `ctx-mcp` и `ctx-eval`.
- Functional Core / Imperative Shell соблюдается: Git, Tree-sitter, SQLite, терминал и MCP не протекают в core policy.
- Git-aware индекс хранит commit-bounded версии files/symbols/claims, provenance, evidence, staleness и human verification history.
- Встроены Python, Rust и Go modules с language-scoped identity/call resolution и analyzer-version refresh на том же commit. Go canonical paths — directory-based (одно-пакет-на-директорию), а не file-based.
- Статические SQL-литералы внутри известных execution calls (Python/Rust/Go) нормализуются в `DbEntity` и `READS_FROM`/`WRITES_TO` FACT edges. Факты имеют producer, validity, file/line evidence; dynamic SQL не угадывается.
- Табличные/колоночные schema facts читаются также из goose SQL-миграций (`-- +goose Up` only, детерминированный DDL-reader, не dialect parser) и SQLAlchemy declarative моделей (`__tablename__` + `Column`/`mapped_column`) — новый `DEFINES_SCHEMA` FACT edge kind, тот же `DbEntity` граф, то же incremental versioning/evidence, что и у SQL-литералов. Таблица, объявленная только миграцией или ORM-моделью и никогда не читаемая/записываемая кодом, всё равно становится `DbEntity`.
- Data contracts участвуют в `impact`, `explain`, Context Pack, review change signals, status и semantic-candidate scoring.
- Реализованы Git-owned Feature / Requirement / Invariant / Decision, exact mappings, bounded typed traversal, conservative review, token-budgeted Context Packs, heuristic suggestions и accept/reject verification.
- CLI/JSON и read-only stdio MCP используют те же application services. Docker/Compose и non-root runtime включены.
- `ctx-eval` содержит 13 Git-history cases / 67 typed checks, включая formatting noise, rename/move, deletion, added call, stale mapping, shared-test isolation, multi-commit evolution, changed DB write, goose-only schema и SQLAlchemy-only schema.
- Публичная документация: `README.md`, `docs/architecture.md`, `docs/evaluation.md`, `CHANGELOG.md`, Apache-2.0 license.

Точные graph/test counts меняются вместе с кодом; бери их из текущего `ctx status` и финального test output, а не копируй из старого handoff.

## Что закрыто

MVP MUST HAVE и базовый SHOULD HAVE из исходных документов реализованы: incremental local indexing, structural graph, explicit business context, provenance/validity/staleness, impact/explain/review, Context Pack, semantic suggestions, verification и MCP.

Engineering M0–M7 имеют рабочие vertical slices. После MVP добавлены:

- pluggable Python/Rust/Go registry;
- first-party product context (включая новый REQ-DATA-002 для schema extraction);
- deterministic product-quality harness;
- исправления bounded traversal, shared-node isolation, graph identity и cross-file moves;
- evidence-backed static database interaction extraction (Python/Rust/Go);
- table/column-level schema extraction из goose migrations и SQLAlchemy models;
- полный fixture-matrix point `changed DB write`;
- **order-independent cross-file symbol identity matching** — найден и исправлен реальный latent bug 2026-08-18: два файла с одинаково названным/одинаково устроенным helper'ом могли получить корректные отдельные identity только в зависимости от порядка обработки файлов в транзакции (`plan_incremental_index` теперь резервирует exact-canonical-path identity для всей транзакции заранее, до любого same-shape fallback). Это тот же класс бага, что уже дважды чинили раньше (см. `worklog.md`), но впервые сделан по-настоящему order-independent.

## Что честно не закрыто

### Продуктовые эксперименты

Зелёный synthetic corpus не подтверждает product hypothesis. Ещё нужны внешние данные или участники:

- labeled historical PR corpus и precision high-confidence findings;
- impact-understanding A/B с `ctx` и без него;
- agent task success/token efficiency с Context Pack и без него;
- human maintenance cost verified mappings через реальную историю;
- kill-criteria evaluation после нескольких реальных workflow iterations.

Не придумывай результаты этих экспериментов. Если пользователь не дал repository/history/participants, подготовь протокол или импортируемый corpus format, но явно обозначь отсутствие измерения.

### Технические границы

- SQL extraction сознательно неполный: нет dynamic SQL, полного ORM query AST (только declarative model schema, не queries), stored procedures и полного dialect parser.
- goose parsing читает только `-- +goose Up`, не строит accumulated "current schema" через несколько миграций (каждая миграция — свой отдельный FACT); SQLAlchemy recognition требует статического `__tablename__` и не резолвит `Base`/inheritance/relationships/mixins/Alembic history.
- `Endpoint`, `Event`, `ExternalSystem`, `EMITS` и `HANDLES` есть в domain model, но source extraction ещё не реализован.
- Semantic suggestions всё ещё deterministic: без embeddings/LLM; explicit/alias signals требуют реального alias use case.
- TypeScript, Java и Zig modules не реализованы (Go теперь есть); добавлять каждый отдельно с parser unit + mixed-language e2e.
- Go/goose/SQLAlchemy имеют только synthetic fixture/eval покрытие — нет большого реального Go- или SQLAlchemy-репозитория для dogfooding (у самого `ctx` нет Go/SQL исходников).
- Нет систематического large-repository performance benchmark.
- `-v/-vv` и duration diagnostics остаются вторичной observability gap — конкретно не хватает способа узнать через CLI/JSON, **какие именно** semantic relationships stale (сейчас только count; 2026-08-18 пришлось запрашивать `.ctx/ctx.db` напрямую через `sqlite3`, чтобы найти конкретную stale edge).

## Предпочтительный следующий этап

Первый выбор — реальный evaluation, если пользователь предоставляет историю или разрешённый public repository:

1. зафиксировать labeling protocol и ground truth до настройки weights;
2. импортировать несколько реальных PR spans;
3. прогнать current baseline без изменения scoring;
4. записать true/false positives, missed intent, context relevance и maintenance events;
5. только по результатам менять ranking/scoring.

Если внешнего corpus нет, следующие безопасные technical vertical slices, в порядке убывания уверенности, что это не преждевременная генерализация:

1. `ctx status -vv` (или отдельная команда) должен перечислять конкретные stale/rejected relationships вместо одного count — сейчас единственный способ найти конкретную stale edge — прямой `sqlite3` запрос к `.ctx/ctx.db`, что не должно быть штатным способом работы с продуктом.
2. один доказуемый external interaction type (например HTTP endpoint/client call или emitted/handled event), через тот же normalized IR → temporal FACT → evidence → impact/review/context → eval путь. Не строй generic interaction framework заранее.
3. large-repository performance benchmark — сейчас нет ни одного числа о том, как `ctx index`/`ctx impact` масштабируются за пределами этого репозитория (~850 symbols).

## Неприкосновенные правила

- Correctness > provenance > precision > coverage.
- Любая surfaced semantic relation имеет evidence и validity.
- Machine inference не становится fact/assertion без отдельного human decision.
- Dynamic/ambiguous source behavior остаётся unknown, а не guessed FACT.
- Traversal typed, bounded, deterministic; shared tests/Features/data nodes не должны случайно соединять unrelated intent.
- Индекс описывает Git commit и отказывается маркировать uncommitted configured inputs как `HEAD`.
- Language-specific types остаются внутри analyzer adapter; shared IR принадлежит core boundary.
- Сохраняй пользовательские изменения; не применяй destructive Git/filesystem commands.
- Веди `worklog.md`, делай небольшие атомарные commits и осмысленно revalidate stale `.context` mappings после clean commit.

## Release gates

```bash
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
cargo run --locked --quiet -p ctx-eval
cargo build --locked --release --workspace
docker compose config --quiet
docker compose --profile mcp config --quiet
target/release/ctx index
target/release/ctx status
git diff --check
git status --short
```

Финальный graph должен быть current/ready, без unresolved mappings, stale semantics, duplicate current fingerprints, orphan current edges или active calls to non-callable targets. Повторный `ctx index` должен быть no-op.

CLI пока не даёт способа проверить последние три условия напрямую — используй `sqlite3 .ctx/ctx.db` (репозиторий этого проекта см. `worklog.md` 2026-08-18 для точных запросов): `PRAGMA integrity_check`, дубликаты `(repository_id, fingerprint)` среди current edges, current edges на retired nodes, `calls` edges на non-callable `symbol_kind`.

Если меняешь что-то в `crates/`, переиндексируй **сам `ctx`** (`target/release/ctx index` в корне репозитория) до финальных gates, а не только запускай test suite — именно так 2026-08-18 нашёлся реальный order-dependent identity bug, который ни один unit-тест до этого не поймал.

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

## Состояние версии 0.2.0

- Workspace разделён на `ctx-core`, `ctx-app`, `ctx-adapters`, `ctx-cli`, `ctx-mcp` и `ctx-eval`.
- Functional Core / Imperative Shell соблюдается: Git, Tree-sitter, SQLite, терминал и MCP не протекают в core policy.
- Git-aware индекс хранит commit-bounded версии files/symbols/claims, provenance, evidence, staleness и human verification history.
- Встроены Python и Rust modules с language-scoped identity/call resolution и analyzer-version refresh на том же commit.
- Статические SQL-литералы внутри известных execution calls нормализуются в `DbEntity` и `READS_FROM`/`WRITES_TO` FACT edges. Факты имеют producer, validity, file/line evidence; dynamic SQL не угадывается.
- Data contracts участвуют в `impact`, `explain`, Context Pack, review change signals, status и semantic-candidate scoring.
- Реализованы Git-owned Feature / Requirement / Invariant / Decision, exact mappings, bounded typed traversal, conservative review, token-budgeted Context Packs, heuristic suggestions и accept/reject verification.
- CLI/JSON и read-only stdio MCP используют те же application services. Docker/Compose и non-root runtime включены.
- `ctx-eval` содержит 11 Git-history cases / 59 typed checks, включая formatting noise, rename/move, deletion, added call, stale mapping, shared-test isolation, multi-commit evolution и changed DB write.
- Публичная документация: `README.md`, `docs/architecture.md`, `docs/evaluation.md`, `CHANGELOG.md`, Apache-2.0 license.

Точные graph/test counts меняются вместе с кодом; бери их из текущего `ctx status` и финального test output, а не копируй из старого handoff.

## Что закрыто

MVP MUST HAVE и базовый SHOULD HAVE из исходных документов реализованы: incremental local indexing, structural graph, explicit business context, provenance/validity/staleness, impact/explain/review, Context Pack, semantic suggestions, verification и MCP.

Engineering M0–M7 имеют рабочие vertical slices. После MVP добавлены:

- pluggable Python/Rust registry;
- first-party product context;
- deterministic product-quality harness;
- исправления bounded traversal, shared-node isolation, graph identity и cross-file moves;
- evidence-backed static database interaction extraction;
- полный fixture-matrix point `changed DB write`.

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

- SQL extraction сознательно неполный: нет dynamic SQL, ORM AST, stored procedures и полного dialect parser.
- `Endpoint`, `Event`, `ExternalSystem`, `EMITS` и `HANDLES` есть в domain model, но source extraction ещё не реализован.
- Semantic suggestions всё ещё deterministic: без embeddings/LLM; explicit/alias signals требуют реального alias use case.
- TypeScript, Go, Java и Zig modules не реализованы; добавлять каждый отдельно с parser unit + mixed-language e2e.
- Нет систематического large-repository performance benchmark.
- `-v/-vv` и duration diagnostics остаются вторичной observability gap.

## Предпочтительный следующий этап

Первый выбор — реальный evaluation, если пользователь предоставляет историю или разрешённый public repository:

1. зафиксировать labeling protocol и ground truth до настройки weights;
2. импортировать несколько реальных PR spans;
3. прогнать current baseline без изменения scoring;
4. записать true/false positives, missed intent, context relevance и maintenance events;
5. только по результатам менять ranking/scoring.

Если внешнего corpus нет, следующий безопасный technical vertical slice — один доказуемый external interaction type (например HTTP endpoint/client call или emitted/handled event), через тот же normalized IR → temporal FACT → evidence → impact/review/context → eval путь. Не строй generic interaction framework заранее.

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

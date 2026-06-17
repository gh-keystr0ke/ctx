# Handoff prompt for the next agent

Ты продолжаешь вести проект `ctx` в репозитории `/home/ks/coding/ctx`.
Не начинай реализацию заново: локальный технический MVP уже собран, протестирован
и использует собственный product context. Твоя задача — сохранить достигнутые
гарантии, честно проверить оставшиеся пункты исходного замысла и двигаться к
следующему доказуемому vertical slice.

## Сначала прочитай

В таком порядке:

1. `product_conclu.md` — продуктовая гипотеза, MVP, эксперименты и kill criteria.
2. `eng_conclu.md` — архитектурные правила и milestones M0–M7.
3. `README.md` — фактически поддерживаемый публичный интерфейс и ограничения.
4. `docs/architecture.md` — границы workspace, validity и traversal policy.
5. `worklog.md` — подробная хронология решений, найденных дефектов и проверок.
6. `.context/` — текущий first-party product context самого `ctx`.

После чтения проверь `git status --short`, последние коммиты и `ctx status`.
Существующий пользовательский код или незнакомые изменения не перезаписывай.

## Текущее состояние

На момент подготовки handoff реализация прошла полный release gate, а индекс был
актуален для `HEAD` и имел health `ready`.

- Rust workspace разделён на `ctx-core`, `ctx-app`, `ctx-adapters`, `ctx-cli` и
  `ctx-mcp`.
- `ctx-core` содержит чистые domain decisions; Git, Tree-sitter, SQLite, CLI и
  MCP остаются adapters/imperative shell.
- SQLite — локальный temporal source of truth с node/edge versions, provenance,
  evidence, annotations, aliases и derivation ownership.
- Реализовано Git-aware incremental indexing с добавлением, изменением,
  удалением, scope reconciliation, conservative identity matching, структурной
  invalidation и semantic staleness.
- Анализаторы подключаются через compile-time `AnalyzerModule` и общий
  `AnalyzerRegistry`, возвращающий normalized IR.
- Встроены Python и Rust. Их stable identities и call resolution изолированы по
  языкам. Rust поддерживает workspace crate paths, trait implementations,
  generic trait arguments и analyzer-version reindexing на том же Git commit.
- Реализованы четыре типа business context: Feature, Requirement, Invariant и
  Decision, включая YAML и Markdown front matter, exact symbol mappings,
  provenance и evidence.
- Работают команды `init`, `index`, `status`, `impact`, `explain`, `review`,
  `context`, `verify` и `serve --mcp`; meaningful output доступен в JSON.
- `ctx review` консервативен: formatting, rename и likely-refactor noise
  подавляются; сильные findings содержат intent, evidence, tests, uncertainty и
  reviewer action.
- Context Pack имеет token budget, typed/bounded traversal, priorities,
  evidence и uncertainty.
- `ctx verify` создаёт heuristic candidates и сохраняет accept/reject решения;
  accepted inference становится отдельным human assertion, а не переписанным
  fact.
- MCP — read-only stdio adapter с пятью tools: `get_context`, `get_impact`,
  `explain_relation`, `find_requirements`, `review_change`.
- Есть Dockerfile и Compose, включая профиль `mcp`.
- В `.context/` находится 19 first-party документов: 4 Features,
  6 Requirements, 5 Invariants и 4 Decisions. Они содержат 69 exact mappings к
  реализации/тестам и создают 83 активных assertions без unresolved/stale
  claims.
- Последняя проверенная graph snapshot содержала 37 files, 627 symbols и
  1,139 structural facts.
- Полный workspace suite содержит 51 проходящий тест.

Ключевые завершённые коммиты и их продолжение смотри через `git log` и
`worklog.md`. В частности:

- `52cd885` / `64b01a6` — Rust module и language-aware identities;
- `58b5f75` — same-commit analyzer refresh;
- `354d898` — first-party `.context` corpus;
- `fbf6b66` — узкие архитектурные mappings;
- `6b33090` — настоящий bounded typed impact traversal;
- `3cfcbd8` — shared-node isolation в Context Pack;
- `3164d02` — revalidated epistemic invariant;
- `29b92bb` — итоговая context release verification.

## Что из исходных документов уже выполнено

`product_conclu.md` MVP MUST HAVE закрыт полностью:

- local repository indexing;
- code symbols и basic structural relationships;
- Git-aware incremental updates;
- Feature / Requirement / Invariant / Decision;
- explicit semantic relationships;
- provenance, validity и stale relationships;
- `ctx impact`, `ctx explain`, `ctx review`;
- bounded Context Pack.

MVP SHOULD HAVE реализован на базовом уровне:

- semantic suggestions;
- human verification flow;
- conservative rename handling;
- MCP;
- basic agent integration через Context Pack/MCP.

Engineering milestones M0–M5 и M7 имеют законченные vertical slices. M6 имеет
работающий минимальный slice, но ещё не весь набор сигналов, перечисленный в
engineering prompt.

## Что ещё не выполнено

### 1. Не доказана продуктовая гипотеза

Главный оставшийся этап — не ещё одна инфраструктурная feature, а evaluation из
разделов 49–52 `product_conclu.md`:

- нет размеченного corpus реальных/historical PR с ground truth;
- не измерена precision high-confidence review findings;
- не сравнивалось время понимания impact с `ctx` и без него;
- не измерена Context Pack efficiency и agent task success;
- не измерена human maintenance cost verified mappings;
- пять critical experiments и kill criteria ещё не оформлены результатами.

Нельзя объявлять эти гипотезы подтверждёнными только потому, что unit/e2e tests
проходят.

### 2. M6 semantic suggestions остаётся базовым

Сейчас scoring использует lexical overlap, structural adjacency и test
correlation. В `ResolutionScore` поля `explicit`/`alias` фактически нулевые,
`semantic_similarity` отсутствует, а DB interaction signal не извлекается.
Verification priority в основном зависит от вида intent и ещё не учитывает
change frequency, usage или реальную maintenance value.

Расширяй эти signals только после измерения текущего baseline. Не добавляй LLM
или embeddings, пока deterministic signals не доказали свой предел.

### 3. Data/interaction graph подготовлен, но не извлекается

Domain model содержит `Endpoint`, `DbEntity`, `Event`, `ExternalSystem` и
relations `READS_FROM`, `WRITES_TO`, `EMITS`, `HANDLES`, но текущие Python/Rust
анализаторы их не создают. Поэтому fixture-пункт про changed DB write и часть
north-star Context Pack пока не покрыты настоящим extraction.

### 4. Вторичные engineering gaps

- Нужен систематический fixture/evaluation matrix для body change, rename,
  symbol move, deletion, added call, changed DB write, stale mapping, linked
  test и unrelated refactor. Отдельные unit/e2e проверки уже есть, но нет
  единого benchmark с ground truth.
- `-v` показывает indexing diagnostics, однако полноценного различия `-v/-vv`
  и измерения index duration нет.
- Производительность проверена dogfooding на этом workspace, но не измерена на
  большом repository/monorepo.
- TypeScript, Go, Java и Zig ещё не реализованы. Extension seam документирован;
  добавляй каждый язык отдельным parser module с unit и mixed-language e2e
  coverage. Dynamic shared libraries сейчас сознательно не поддерживаются.

## Что сознательно не нужно строить

Не считай отсутствием MVP и не реализуй без нового прямого требования:

- web UI или graph visualization;
- cloud/enterprise backend;
- GitHub/GitLab checks;
- Jira/Linear/Confluence integrations;
- RBAC/SSO;
- multi-repository organization graph;
- runtime tracing;
- Neo4j/vector database/generic graph query language;
- automatic requirement generation;
- hidden LLM inference.

Эти пункты в исходных документах помечены как future или NOT MVP.

## Приоритетная следующая миссия

Построй минимальный, воспроизводимый evaluation vertical slice для проверки
основной продуктовой гипотезы. Не пытайся закрыть все пять экспериментов одной
огромной системой.

Предпочтительный порядок:

1. Зафиксируй machine-readable ground truth schema для evaluation case:
   Git base/change, ожидаемая classification, ожидаемые affected intent IDs,
   допустимые/запрещённые findings, обязательные Context Pack items.
2. Создай небольшой realistic Git-history corpus. Начни с subscriptions и
   добавь как минимум:
   - meaningful cancellation behavior change;
   - formatting-only change;
   - unrelated refactor;
   - rename/move;
   - deleted contract implementation;
   - stale semantic mapping;
   - shared-test case, который не должен соединять unrelated requirements.
3. Реализуй deterministic Rust evaluation runner или integration harness,
   который прогоняет текущие application use cases, сравнивает результат с
   ground truth и выдаёт machine-readable summary. Не дублируй core logic.
4. Считай как минимум surfaced true/false positives, missed required intent,
   unexpected Context Pack items и budget compliance. Не называй маленький
   synthetic corpus статистически значимым.
5. Запиши baseline в документацию и `worklog.md`: какие гипотезы проверены,
   какие только автоматизированы, какие требуют human/agent A/B experiment.
6. Только по результатам baseline выбирай следующий implementation gap:
   semantic scoring, DB interactions, ranking или parser breadth.

Хороший первый результат — не максимальная метрика, а воспроизводимый benchmark,
который способен поймать regression наподобие прежнего unbounded traversal.

## Неприкосновенные правила реализации

- Correctness > provenance > precision > coverage.
- Functional Core / Imperative Shell сохраняется.
- Не протаскивай Git/SQLite/Tree-sitter/terminal types в `ctx-core`.
- Не превращай inference в fact/assertion без отдельного human decision.
- Любая surfaced semantic relation должна иметь evidence и validity.
- Traversal остаётся typed, bounded и deterministic; test/Feature не должны
  становиться случайными мостами между unrelated intent.
- Индекс описывает Git commit. Не сохраняй uncommitted configured source или
  `.context` bytes как будто они принадлежат `HEAD`.
- Новая language-specific логика остаётся внутри analyzer module и normalized
  IR.
- Не добавляй generic abstractions без concrete pressure.
- Сохраняй пользовательские/параллельные изменения и не применяй destructive
  Git/filesystem commands.

## Рабочий процесс

- Веди подробный хронологический журнал в `worklog.md`: причина изменения,
  найденный дефект, решение, проверки и следующий шаг.
- Делай маленькие атомарные Git commits. Не смешивай feature, unrelated cleanup
  и документацию, если их можно проверить независимо.
- Если меняется symbol, связанный из `.context`, после clean commit выполни
  `ctx index`, проверь stale claim через `ctx explain`, а затем revalidate mapping
  осмысленным изменением context document. Не редактируй SQLite вручную для
  сокрытия staleness.
- `ctx index` принимает только committed configured source/context. Сначала
  commit, затем dogfood index.
- После каждого законченного slice обновляй публичную документацию и current
  limits, если поведение действительно изменилось.

Обязательные финальные gates:

```bash
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
cargo build --locked --release --workspace
docker compose config --quiet
docker compose --profile mcp config --quiet
target/release/ctx index
target/release/ctx status
git status --short
```

Финальный `ctx status` должен быть `ready`, index — current, configured inputs —
clean, unresolved mappings и stale semantics — zero. Немедленный повторный
`ctx index` должен быть no-op.

## Definition of done следующего этапа

- Evaluation corpus и ground truth хранятся в Git и кратко документированы.
- Harness детерминированно запускается одной командой и имеет regression tests.
- Есть хотя бы один positive behavior-change case и несколько negative/noise
  cases.
- Результат показывает precision/noise и Context Pack relevance, а не vanity
  graph-size metrics.
- Ограничения corpus и ещё не проведённые human/agent experiments названы
  явно.
- Все release gates проходят.
- `worklog.md` содержит историю этапа, commits атомарны, worktree чистый, а
  dogfood graph снова `ready` на текущем `HEAD`.


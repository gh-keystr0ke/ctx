use super::{
    ApiParam, BTreeMap, BTreeSet, CallResolution, Cli, CliError, Deserialize, EndpointTrace,
    ExportManifest, ExportedDocument, ExportedEndpoint, ExternalCallContract,
    FEDERATION_SCHEMA_VERSION, FederatedRepositoryData, FederationCommand, FederationError,
    FederationSyncState, GitRepo, GitRepository, GraphStore, LocalCall, NeighborRegistry,
    ParamSource, Path, PathBuf, PlannedNodeAttributes, ProcessCommand, RegistryNeighbor,
    RelationKind, Serialize, SqliteStore, TerminalReason, TraceBudget, TraceResolver, Utc,
    VisitedKey, database_path, env, json, matching_resolutions, neighbor_head, parse_method_path,
    path_template, require_service_name, resolve_endpoint_seeds, short_oid, trace_endpoint,
};

#[derive(Serialize)]
struct NeighborSyncSuccess {
    name: String,
    path: String,
    source_commit: String,
    documents: usize,
    endpoints: usize,
    resolutions: usize,
}

#[derive(Serialize)]
struct NeighborSyncFailure {
    name: String,
    path: String,
    error: String,
}

#[derive(Serialize)]
struct SyncReport {
    synced: Vec<NeighborSyncSuccess>,
    errors: Vec<NeighborSyncFailure>,
    unresolved_calls: Vec<ExternalCallContract>,
}

pub(super) fn sync(cli: &Cli, git: &GitRepo) -> Result<(), CliError> {
    require_service_name(git)?;
    let registry = NeighborRegistry::load(git.root())?;
    let database_path = database_path(git.root())?;
    let mut store = SqliteStore::open(&database_path, git.context_root())?;
    let repository = git.descriptor()?;
    let graph = store.load_graph(&repository.id)?;
    let local_commit = git.head()?.oid.to_string();
    let calls = external_call_contracts(&graph);
    let binary = federation_binary()?;
    let synced_at = Utc::now().to_rfc3339();
    let mut successes = Vec::new();
    let mut failures = Vec::new();

    for neighbor in &registry.neighbors {
        let result = sync_neighbor(
            &mut store,
            &binary,
            neighbor,
            &local_commit,
            &synced_at,
            &calls,
        );
        match result {
            Ok(success) => successes.push(success),
            Err(error) => failures.push(NeighborSyncFailure {
                name: neighbor.name.clone(),
                path: neighbor.path.clone(),
                error,
            }),
        }
    }
    let all_endpoints = registry
        .neighbors
        .iter()
        .filter_map(|neighbor| store.federated_repository(&neighbor.name).ok())
        .flat_map(|data| data.endpoints)
        .collect::<Vec<_>>();
    let unresolved_calls = unresolved_calls(&calls, &all_endpoints);
    let report = SyncReport {
        synced: successes,
        errors: failures,
        unresolved_calls,
    };
    print_sync_report(cli, &report)?;
    Ok(())
}

fn sync_neighbor(
    store: &mut SqliteStore,
    binary: &Path,
    neighbor: &RegistryNeighbor,
    local_commit: &str,
    synced_at: &str,
    calls: &[ExternalCallContract],
) -> Result<NeighborSyncSuccess, String> {
    let export_path = PathBuf::from(&neighbor.path).join(".ctx/export.json");
    let manifest = export_neighbor(binary, neighbor, &export_path)?;
    if manifest.schema_version != FEDERATION_SCHEMA_VERSION {
        return Err(CliError::FederationSchemaMismatch {
            name: neighbor.name.clone(),
            actual: manifest.schema_version,
            expected: FEDERATION_SCHEMA_VERSION,
        }
        .to_string());
    }
    if manifest.service_name != neighbor.name {
        return Err(CliError::FederationIdentityMismatch {
            name: neighbor.name.clone(),
            exported: manifest.service_name,
        }
        .to_string());
    }
    let resolutions = matching_resolutions(
        &neighbor.name,
        &manifest.source_commit,
        local_commit,
        synced_at,
        calls,
        &manifest.endpoints,
    );
    let state = FederationSyncState {
        source_repo: neighbor.name.clone(),
        source_path: neighbor.path.clone(),
        source_commit: manifest.source_commit.clone(),
        synced_at: synced_at.to_owned(),
        schema_version: manifest.schema_version,
    };
    store
        .replace_federated_repository(&state, &manifest, &resolutions)
        .map_err(|error| error.to_string())?;
    Ok(NeighborSyncSuccess {
        name: neighbor.name.clone(),
        path: neighbor.path.clone(),
        source_commit: manifest.source_commit,
        documents: manifest.documents.len(),
        endpoints: manifest.endpoints.len(),
        resolutions: resolutions.len(),
    })
}

fn print_sync_report(cli: &Cli, report: &SyncReport) -> Result<(), CliError> {
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for success in &report.synced {
            println!(
                "Synced {} at {} ({} documents, {} endpoints, {} FEDERATED_MATCH records)",
                success.name,
                short_oid(&success.source_commit),
                success.documents,
                success.endpoints,
                success.resolutions
            );
        }
        for failure in &report.errors {
            eprintln!("Neighbor {} failed: {}", failure.name, failure.error);
        }
        for call in &report.unresolved_calls {
            println!(
                "Unresolved: {} {} from {} does not resolve to any known neighbor",
                call.method.as_str(),
                call.path_template,
                call.handler
            );
        }
    }
    Ok(())
}

fn export_neighbor(
    binary: &Path,
    neighbor: &RegistryNeighbor,
    export_path: &Path,
) -> Result<ExportManifest, String> {
    let output = ProcessCommand::new(binary)
        .current_dir(&neighbor.path)
        .arg("--json")
        .arg("export")
        .arg("--out")
        .arg(export_path)
        .output()
        .map_err(|error| {
            format!(
                "could not run ctx for neighbor '{}': {error}",
                neighbor.name
            )
        })?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if message.is_empty() {
            format!("neighbor ctx exited with {}", output.status)
        } else {
            message
        });
    }
    ExportManifest::read(export_path).map_err(|error| error.to_string())
}

fn external_call_contracts(graph: &ctx_core::graph::GraphSnapshot) -> Vec<ExternalCallContract> {
    let mut calls = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == RelationKind::CallsExternal)
        .filter_map(|edge| {
            let source = graph.nodes.get(&edge.source)?;
            let target = graph.nodes.get(&edge.target)?;
            let PlannedNodeAttributes::ExternalCall { call } = &target.attributes else {
                return None;
            };
            Some(ExternalCallContract {
                stable_key: edge.fingerprint.clone(),
                handler: source.identifier().to_owned(),
                method: call.method,
                url: call.url.clone(),
                path_template: path_template(&call.url)?,
            })
        })
        .collect::<Vec<_>>();
    calls.sort_by(|left, right| left.stable_key.cmp(&right.stable_key));
    calls.dedup();
    calls
}

fn unresolved_calls(
    calls: &[ExternalCallContract],
    endpoints: &[ExportedEndpoint],
) -> Vec<ExternalCallContract> {
    let resolved = calls
        .iter()
        .filter(|call| {
            endpoints.iter().any(|endpoint| {
                call.method == endpoint.method
                    && path_template(&endpoint.path).as_deref() == Some(call.path_template.as_str())
            })
        })
        .map(|call| call.stable_key.as_str())
        .collect::<BTreeSet<_>>();
    calls
        .iter()
        .filter(|call| !resolved.contains(call.stable_key.as_str()))
        .cloned()
        .collect()
}

#[derive(Serialize)]
struct FederationListEntry {
    name: String,
    path: String,
    synced_at: Option<String>,
    source_commit: Option<String>,
    stale: Option<bool>,
}

#[derive(Serialize)]
struct FederationShowReport {
    name: String,
    state: FederationSyncState,
    documents: Vec<ExportedDocument>,
    endpoints: Vec<ExportedEndpoint>,
    resolutions: Vec<ctx_adapters::federation::FederatedResolution>,
    unresolved_calls: Vec<ExternalCallContract>,
}

pub(super) fn federation(
    cli: &Cli,
    git: &GitRepo,
    command: &FederationCommand,
) -> Result<(), CliError> {
    let registry = NeighborRegistry::load(git.root())?;
    let database_path = database_path(git.root())?;
    let store = SqliteStore::open(&database_path, git.context_root())?;
    match command {
        FederationCommand::List => federation_list(cli, &registry, &store),
        FederationCommand::Show { name } => federation_show(cli, git, &registry, &store, name),
    }
}

fn federation_list(
    cli: &Cli,
    registry: &NeighborRegistry,
    store: &SqliteStore,
) -> Result<(), CliError> {
    let states = store
        .federation_sync_states()?
        .into_iter()
        .map(|state| (state.source_repo.clone(), state))
        .collect::<BTreeMap<_, _>>();
    let entries = registry
        .neighbors
        .iter()
        .map(|neighbor| {
            let state = states.get(&neighbor.name);
            FederationListEntry {
                name: neighbor.name.clone(),
                path: neighbor.path.clone(),
                synced_at: state.map(|value| value.synced_at.clone()),
                source_commit: state.map(|value| value.source_commit.clone()),
                stale: state.and_then(|value| {
                    neighbor_head(Path::new(&neighbor.path)).map(|head| head != value.source_commit)
                }),
            }
        })
        .collect::<Vec<_>>();
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"neighbors": entries}))?
        );
    } else {
        print_federation_list(&entries);
    }
    Ok(())
}

fn print_federation_list(entries: &[FederationListEntry]) {
    if entries.is_empty() {
        println!("No neighbors registered.");
        return;
    }
    println!("NAME\tPATH\tSYNCED_AT\tSOURCE_COMMIT\tSTALE?");
    for entry in entries {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            entry.name,
            entry.path,
            entry.synced_at.as_deref().unwrap_or("never"),
            entry
                .source_commit
                .as_deref()
                .map_or_else(|| "-".to_owned(), short_oid),
            entry
                .stale
                .map_or("unknown", |stale| if stale { "yes" } else { "no" })
        );
    }
}

fn federation_show(
    cli: &Cli,
    git: &GitRepo,
    registry: &NeighborRegistry,
    store: &SqliteStore,
    name: &str,
) -> Result<(), CliError> {
    if !registry
        .neighbors
        .iter()
        .any(|neighbor| neighbor.name == name)
    {
        return Err(FederationError::UnknownNeighbor(name.to_owned()).into());
    }
    let FederatedRepositoryData {
        state,
        documents,
        endpoints,
        resolutions,
    } = store.federated_repository(name)?;
    let state = state.ok_or_else(|| CliError::NoFederationData(name.to_owned()))?;
    let repository = git.descriptor()?;
    let calls = external_call_contracts(&store.load_graph(&repository.id)?);
    let all_endpoints = registry
        .neighbors
        .iter()
        .filter_map(|neighbor| store.federated_repository(&neighbor.name).ok())
        .flat_map(|data| data.endpoints)
        .collect::<Vec<_>>();
    let report = FederationShowReport {
        name: name.to_owned(),
        state,
        documents,
        endpoints,
        resolutions,
        unresolved_calls: unresolved_calls(&calls, &all_endpoints),
    };
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_federation_show(&report);
    }
    Ok(())
}

fn print_federation_show(report: &FederationShowReport) {
    println!(
        "{} at {} (synced {})",
        report.name,
        short_oid(&report.state.source_commit),
        report.state.synced_at
    );
    println!("Public documents:");
    for document in &report.documents {
        println!("  - {}: {}", document.id, document.title);
    }
    println!("Endpoints:");
    for endpoint in &report.endpoints {
        println!(
            "  - {} {} -> {}{}",
            endpoint.method.as_str(),
            endpoint.path,
            endpoint.handler,
            format_params(&endpoint.params)
        );
    }
    println!("FEDERATED_MATCH records:");
    for resolution in &report.resolutions {
        println!(
            "  - {} {} from {} -> {}{}",
            resolution.call.method.as_str(),
            resolution.call.path_template,
            resolution.call.handler,
            resolution.endpoint.handler,
            format_params(&resolution.endpoint.params)
        );
    }
    for call in &report.unresolved_calls {
        println!(
            "  - unresolved: {} {} from {} does not resolve to any known neighbor",
            call.method.as_str(),
            call.path_template,
            call.handler
        );
    }
}

/// Renders a compact `(name:source[?][:type], ...)` contract summary for a
/// human-readable listing, empty when the endpoint declares no parameters.
fn format_params(params: &[ApiParam]) -> String {
    if params.is_empty() {
        return String::new();
    }
    let rendered = params
        .iter()
        .map(|param| {
            let source = match param.source {
                ParamSource::Path => "path",
                ParamSource::Query => "query",
                ParamSource::Body => "body",
            };
            let optional = if param.required { "" } else { "?" };
            param.type_hint.as_deref().map_or_else(
                || format!("{}:{source}{optional}", param.name),
                |type_hint| format!("{}:{source}{optional}:{type_hint}", param.name),
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(" ({rendered})")
}

pub(super) fn federation_binary() -> Result<PathBuf, CliError> {
    Ok(match env::var_os("CTX_FEDERATION_BINARY") {
        Some(path) => PathBuf::from(path),
        None => env::current_exe()?,
    })
}

#[derive(Serialize, Deserialize)]
struct TraceContinuation {
    budget: TraceBudget,
    visited: BTreeSet<VisitedKey>,
    #[serde(default)]
    verbose: bool,
}

impl TraceContinuation {
    fn decode(raw: &str) -> Result<Self, CliError> {
        serde_json::from_str(raw)
            .map_err(|error| CliError::InvalidTraceContinuation(error.to_string()))
    }
}

/// Matches an outbound call against every registered neighbor's last
/// synchronized manifest (never a live fetch/index/sync -- `ADR-FEDERATION-003`
/// reads one synchronized snapshot per service) and, on a fresh-enough match,
/// continues the trace by invoking that neighbor's own `ctx` binary in its
/// own checkout so only that neighbor's own process decides what of its
/// graph is traceable.
pub(super) struct CliFederationResolver<'a> {
    pub(super) registry: &'a NeighborRegistry,
    pub(super) store: &'a SqliteStore,
    pub(super) binary: &'a Path,
    pub(super) verbose: bool,
}

impl TraceResolver for CliFederationResolver<'_> {
    fn resolve(
        &mut self,
        call: &LocalCall,
        budget: TraceBudget,
        visited: &BTreeSet<VisitedKey>,
    ) -> CallResolution {
        let Some(call_template) = path_template(&call.url) else {
            return CallResolution::Unresolved(TerminalReason::NoNeighborMatch);
        };
        for neighbor in &self.registry.neighbors {
            let Ok(data) = self.store.federated_repository(&neighbor.name) else {
                continue;
            };
            let Some(state) = &data.state else {
                continue;
            };
            let Some(endpoint) = data.endpoints.iter().find(|endpoint| {
                endpoint.method == call.method
                    && path_template(&endpoint.path).as_deref() == Some(call_template.as_str())
            }) else {
                continue;
            };
            let Some(current_head) = neighbor_head(Path::new(&neighbor.path)) else {
                return CallResolution::Unresolved(TerminalReason::NeighborUnavailable {
                    service: neighbor.name.clone(),
                });
            };
            if current_head != state.source_commit {
                return CallResolution::Unresolved(TerminalReason::NeighborStale {
                    service: neighbor.name.clone(),
                });
            }
            return self.cross(neighbor, endpoint, budget, visited);
        }
        CallResolution::Unresolved(TerminalReason::NoNeighborMatch)
    }
}

impl CliFederationResolver<'_> {
    fn cross(
        &self,
        neighbor: &RegistryNeighbor,
        endpoint: &ExportedEndpoint,
        budget: TraceBudget,
        visited: &BTreeSet<VisitedKey>,
    ) -> CallResolution {
        let unavailable = || {
            CallResolution::Unresolved(TerminalReason::NeighborUnavailable {
                service: neighbor.name.clone(),
            })
        };
        let Ok(payload) = serde_json::to_string(&TraceContinuation {
            budget,
            visited: visited.clone(),
            verbose: self.verbose,
        }) else {
            return unavailable();
        };
        let target = format!("{} {}", endpoint.method.as_str(), endpoint.path);
        let Ok(output) = ProcessCommand::new(self.binary)
            .current_dir(&neighbor.path)
            .arg("trace")
            .arg(&target)
            .arg("--federation-continuation")
            .arg(&payload)
            .output()
        else {
            return unavailable();
        };
        if !output.status.success() {
            return unavailable();
        }
        let Ok(subtree) = serde_json::from_slice::<EndpointTrace>(&output.stdout) else {
            return unavailable();
        };
        if subtree.service != neighbor.name {
            return unavailable();
        }
        CallResolution::Crosses(Box::new(subtree))
    }
}

pub(super) fn trace(
    cli: &Cli,
    git: &GitRepo,
    target: &str,
    continuation: Option<&str>,
) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let store = SqliteStore::open(&database_path, git.context_root())?;
    let repository = git.descriptor()?;
    let graph = store.load_graph(&repository.id)?;
    let local_commit = git.head()?.oid.to_string();
    let service = git.service_name().unwrap_or("").to_owned();
    let registry = NeighborRegistry::load(git.root())?;
    let binary = federation_binary()?;
    let continuation = continuation.map(TraceContinuation::decode).transpose()?;
    let verbose = continuation.as_ref().map_or(cli.verbose > 0, |c| c.verbose);

    let seeds = resolve_endpoint_seeds(target, &graph)
        .map_err(|error| hint_not_found(error, target, &graph))?;
    let mut reports = Vec::with_capacity(seeds.len());
    for seed in seeds {
        let mut budget = continuation
            .as_ref()
            .map_or(TraceBudget::root(), |c| c.budget);
        let mut visited = continuation
            .as_ref()
            .map_or_else(BTreeSet::new, |c| c.visited.clone());
        let mut resolver = CliFederationResolver {
            registry: &registry,
            store: &store,
            binary: &binary,
            verbose,
        };
        let mut report = trace_endpoint(
            seed,
            &graph,
            &service,
            &local_commit,
            &mut budget,
            &mut visited,
            &mut resolver,
        );
        if verbose {
            attach_product_context(&mut report, &graph);
        }
        reports.push(report);
    }

    if continuation.is_some() {
        let root = reports
            .into_iter()
            .next()
            .ok_or_else(|| ctx_core::trace::TraceError::NotFound(target.to_owned()))?;
        println!("{}", serde_json::to_string(&root)?);
        return Ok(());
    }
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"query": target, "traces": reports}))?
        );
        return Ok(());
    }
    let total = reports.len();
    for (index, report) in reports.iter().enumerate() {
        if total > 1 {
            println!("[{}/{total}]", index + 1);
        }
        print_endpoint_trace(report, 0);
        println!();
    }
    Ok(())
}

/// Looks up the Features/Requirements mapped to `trace`'s own handler in
/// `graph` (the graph that produced `trace` -- never a crossed neighbor's,
/// which this process never loaded) and attaches them as a display-only
/// annotation. A no-op when the handler is unmapped or unknown; `ctx-core`'s
/// traversal itself never calls this.
pub(super) fn attach_product_context(
    trace: &mut EndpointTrace,
    graph: &ctx_core::graph::GraphSnapshot,
) {
    let Some(handler) = trace.handler.clone() else {
        return;
    };
    let Ok(reports) = ctx_core::impact::analyze_impact(&handler, graph) else {
        return;
    };
    let Some(report) = reports.into_iter().next() else {
        return;
    };
    let features = report
        .features
        .iter()
        .map(|node| node.identifier.clone())
        .collect::<Vec<_>>();
    let requirements = report
        .requirements
        .iter()
        .map(|node| node.identifier.clone())
        .collect::<Vec<_>>();
    if features.is_empty() && requirements.is_empty() {
        return;
    }
    trace.product_context = Some(ctx_core::trace::ProductContext {
        features,
        requirements,
    });
}

/// `resolve_endpoint_seeds` only ever looks at this repository's own graph,
/// so a target naming an endpoint this repository merely *calls* (typically
/// copy-pasted from `ctx federation show`'s output, which lists a
/// *neighbor's* endpoints) is honestly "not found" rather than silently
/// jumping repositories. When that's exactly what happened, point at the
/// local handler(s) that already reach it instead of leaving a bare error.
fn hint_not_found(
    error: ctx_core::trace::TraceError,
    target: &str,
    graph: &ctx_core::graph::GraphSnapshot,
) -> CliError {
    let Some((method, path)) = parse_method_path(target) else {
        return error.into();
    };
    let Some(normalized) = path_template(&path) else {
        return error.into();
    };
    let handlers = external_call_contracts(graph)
        .into_iter()
        .filter(|call| {
            call.method == method
                && path_template(&call.url).as_deref() == Some(normalized.as_str())
        })
        .map(|call| call.handler)
        .collect::<BTreeSet<_>>();
    let Some(first_handler) = handlers.iter().next().cloned() else {
        return error.into();
    };
    CliError::TraceTargetBelongsToCaller {
        target: target.to_owned(),
        handlers: handlers.into_iter().collect::<Vec<_>>().join(", "),
        first_handler,
    }
}

pub(super) fn print_endpoint_trace(trace: &EndpointTrace, indent: usize) {
    let pad = "  ".repeat(indent);
    let service = if trace.service.is_empty() {
        "(local)"
    } else {
        trace.service.as_str()
    };
    match &trace.handler {
        Some(handler) => println!(
            "{pad}{service} {} {} -> {handler}",
            trace.method.as_str(),
            trace.path
        ),
        None => println!("{pad}{service} {} {}", trace.method.as_str(), trace.path),
    }
    if let Some(context) = &trace.product_context {
        if !context.features.is_empty() {
            println!("{pad}  features: {}", context.features.join(", "));
        }
        if !context.requirements.is_empty() {
            println!("{pad}  requirements: {}", context.requirements.join(", "));
        }
    }
    if !trace.reads.is_empty() {
        println!("{pad}  reads: {}", trace.reads.join(", "));
    }
    if !trace.writes.is_empty() {
        println!("{pad}  writes: {}", trace.writes.join(", "));
    }
    for call in &trace.calls {
        println!("{pad}  calls: {} {}", call.method.as_str(), call.url);
        match &call.resolution {
            CallResolution::Crosses(subtree) => print_endpoint_trace(subtree, indent + 2),
            CallResolution::Unresolved(reason) => {
                println!("{pad}    -> {}", describe_terminal(reason));
            }
        }
    }
    if let Some(reason) = &trace.stopped {
        println!("{pad}  (stopped: {})", describe_terminal(reason));
    }
}

fn describe_terminal(reason: &TerminalReason) -> String {
    match reason {
        TerminalReason::NoNeighborMatch => {
            "no synchronized neighbor exposes a matching endpoint; no context available past this call"
                .to_owned()
        }
        TerminalReason::NeighborStale { service } => format!(
            "neighbor '{service}' is stale (run `ctx sync`); stopping rather than tracing possibly-outdated structure"
        ),
        TerminalReason::NeighborUnavailable { service } => {
            format!("neighbor '{service}' has no usable synchronized snapshot")
        }
        TerminalReason::RetiredFact => {
            "this fact is no longer active (code changed since it was indexed)".to_owned()
        }
        TerminalReason::Cycle => "already visited earlier in this trace (cycle)".to_owned(),
        TerminalReason::ServiceTransitionCapReached => format!(
            "reached the {}-service-transition limit",
            ctx_core::trace::MAX_SERVICE_TRANSITIONS
        ),
        TerminalReason::NodeCapReached => {
            format!("reached the {}-node limit", ctx_core::trace::MAX_NODES)
        }
        TerminalReason::BranchCapReached => {
            format!("reached the {}-branch limit", ctx_core::trace::MAX_BRANCHES)
        }
    }
}

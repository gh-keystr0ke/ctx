//! Cross-repository request tracing (`ADR-FEDERATION-003`).
//!
//! `trace_endpoint` walks only commit-labelled deterministic structure: an
//! `ApiEndpoint` node's `Exposes` handler, that handler's `ReadsFrom`/
//! `WritesTo` data entities and `CallsExternal` outbound contracts, and (via
//! the caller-supplied [`FederationResolver`]) a `FEDERATED_MATCH` into a
//! synchronized neighbor's own endpoint. `ctx-core` never knows about
//! federation storage or subprocesses -- [`FederationResolver`] is the one
//! boundary a concrete resolver (in `ctx-cli`) implements, so this module's
//! traversal and bounds stay unit-testable with a fake resolver.
//!
//! Product-semantic assertions (Features/Requirements/Invariants/Decisions)
//! are never part of this traversal -- only the structural fact sequence the
//! ADR names. [`EndpointTrace::product_context`] exists so a caller can
//! attach that semantic layer afterward, purely for display -- `trace_endpoint`
//! itself never populates it.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    domain::{ClaimStatus, NodeKind, RelationKind},
    graph::GraphSnapshot,
    indexing::PlannedNodeAttributes,
    ir::HttpMethod,
};

/// Hard ceiling on cross-repository transitions in one trace (`ADR-FEDERATION-003`).
pub const MAX_SERVICE_TRANSITIONS: usize = 8;
/// Hard ceiling on total structural nodes in one trace's complete result.
pub const MAX_NODES: usize = 50;
/// Hard ceiling on total branches (outbound calls examined) in one trace.
pub const MAX_BRANCHES: usize = 16;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum TraceError {
    #[error("no indexed HTTP endpoint matches '{0}'")]
    NotFound(String),
}

/// Identifies one endpoint visit for cycle detection: revisiting the same
/// key ends the branch as [`TerminalReason::Cycle`] instead of recursing.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct VisitedKey {
    pub service: String,
    pub source_commit: String,
    pub method: HttpMethod,
    pub path: String,
}

/// An outbound call's structural facts, handed to a [`FederationResolver`]
/// with no normalization applied -- normalizing the URL into a path template
/// comparable to a neighbor's endpoint paths is the resolver's job, since
/// only the resolver (not `ctx-core`) knows about neighbor manifests.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LocalCall {
    pub method: HttpMethod,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TerminalReason {
    /// No registered/synchronized neighbor exposes an endpoint matching this
    /// call's method and normalized path.
    NoNeighborMatch,
    /// A neighbor endpoint matched, but that neighbor's own checkout has
    /// moved past the commit last synchronized -- the synced snapshot could
    /// misdescribe its current handler, so the branch stops rather than
    /// tracing possibly-stale structure.
    NeighborStale { service: String },
    /// A neighbor endpoint matched, but its synchronized snapshot could not
    /// be read (never synced, incompatible schema version, or the neighbor
    /// process failed).
    NeighborUnavailable { service: String },
    /// The endpoint's `Exposes` edge (or a `ReadsFrom`/`WritesTo`/
    /// `CallsExternal` edge reached while expanding it) is not
    /// [`ClaimStatus::Active`] -- the fact existed at some point but no
    /// longer reflects current code.
    RetiredFact,
    /// Revisiting a [`VisitedKey`] already on this trace's path.
    Cycle,
    /// The trace has already used its full `MAX_SERVICE_TRANSITIONS` budget.
    ServiceTransitionCapReached,
    /// The trace has already used its full `MAX_NODES` budget.
    NodeCapReached,
    /// The trace has already used its full `MAX_BRANCHES` budget.
    BranchCapReached,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallTrace {
    pub method: HttpMethod,
    pub url: String,
    pub resolution: CallResolution,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CallResolution {
    /// The call resolved to a neighbor endpoint and that endpoint's own
    /// sequence was traced (bounded by whatever budget remained).
    Crosses(Box<EndpointTrace>),
    Unresolved(TerminalReason),
}

/// One endpoint's traced sequence: its handler, that handler's data
/// entities, and its outbound calls. `stopped` is set when this endpoint's
/// own expansion was cut short -- `reads`/`writes`/`calls` still hold
/// whatever was gathered before the cut, never presented as if complete.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EndpointTrace {
    pub service: String,
    pub source_commit: String,
    pub method: HttpMethod,
    pub path: String,
    pub handler: Option<String>,
    pub reads: Vec<String>,
    pub writes: Vec<String>,
    pub calls: Vec<CallTrace>,
    pub stopped: Option<TerminalReason>,
    /// Features/Requirements mapped to this endpoint's handler. Never
    /// populated by [`trace_endpoint`] itself -- product-semantic assertions
    /// are deliberately outside this traversal's structural sequence. A
    /// caller (`ctx-cli`, gated by `--verbose`) may attach this afterward as
    /// a display-only annotation, using its own graph for whichever service
    /// owns this node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_context: Option<ProductContext>,
}

/// Product-semantic context optionally attached to one [`EndpointTrace`]
/// node after the fact -- see `product_context`'s doc comment.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProductContext {
    pub features: Vec<String>,
    pub requirements: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TraceBudget {
    pub nodes: usize,
    pub branches: usize,
    pub service_transitions: usize,
}

impl TraceBudget {
    #[must_use]
    pub const fn root() -> Self {
        Self {
            nodes: MAX_NODES,
            branches: MAX_BRANCHES,
            service_transitions: MAX_SERVICE_TRANSITIONS,
        }
    }
}

/// The one boundary `ctx-core` knows about federation through: given an
/// outbound call and the budget/visited set left to spend on it, either a
/// fully-traced neighbor subtree (bounded by `budget`) or the reason it
/// can't continue. A concrete resolver (in `ctx-cli`) matches `call` against
/// synchronized neighbor manifests and, on a match, continues the trace --
/// typically by invoking that neighbor's own `ctx` binary, since only that
/// neighbor's own process can safely decide what of its graph is traceable.
pub trait FederationResolver {
    fn resolve(
        &mut self,
        call: &LocalCall,
        budget: TraceBudget,
        visited: &BTreeSet<VisitedKey>,
    ) -> CallResolution;
}

/// A resolver for a repository with no registered neighbors (or when
/// federation is out of scope): every outbound call is honestly reported as
/// unresolved rather than guessed.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoFederation;

impl FederationResolver for NoFederation {
    fn resolve(
        &mut self,
        _call: &LocalCall,
        _budget: TraceBudget,
        _visited: &BTreeSet<VisitedKey>,
    ) -> CallResolution {
        CallResolution::Unresolved(TerminalReason::NoNeighborMatch)
    }
}

/// Resolves `query` to every distinct `ApiEndpoint` node it names -- either
/// directly, or via a `CodeSymbol` that `Exposes` one or more endpoints.
/// Accepts a literal `"METHOD /path"` selector (case-insensitive method) for
/// an unambiguous seed alongside the same fuzzy name/suffix resolution
/// `ctx impact`/`ctx explain` already use.
///
/// # Errors
///
/// Returns [`TraceError::NotFound`] when nothing matches, or matches only
/// nodes that expose no HTTP endpoint at all.
pub fn resolve_endpoint_seeds<'a>(
    query: &str,
    graph: &'a GraphSnapshot,
) -> Result<Vec<&'a crate::graph::GraphNode>, TraceError> {
    if let Some((method, path)) = parse_method_path(query) {
        let mut matches = graph
            .nodes
            .values()
            .filter(|node| {
                node.kind == NodeKind::ApiEndpoint
                    && matches!(
                        &node.attributes,
                        PlannedNodeAttributes::ApiEndpoint { endpoint }
                            if endpoint.method == method && endpoint.path == path
                    )
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|node| node.stable_key.clone());
        if matches.is_empty() {
            return Err(TraceError::NotFound(query.to_owned()));
        }
        return Ok(matches);
    }

    let resolved = graph.resolve(query);
    if resolved.is_empty() {
        return Err(TraceError::NotFound(query.to_owned()));
    }
    let mut endpoints = BTreeSet::new();
    for node in &resolved {
        match node.kind {
            NodeKind::ApiEndpoint => {
                endpoints.insert(node.stable_key.clone());
            }
            NodeKind::CodeSymbol => {
                for edge in &graph.edges {
                    if edge.source == node.stable_key && edge.kind == RelationKind::Exposes {
                        endpoints.insert(edge.target.clone());
                    }
                }
            }
            _ => {}
        }
    }
    let mut matches = endpoints
        .iter()
        .filter_map(|key| graph.nodes.get(key))
        .collect::<Vec<_>>();
    matches.sort_by_key(|node| node.stable_key.clone());
    if matches.is_empty() {
        return Err(TraceError::NotFound(query.to_owned()));
    }
    Ok(matches)
}

/// Parses a literal `"METHOD /path"` endpoint selector (case-insensitive
/// method), the same selector [`resolve_endpoint_seeds`] accepts.
#[must_use]
pub fn parse_method_path(query: &str) -> Option<(HttpMethod, String)> {
    let (method, path) = query.trim().split_once(char::is_whitespace)?;
    let method = match method.to_ascii_uppercase().as_str() {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        "PUT" => HttpMethod::Put,
        "DELETE" => HttpMethod::Delete,
        "PATCH" => HttpMethod::Patch,
        "HEAD" => HttpMethod::Head,
        "OPTIONS" => HttpMethod::Options,
        "TRACE" => HttpMethod::Trace,
        _ => return None,
    };
    let path = path.trim();
    path.starts_with('/').then(|| (method, path.to_owned()))
}

/// Traces one endpoint node's sequence, recursing across service boundaries
/// through `resolver`. `service`/`source_commit` label the repository this
/// `graph` was loaded from (empty service name means "this repository" when
/// the caller has no configured federation identity).
#[allow(clippy::too_many_arguments)]
pub fn trace_endpoint(
    endpoint: &crate::graph::GraphNode,
    graph: &GraphSnapshot,
    service: &str,
    source_commit: &str,
    budget: &mut TraceBudget,
    visited: &mut BTreeSet<VisitedKey>,
    resolver: &mut impl FederationResolver,
) -> EndpointTrace {
    let PlannedNodeAttributes::ApiEndpoint { endpoint: contract } = &endpoint.attributes else {
        unreachable!("resolve_endpoint_seeds only returns ApiEndpoint nodes")
    };
    let key = VisitedKey {
        service: service.to_owned(),
        source_commit: source_commit.to_owned(),
        method: contract.method,
        path: contract.path.clone(),
    };
    let mut trace = EndpointTrace {
        service: service.to_owned(),
        source_commit: source_commit.to_owned(),
        method: contract.method,
        path: contract.path.clone(),
        handler: None,
        reads: Vec::new(),
        writes: Vec::new(),
        calls: Vec::new(),
        stopped: None,
        product_context: None,
    };
    if !visited.insert(key) {
        trace.stopped = Some(TerminalReason::Cycle);
        return trace;
    }
    if !take_node(budget) {
        trace.stopped = Some(TerminalReason::NodeCapReached);
        return trace;
    }

    let Some(handler) = active_handler(graph, &endpoint.stable_key) else {
        trace.stopped = Some(TerminalReason::RetiredFact);
        return trace;
    };
    if !take_node(budget) {
        trace.stopped = Some(TerminalReason::NodeCapReached);
        return trace;
    }
    trace.handler = Some(handler.identifier().to_owned());

    if !collect_entities(
        graph,
        &handler.stable_key,
        RelationKind::ReadsFrom,
        budget,
        &mut trace.reads,
    ) {
        trace.stopped = Some(TerminalReason::NodeCapReached);
        return trace;
    }
    if !collect_entities(
        graph,
        &handler.stable_key,
        RelationKind::WritesTo,
        budget,
        &mut trace.writes,
    ) {
        trace.stopped = Some(TerminalReason::NodeCapReached);
        return trace;
    }

    trace.calls = trace_calls(graph, &handler.stable_key, budget, visited, resolver);
    trace
}

fn active_handler<'a>(
    graph: &'a GraphSnapshot,
    endpoint_key: &crate::domain::StableKey,
) -> Option<&'a crate::graph::GraphNode> {
    let handler_edge = graph
        .edges
        .iter()
        .filter(|edge| edge.target == *endpoint_key && edge.kind == RelationKind::Exposes)
        .min_by(|left, right| left.source.cmp(&right.source))?;
    if handler_edge.status != ClaimStatus::Active {
        return None;
    }
    graph.nodes.get(&handler_edge.source)
}

/// Appends every active `kind`-edge target's identifier reachable from
/// `source` into `into`, spending one node budget unit per entity. Returns
/// `false` (leaving `into` partially filled) the moment the budget runs out.
fn collect_entities(
    graph: &GraphSnapshot,
    source: &crate::domain::StableKey,
    kind: RelationKind,
    budget: &mut TraceBudget,
    into: &mut Vec<String>,
) -> bool {
    for edge in sorted_outgoing(graph, source, kind) {
        if edge.status != ClaimStatus::Active {
            continue;
        }
        let Some(entity) = graph.nodes.get(&edge.target) else {
            continue;
        };
        if !take_node(budget) {
            return false;
        }
        into.push(entity.identifier().to_owned());
    }
    true
}

fn trace_calls(
    graph: &GraphSnapshot,
    handler_key: &crate::domain::StableKey,
    budget: &mut TraceBudget,
    visited: &mut BTreeSet<VisitedKey>,
    resolver: &mut impl FederationResolver,
) -> Vec<CallTrace> {
    let mut calls = Vec::new();
    for edge in sorted_outgoing(graph, handler_key, RelationKind::CallsExternal) {
        if edge.status != ClaimStatus::Active {
            continue;
        }
        let Some(call_node) = graph.nodes.get(&edge.target) else {
            continue;
        };
        let PlannedNodeAttributes::ExternalCall { call } = &call_node.attributes else {
            continue;
        };
        let local_call = LocalCall {
            method: call.method,
            url: call.url.clone(),
        };
        calls.push(trace_one_call(local_call, budget, visited, resolver));
    }
    calls
}

fn trace_one_call(
    local_call: LocalCall,
    budget: &mut TraceBudget,
    visited: &mut BTreeSet<VisitedKey>,
    resolver: &mut impl FederationResolver,
) -> CallTrace {
    if budget.branches == 0 {
        return CallTrace {
            method: local_call.method,
            url: local_call.url,
            resolution: CallResolution::Unresolved(TerminalReason::BranchCapReached),
        };
    }
    budget.branches -= 1;
    if budget.service_transitions == 0 {
        return CallTrace {
            method: local_call.method,
            url: local_call.url,
            resolution: CallResolution::Unresolved(TerminalReason::ServiceTransitionCapReached),
        };
    }
    let resolution = resolver.resolve(&local_call, *budget, visited);
    if let CallResolution::Crosses(ref subtree) = resolution {
        budget.nodes = budget.nodes.saturating_sub(count_nodes(subtree));
        budget.service_transitions = budget
            .service_transitions
            .saturating_sub(count_service_transitions(subtree));
        visited.extend(collect_visited(subtree));
    }
    CallTrace {
        method: local_call.method,
        url: local_call.url,
        resolution,
    }
}

fn take_node(budget: &mut TraceBudget) -> bool {
    if budget.nodes == 0 {
        return false;
    }
    budget.nodes -= 1;
    true
}

fn sorted_outgoing<'a>(
    graph: &'a GraphSnapshot,
    source: &crate::domain::StableKey,
    kind: RelationKind,
) -> Vec<&'a crate::graph::GraphEdge> {
    let mut edges = graph
        .edges
        .iter()
        .filter(|edge| &edge.source == source && edge.kind == kind)
        .collect::<Vec<_>>();
    edges.sort_by(|left, right| left.target.cmp(&right.target));
    edges
}

/// Total structural nodes counted in `trace` and everything it crosses into:
/// the endpoint itself, its handler, each read/write entity, and each
/// outbound call examined (crossed or not).
#[must_use]
pub fn count_nodes(trace: &EndpointTrace) -> usize {
    let mut total = 1; // the endpoint node itself
    if trace.handler.is_some() {
        total += 1;
    }
    total += trace.reads.len() + trace.writes.len();
    for call in &trace.calls {
        total += 1; // the ExternalCall node itself
        if let CallResolution::Crosses(subtree) = &call.resolution {
            total += count_nodes(subtree);
        }
    }
    total
}

/// Total branches (outbound calls examined, crossed or not) in `trace` and
/// everything it crosses into.
#[must_use]
pub fn count_branches(trace: &EndpointTrace) -> usize {
    let mut total = trace.calls.len();
    for call in &trace.calls {
        if let CallResolution::Crosses(subtree) = &call.resolution {
            total += count_branches(subtree);
        }
    }
    total
}

/// Total cross-repository transitions actually made: one per `Crosses`
/// resolution, plus however many happened further inside that subtree.
#[must_use]
pub fn count_service_transitions(trace: &EndpointTrace) -> usize {
    trace
        .calls
        .iter()
        .map(|call| match &call.resolution {
            CallResolution::Crosses(subtree) => 1 + count_service_transitions(subtree),
            CallResolution::Unresolved(_) => 0,
        })
        .sum()
}

fn collect_visited(trace: &EndpointTrace) -> BTreeSet<VisitedKey> {
    let mut keys = BTreeSet::new();
    keys.insert(VisitedKey {
        service: trace.service.clone(),
        source_commit: trace.source_commit.clone(),
        method: trace.method,
        path: trace.path.clone(),
    });
    for call in &trace.calls {
        if let CallResolution::Crosses(subtree) = &call.resolution {
            keys.extend(collect_visited(subtree));
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        CallResolution, FederationResolver, LocalCall, MAX_BRANCHES, MAX_NODES,
        MAX_SERVICE_TRANSITIONS, NoFederation, TerminalReason, TraceBudget, VisitedKey,
        parse_method_path, resolve_endpoint_seeds, trace_endpoint,
    };
    use crate::{
        domain::{
            ClaimClass, ClaimStatus, Confidence, NodeKind, RelationKind, SourceKind, StableKey,
        },
        graph::{GraphEdge, GraphNode, GraphSnapshot},
        indexing::PlannedNodeAttributes,
        ir::{ApiEndpoint, ExternalCall, HttpMethod, SourceRange},
    };

    fn symbol_node(key: &str, canonical: &str) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("stable key"),
            kind: NodeKind::CodeSymbol,
            name: canonical.to_owned(),
            content_hash: "hash".to_owned(),
            attributes: PlannedNodeAttributes::Symbol {
                file_path: "main.py".to_owned(),
                canonical_path: canonical.to_owned(),
                symbol_kind: crate::ir::SymbolKind::Function,
                range: SourceRange::default(),
                signature: None,
                structural_fingerprint: "shape".to_owned(),
                calls: Vec::new(),
                database_accesses: Vec::new(),
                orm_accesses: Vec::new(),
                schema_tables: Vec::new(),
                api_endpoints: Vec::new(),
                external_calls: Vec::new(),
            },
        }
    }

    fn db_entity_node(key: &str, identifier: &str) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("stable key"),
            kind: NodeKind::DbEntity,
            name: identifier.to_owned(),
            content_hash: identifier.to_owned(),
            attributes: PlannedNodeAttributes::Interaction {
                identifier: identifier.to_owned(),
            },
        }
    }

    fn endpoint_node(key: &str, method: HttpMethod, path: &str) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("stable key"),
            kind: NodeKind::ApiEndpoint,
            name: format!("{} {path}", method.as_str()),
            content_hash: "endpoint".to_owned(),
            attributes: PlannedNodeAttributes::ApiEndpoint {
                endpoint: ApiEndpoint {
                    path: path.to_owned(),
                    method,
                    params: Vec::new(),
                    return_type: None,
                    framework: "python_http_framework".to_owned(),
                    range: SourceRange::default(),
                    openapi: None,
                },
            },
        }
    }

    fn external_call_node(key: &str, method: HttpMethod, url: &str) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("stable key"),
            kind: NodeKind::ExternalSystem,
            name: url.to_owned(),
            content_hash: url.to_owned(),
            attributes: PlannedNodeAttributes::ExternalCall {
                call: ExternalCall {
                    method,
                    url: url.to_owned(),
                    range: SourceRange::default(),
                },
            },
        }
    }

    fn edge(
        source: &GraphNode,
        target: &GraphNode,
        kind: RelationKind,
        status: ClaimStatus,
    ) -> GraphEdge {
        GraphEdge {
            source: source.stable_key.clone(),
            target: target.stable_key.clone(),
            kind,
            claim_class: ClaimClass::Fact,
            source_kind: SourceKind::StaticAnalysis,
            confidence: Confidence::CERTAIN,
            status,
            valid_from: "commit".to_owned(),
            valid_to: None,
            producer: "test".to_owned(),
            fingerprint: format!("{}:{kind:?}:{}", source.stable_key, target.stable_key),
            stale_reason: (status != ClaimStatus::Active).then(|| "changed".to_owned()),
            evidence: Vec::new(),
        }
    }

    fn snapshot(nodes: Vec<GraphNode>, edges: Vec<GraphEdge>) -> GraphSnapshot {
        GraphSnapshot {
            nodes: nodes
                .into_iter()
                .map(|node| (node.stable_key.clone(), node))
                .collect::<BTreeMap<_, _>>(),
            edges,
        }
    }

    #[test]
    fn endpoint_selectors_cover_every_openapi_http_method() {
        for (literal, expected) in [
            ("HEAD /health", HttpMethod::Head),
            ("OPTIONS /items", HttpMethod::Options),
            ("TRACE /debug", HttpMethod::Trace),
        ] {
            assert_eq!(
                parse_method_path(literal),
                Some((
                    expected,
                    literal.split_once(' ').expect("method path").1.to_owned()
                ))
            );
        }
    }

    /// `POST /pay` (`main.pay`) reads `db:accounts`, writes `db:ledger`, and
    /// calls out to `https://fraud-checker.internal/check`.
    fn billing_fixture() -> GraphSnapshot {
        let endpoint = endpoint_node("endpoint:pay", HttpMethod::Post, "/pay");
        let handler = symbol_node("sym:pay", "main.pay");
        let accounts = db_entity_node("db:accounts", "accounts");
        let ledger = db_entity_node("db:ledger", "ledger");
        let call = external_call_node(
            "call:fraud",
            HttpMethod::Post,
            "https://fraud-checker.internal/check",
        );
        let edges = vec![
            edge(
                &handler,
                &endpoint,
                RelationKind::Exposes,
                ClaimStatus::Active,
            ),
            edge(
                &handler,
                &accounts,
                RelationKind::ReadsFrom,
                ClaimStatus::Active,
            ),
            edge(
                &handler,
                &ledger,
                RelationKind::WritesTo,
                ClaimStatus::Active,
            ),
            edge(
                &handler,
                &call,
                RelationKind::CallsExternal,
                ClaimStatus::Active,
            ),
        ];
        snapshot(vec![endpoint, handler, accounts, ledger, call], edges)
    }

    #[test]
    fn traces_reads_writes_and_reports_an_unmatched_outbound_call_honestly() {
        let graph = billing_fixture();
        let seeds = resolve_endpoint_seeds("POST /pay", &graph).expect("seed");
        let mut budget = TraceBudget::root();
        let mut visited = std::collections::BTreeSet::new();
        let trace = trace_endpoint(
            seeds[0],
            &graph,
            "billing",
            "abc123",
            &mut budget,
            &mut visited,
            &mut NoFederation,
        );

        assert_eq!(trace.handler.as_deref(), Some("main.pay"));
        assert_eq!(trace.reads, vec!["accounts".to_owned()]);
        assert_eq!(trace.writes, vec!["ledger".to_owned()]);
        assert_eq!(trace.calls.len(), 1);
        assert_eq!(
            trace.calls[0].resolution,
            CallResolution::Unresolved(TerminalReason::NoNeighborMatch)
        );
        assert!(trace.stopped.is_none());
        assert_eq!(trace.product_context, None);
    }

    #[test]
    fn resolves_a_handler_name_seed_to_the_endpoint_it_exposes() {
        let graph = billing_fixture();
        let seeds = resolve_endpoint_seeds("main.pay", &graph).expect("seed");
        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds[0].stable_key.as_str(), "endpoint:pay");
    }

    #[test]
    fn a_query_matching_nothing_at_all_is_not_found() {
        let graph = billing_fixture();
        assert!(resolve_endpoint_seeds("nothing.here", &graph).is_err());
    }

    #[test]
    fn a_symbol_matched_by_name_that_exposes_no_endpoint_is_not_found() {
        let mut graph = billing_fixture();
        let other = symbol_node("sym:other", "main.unrelated");
        graph.nodes.insert(other.stable_key.clone(), other);
        assert!(resolve_endpoint_seeds("main.unrelated", &graph).is_err());
    }

    #[test]
    fn a_stale_exposes_edge_stops_the_branch_as_a_retired_fact() {
        let endpoint = endpoint_node("endpoint:pay", HttpMethod::Post, "/pay");
        let handler = symbol_node("sym:pay", "main.pay");
        let edges = vec![edge(
            &handler,
            &endpoint,
            RelationKind::Exposes,
            ClaimStatus::Stale,
        )];
        let graph = snapshot(vec![endpoint, handler], edges);
        let seeds = resolve_endpoint_seeds("POST /pay", &graph).expect("seed");
        let mut budget = TraceBudget::root();
        let mut visited = std::collections::BTreeSet::new();
        let trace = trace_endpoint(
            seeds[0],
            &graph,
            "billing",
            "abc123",
            &mut budget,
            &mut visited,
            &mut NoFederation,
        );
        assert_eq!(trace.handler, None);
        assert_eq!(trace.stopped, Some(TerminalReason::RetiredFact));
    }

    /// A resolver simulating a neighbor whose own `ctx trace` process would
    /// answer the request: it runs the exact same [`trace_endpoint`] against
    /// a second in-memory graph, using the visited set and budget it was
    /// handed -- exercising the same cross-process protocol shape `ctx-cli`
    /// uses over a subprocess, without a subprocess.
    struct FakeNeighbor {
        service: String,
        commit: String,
        graph: GraphSnapshot,
    }

    impl FederationResolver for FakeNeighbor {
        fn resolve(
            &mut self,
            call: &LocalCall,
            budget: TraceBudget,
            visited: &std::collections::BTreeSet<VisitedKey>,
        ) -> CallResolution {
            let Some(path) = path_for(&call.url) else {
                return CallResolution::Unresolved(TerminalReason::NoNeighborMatch);
            };
            let Ok(seeds) =
                resolve_endpoint_seeds(&format!("{} {path}", call.method.as_str()), &self.graph)
            else {
                return CallResolution::Unresolved(TerminalReason::NoNeighborMatch);
            };
            let mut nested_budget = budget;
            let mut nested_visited = visited.clone();
            let subtree = trace_endpoint(
                seeds[0],
                &self.graph,
                &self.service,
                &self.commit,
                &mut nested_budget,
                &mut nested_visited,
                &mut NoFederation,
            );
            CallResolution::Crosses(Box::new(subtree))
        }
    }

    fn path_for(url: &str) -> Option<&str> {
        url.strip_prefix("https://fraud-checker.internal")
    }

    fn fraud_checker_fixture() -> GraphSnapshot {
        let endpoint = endpoint_node("endpoint:check", HttpMethod::Post, "/check");
        let handler = symbol_node("sym:check", "main.check_fraud");
        let edges = vec![edge(
            &handler,
            &endpoint,
            RelationKind::Exposes,
            ClaimStatus::Active,
        )];
        snapshot(vec![endpoint, handler], edges)
    }

    #[test]
    fn crosses_into_a_neighbor_and_traces_its_own_handler() {
        let graph = billing_fixture();
        let seeds = resolve_endpoint_seeds("POST /pay", &graph).expect("seed");
        let mut budget = TraceBudget::root();
        let mut visited = std::collections::BTreeSet::new();
        let mut resolver = FakeNeighbor {
            service: "fraud-checker".to_owned(),
            commit: "def456".to_owned(),
            graph: fraud_checker_fixture(),
        };
        let trace = trace_endpoint(
            seeds[0],
            &graph,
            "billing",
            "abc123",
            &mut budget,
            &mut visited,
            &mut resolver,
        );

        let CallResolution::Crosses(subtree) = &trace.calls[0].resolution else {
            panic!("expected the call to cross into the neighbor");
        };
        assert_eq!(subtree.service, "fraud-checker");
        assert_eq!(subtree.handler.as_deref(), Some("main.check_fraud"));
        assert!(subtree.calls.is_empty());
    }

    #[test]
    fn a_cycle_back_to_an_already_visited_endpoint_stops_without_recursing() {
        let graph = billing_fixture();
        let seeds = resolve_endpoint_seeds("POST /pay", &graph).expect("seed");
        let mut budget = TraceBudget::root();
        let mut visited = std::collections::BTreeSet::from([VisitedKey {
            service: "fraud-checker".to_owned(),
            source_commit: "def456".to_owned(),
            method: HttpMethod::Post,
            path: "/check".to_owned(),
        }]);
        let mut resolver = FakeNeighbor {
            service: "fraud-checker".to_owned(),
            commit: "def456".to_owned(),
            graph: fraud_checker_fixture(),
        };
        let trace = trace_endpoint(
            seeds[0],
            &graph,
            "billing",
            "abc123",
            &mut budget,
            &mut visited,
            &mut resolver,
        );

        let CallResolution::Crosses(subtree) = &trace.calls[0].resolution else {
            panic!("expected the neighbor to still answer, reporting its own cycle");
        };
        assert_eq!(subtree.stopped, Some(TerminalReason::Cycle));
    }

    #[test]
    fn the_node_cap_stops_a_branch_once_exhausted() {
        let graph = billing_fixture();
        let seeds = resolve_endpoint_seeds("POST /pay", &graph).expect("seed");
        // endpoint(1) + handler(1) leaves nothing for the single read entity.
        let mut budget = TraceBudget {
            nodes: 2,
            branches: MAX_BRANCHES,
            service_transitions: MAX_SERVICE_TRANSITIONS,
        };
        let mut visited = std::collections::BTreeSet::new();
        let trace = trace_endpoint(
            seeds[0],
            &graph,
            "billing",
            "abc123",
            &mut budget,
            &mut visited,
            &mut NoFederation,
        );
        assert_eq!(trace.handler.as_deref(), Some("main.pay"));
        assert!(trace.reads.is_empty());
        assert_eq!(trace.stopped, Some(TerminalReason::NodeCapReached));
    }

    #[test]
    fn the_branch_cap_reports_further_calls_as_capped_rather_than_dropping_them() {
        let mut graph = billing_fixture();
        let handler = graph
            .nodes
            .get(&StableKey::new("sym:pay").expect("key"))
            .expect("handler")
            .clone();
        let second_call =
            external_call_node("call:second", HttpMethod::Get, "https://audit.internal/log");
        graph.edges.push(edge(
            &handler,
            &second_call,
            RelationKind::CallsExternal,
            ClaimStatus::Active,
        ));
        graph
            .nodes
            .insert(second_call.stable_key.clone(), second_call);

        let seeds = resolve_endpoint_seeds("POST /pay", &graph).expect("seed");
        let mut budget = TraceBudget {
            nodes: MAX_NODES,
            branches: 1,
            service_transitions: MAX_SERVICE_TRANSITIONS,
        };
        let mut visited = std::collections::BTreeSet::new();
        let trace = trace_endpoint(
            seeds[0],
            &graph,
            "billing",
            "abc123",
            &mut budget,
            &mut visited,
            &mut NoFederation,
        );
        assert_eq!(trace.calls.len(), 2);
        assert!(
            trace.calls.iter().any(|call| call.resolution
                == CallResolution::Unresolved(TerminalReason::BranchCapReached))
        );
    }
}

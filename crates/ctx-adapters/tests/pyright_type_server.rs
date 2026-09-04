use std::{env, fs, path::PathBuf, time::Duration};

use ctx_adapters::{pyright::PyrightTypeServer, python::PythonAnalyzer};
use ctx_app::ports::PythonTypeOracle;
use ctx_core::type_inference::{PythonType, TypeWriteCandidate};

#[test]
#[ignore = "requires CTX_PYRIGHT_TYPESERVER pointing to a real Pyright Type Server"]
fn real_type_server_resolves_tier_one_write_sites() {
    let executable = env::var_os("CTX_PYRIGHT_TYPESERVER")
        .map(PathBuf::from)
        .expect("set CTX_PYRIGHT_TYPESERVER to the pyright-typeserver executable");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pyright_tier1")
        .canonicalize()
        .expect("canonical fixture root");
    let app = root.join("app.py");
    let source = fs::read_to_string(&app).expect("read fixture");
    let candidates =
        PythonAnalyzer::type_write_candidates("app.py", &source).expect("extract write candidates");
    let mut oracle = PyrightTypeServer::start(&executable, &root, Duration::from_secs(60))
        .expect("start real Pyright Type Server");

    assert_eq!(candidates.len(), 17);
    assert_model_probe(&mut oracle, &app, &candidates, "row");
    assert_model_probe(&mut oracle, &app, &candidates, "fetched");
    assert_model_probe(&mut oracle, &app, &candidates, "selected");
    assert_model_probe(&mut oracle, &app, &candidates, "annotated");
    assert!(matches!(
        inferred_probe(&mut oracle, &app, &candidates, "dynamic"),
        PythonType::Any
    ));
    assert!(matches!(
        inferred_probe(&mut oracle, &app, &candidates, "fetched_optional"),
        PythonType::Union { .. }
    ));

    assert_session_method(
        &mut oracle,
        &app,
        &candidates,
        "session.add",
        "/sqlalchemy/orm/session.py",
    );
    assert_session_method(
        &mut oracle,
        &app,
        &candidates,
        "async_session.add",
        "/sqlalchemy/ext/asyncio/session.py",
    );
    let collection = inferred_method(&mut oracle, &app, &candidates, "collection.add");
    let PythonType::Function(collection) = collection else {
        panic!("set.add did not resolve to a function identity");
    };
    assert!(!collection.declaration.uri.contains("sqlalchemy"));
    oracle.shutdown().expect("shutdown Type Server");
}

fn inferred_probe(
    oracle: &mut PyrightTypeServer,
    app: &std::path::Path,
    candidates: &[TypeWriteCandidate],
    expression: &str,
) -> PythonType {
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.probe.expression == expression)
        .unwrap_or_else(|| panic!("missing candidate probe {expression}"));
    oracle
        .inferred_type(app, &candidate.probe)
        .unwrap_or_else(|error| panic!("resolve {expression}: {error}"))
}

fn inferred_method(
    oracle: &mut PyrightTypeServer,
    app: &std::path::Path,
    candidates: &[TypeWriteCandidate],
    expression: &str,
) -> PythonType {
    let candidate = candidates
        .iter()
        .find(|candidate| {
            candidate
                .method_probe
                .as_ref()
                .is_some_and(|probe| probe.expression == expression)
        })
        .unwrap_or_else(|| panic!("missing method probe {expression}"));
    oracle
        .inferred_type(app, candidate.method_probe.as_ref().expect("method probe"))
        .unwrap_or_else(|error| panic!("resolve {expression}: {error}"))
}

fn assert_model_probe(
    oracle: &mut PyrightTypeServer,
    app: &std::path::Path,
    candidates: &[TypeWriteCandidate],
    expression: &str,
) {
    let inferred = inferred_probe(oracle, app, candidates, expression);
    let PythonType::Class(model) = inferred else {
        panic!("{expression} resolved to {}", inferred.diagnostic_name());
    };
    assert!(model.is_instance);
    assert_eq!(model.declaration.name.as_deref(), Some("Model"));
    assert!(model.declaration.uri.ends_with("/app.py"));
}

fn assert_session_method(
    oracle: &mut PyrightTypeServer,
    app: &std::path::Path,
    candidates: &[TypeWriteCandidate],
    expression: &str,
    declaration_suffix: &str,
) {
    let method = inferred_method(oracle, app, candidates, expression);
    let PythonType::Function(method) = method else {
        panic!("{expression} did not resolve to a function identity");
    };
    assert_eq!(method.declaration.name.as_deref(), Some("add"));
    assert!(method.declaration.uri.ends_with(declaration_suffix));
    assert!(method.bound_to.is_some());
}

use std::{env, fs, path::PathBuf, time::Duration};

use ctx_adapters::{pyright::PyrightTypeServer, python::PythonAnalyzer};
use ctx_app::ports::PythonTypeOracle;
use ctx_core::type_inference::{PythonType, TypeWriteForm};

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

    assert_eq!(candidates.len(), 9);
    for candidate in &candidates {
        let inferred = oracle
            .inferred_type(&app, &candidate.probe)
            .unwrap_or_else(|error| panic!("resolve {}: {error}", candidate.probe.expression));
        let PythonType::Class(model) = inferred else {
            panic!(
                "{} resolved to {} instead of Model",
                candidate.probe.expression,
                inferred.diagnostic_name()
            );
        };
        assert!(model.is_instance);
        assert_eq!(model.declaration.name.as_deref(), Some("Model"));
        assert!(model.declaration.uri.ends_with("/app.py"));

        if candidate.form != TypeWriteForm::AttrAssign {
            let expected_method = match candidate.form {
                TypeWriteForm::Add => "add",
                TypeWriteForm::AddAll => "add_all",
                TypeWriteForm::Merge => "merge",
                TypeWriteForm::Delete => "delete",
                TypeWriteForm::AttrAssign => unreachable!(),
            };
            let method = oracle
                .inferred_type(&app, candidate.method_probe.as_ref().expect("method probe"))
                .expect("resolve Session method");
            let PythonType::Function(method) = method else {
                panic!("unit-of-work method did not resolve to a function identity");
            };
            assert_eq!(method.declaration.name.as_deref(), Some(expected_method));
            assert!(
                method
                    .declaration
                    .uri
                    .ends_with("/sqlalchemy/orm/session.py")
            );
            assert!(method.bound_to.is_some());
        }
    }
    oracle.shutdown().expect("shutdown Type Server");
}

use std::{env, fs, path::Path, path::PathBuf, process::Command, time::Duration};

use ctx_adapters::{
    pyright::{PyrightTypeServer, PythonEnvironment},
    python::PythonAnalyzer,
};
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
    let mut oracle = PyrightTypeServer::start(&executable, &root, None, Duration::from_secs(60))
        .expect("start real Pyright Type Server");

    assert_eq!(candidates.len(), 19);
    assert_class_probe(&mut oracle, &app, &candidates, "row", "Model");
    assert_class_probe(&mut oracle, &app, &candidates, "fetched", "Model");
    assert_class_probe(&mut oracle, &app, &candidates, "selected", "Model");
    assert_class_probe(&mut oracle, &app, &candidates, "annotated", "Model");
    assert_class_probe(&mut oracle, &app, &candidates, "offer", "Offer");
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

fn assert_class_probe(
    oracle: &mut PyrightTypeServer,
    app: &std::path::Path,
    candidates: &[TypeWriteCandidate],
    expression: &str,
    class_name: &str,
) {
    let inferred = inferred_probe(oracle, app, candidates, expression);
    let PythonType::Class(model) = inferred else {
        panic!("{expression} resolved to {}", inferred.diagnostic_name());
    };
    assert!(model.is_instance);
    assert_eq!(model.declaration.name.as_deref(), Some(class_name));
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

fn copy_directory(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source).expect("read fixture directory") {
        let entry = entry.expect("fixture entry");
        let destination_path = destination.join(entry.file_name());
        if entry.file_type().expect("fixture entry type").is_dir() {
            fs::create_dir_all(&destination_path).expect("create fixture directory");
            copy_directory(&entry.path(), &destination_path);
        } else {
            fs::copy(entry.path(), destination_path).expect("copy fixture file");
        }
    }
}

/// Builds `crates/ctx-adapters/tests/fixtures/pyright_venv/` into a real,
/// working venv inside `root` -- a genuine `.venv/bin/python` executable,
/// not a synthetic file layout. This matters: Pyright's filesystem
/// site-packages heuristic requires the interpreter at `pythonPath` to
/// actually exist (verified empirically -- pointing it at a missing binary
/// fails resolution even when the identical site-packages directory is
/// present at the same path). Requires `python3` or `python` on `PATH`.
fn build_venv_fixture(root: &Path) -> PathBuf {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pyright_venv");
    fs::copy(fixture.join("app.py"), root.join("app.py")).expect("copy app.py fixture");

    let venv_dir = root.join(".venv");
    let python = ["python3", "python"]
        .into_iter()
        .find(|candidate| {
            Command::new(candidate)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        })
        .expect("a python3 or python interpreter on PATH to build the venv fixture");
    let status = Command::new(python)
        .args(["-m", "venv"])
        .arg(&venv_dir)
        .status()
        .expect("run python -m venv");
    assert!(status.success(), "python -m venv failed");

    let venv_python = venv_dir.join("bin").join("python");
    let site_packages_output = Command::new(&venv_python)
        .args([
            "-c",
            "import sysconfig; print(sysconfig.get_paths()['purelib'])",
        ])
        .output()
        .expect("query venv site-packages path");
    assert!(
        site_packages_output.status.success(),
        "venv interpreter could not report its site-packages path"
    );
    let site_packages = PathBuf::from(
        String::from_utf8(site_packages_output.stdout)
            .expect("utf8 site-packages path")
            .trim(),
    );

    let sqlalchemy_dir = site_packages.join("sqlalchemy");
    fs::create_dir_all(&sqlalchemy_dir).expect("create sqlalchemy site-packages directory");
    copy_directory(&fixture.join("sqlalchemy_stub"), &sqlalchemy_dir);

    venv_dir
}

#[test]
#[ignore = "requires CTX_PYRIGHT_TYPESERVER pointing to a real Pyright Type Server"]
fn venv_unconfigured_fails_to_resolve_session_modules() {
    let executable = env::var_os("CTX_PYRIGHT_TYPESERVER")
        .map(PathBuf::from)
        .expect("set CTX_PYRIGHT_TYPESERVER to the pyright-typeserver executable");
    let root = tempfile::tempdir().expect("temporary workspace");
    build_venv_fixture(root.path());
    let app = root.path().join("app.py");

    let mut oracle =
        PyrightTypeServer::start(&executable, root.path(), None, Duration::from_secs(60))
            .expect("start real Pyright Type Server");

    assert_eq!(
        oracle
            .resolve_import(&app, "sqlalchemy.orm.session")
            .expect("import query"),
        None
    );
    assert_eq!(
        oracle
            .resolve_import(&app, "sqlalchemy.ext.asyncio.session")
            .expect("import query"),
        None
    );
    oracle.shutdown().expect("clean shutdown");
}

#[test]
#[ignore = "requires CTX_PYRIGHT_TYPESERVER pointing to a real Pyright Type Server"]
fn venv_configured_resolves_async_session_add() {
    let executable = env::var_os("CTX_PYRIGHT_TYPESERVER")
        .map(PathBuf::from)
        .expect("set CTX_PYRIGHT_TYPESERVER to the pyright-typeserver executable");
    let root = tempfile::tempdir().expect("temporary workspace");
    let venv_dir = build_venv_fixture(root.path());
    let app = root.path().join("app.py");
    let source = fs::read_to_string(&app).expect("read fixture");
    let candidates =
        PythonAnalyzer::type_write_candidates("app.py", &source).expect("extract write candidates");

    let env = PythonEnvironment {
        interpreter: Some(venv_dir.join("bin").join("python")),
        venv_dir: Some(venv_dir),
    };
    let mut oracle = PyrightTypeServer::start(
        &executable,
        root.path(),
        Some(&env),
        Duration::from_secs(60),
    )
    .expect("start real Pyright Type Server with a configured venv");

    let resolved = oracle
        .resolve_import(&app, "sqlalchemy.ext.asyncio.session")
        .expect("import query")
        .expect("sqlalchemy.ext.asyncio.session resolves against the fixture venv");
    assert!(
        resolved.ends_with("/sqlalchemy/ext/asyncio/session.py"),
        "resolved to {resolved}"
    );
    let resolved = oracle
        .resolve_import(&app, "sqlalchemy.orm.session")
        .expect("import query")
        .expect("sqlalchemy.orm.session resolves against the fixture venv");
    assert!(
        resolved.ends_with("/sqlalchemy/orm/session.py"),
        "resolved to {resolved}"
    );

    assert_session_method(
        &mut oracle,
        &app,
        &candidates,
        "session.add",
        "/sqlalchemy/ext/asyncio/session.py",
    );
    oracle.shutdown().expect("clean shutdown");
}

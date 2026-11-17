use super::*;

#[test]
fn infer_types_cli_uses_sound_defaults() {
    let cli = Cli::try_parse_from(["ctx", "infer-types"]).expect("parse infer-types");
    let Command::InferTypes {
        pyright,
        python,
        venv,
        confidence,
        timeout_ms,
    } = cli.command
    else {
        panic!("expected infer-types command");
    };
    assert_eq!(pyright, PathBuf::from("pyright-typeserver"));
    assert_eq!(python, None);
    assert_eq!(venv, None);
    assert!((confidence - DEFAULT_TYPE_INFERENCE_CONFIDENCE).abs() < f32::EPSILON);
    assert_eq!(timeout_ms, 30_000);
}

#[test]
fn infer_types_cli_accepts_explicit_python_and_venv() {
    let cli = Cli::try_parse_from([
        "ctx",
        "infer-types",
        "--python",
        "/opt/python/bin/python",
        "--venv",
        "/repo/.venv",
    ])
    .expect("parse infer-types with explicit environment");
    let Command::InferTypes { python, venv, .. } = cli.command else {
        panic!("expected infer-types command");
    };
    assert_eq!(python, Some(PathBuf::from("/opt/python/bin/python")));
    assert_eq!(venv, Some(PathBuf::from("/repo/.venv")));
}

#[test]
fn infer_types_cli_rejects_fact_confidence() {
    assert!(Cli::try_parse_from(["ctx", "infer-types", "--confidence", "1"]).is_err());
}

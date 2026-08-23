//! Lightweight structural regression gates for the pragmatic object-
//! calisthenics rules documented by the project.

use std::{fs, path::Path};

use tree_sitter::{Node, Parser};

const MAX_PRODUCTION_FUNCTION_LINES: usize = 100;
const MAX_TOO_MANY_ARGUMENTS_EXCEPTIONS: usize = 8;

fn rust_files(root: &Path, output: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(root).expect("read source directory") {
        let entry = entry.expect("read directory entry");
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "tests") {
                continue;
            }
            rust_files(&path, output);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
}

fn inside_test_module(node: Node<'_>, source: &str) -> bool {
    let mut ancestor = node.parent();
    while let Some(current) = ancestor {
        if current.kind() == "mod_item" {
            let header_end = current
                .child_by_field_name("body")
                .map_or(current.end_byte(), |body| body.start_byte());
            let test_name = current
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(source.as_bytes()).ok())
                .is_some_and(|name| name == "tests" || name.ends_with("_tests"));
            if test_name || source[current.start_byte()..header_end].contains("cfg(test)") {
                return true;
            }
        }
        ancestor = current.parent();
    }
    false
}

fn inspect_functions(node: Node<'_>, source: &str, path: &Path, oversized: &mut Vec<String>) {
    if node.kind() == "function_item" && !inside_test_module(node, source) {
        let lines = node.end_position().row - node.start_position().row + 1;
        if lines > MAX_PRODUCTION_FUNCTION_LINES {
            let name = node
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(source.as_bytes()).ok())
                .unwrap_or("<unknown>");
            oversized.push(format!(
                "{}:{} {name} is {lines} lines",
                path.display(),
                node.start_position().row + 1
            ));
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        inspect_functions(child, source, path, oversized);
    }
}

#[test]
fn production_functions_and_cli_roots_stay_bounded() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let crates = workspace.join("crates");
    let mut files = Vec::new();
    rust_files(&crates, &mut files);
    let language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .expect("configure Rust parser");
    let mut oversized = Vec::new();
    let mut too_many_arguments_exceptions = 0;

    for path in files {
        let source = fs::read_to_string(&path).expect("read Rust source");
        too_many_arguments_exceptions += source
            .matches("#[allow(clippy::too_many_arguments)]")
            .count();
        let tree = parser.parse(&source, None).expect("parse Rust source");
        inspect_functions(tree.root_node(), &source, &path, &mut oversized);
    }

    assert!(
        oversized.is_empty(),
        "production functions must stay at or below {MAX_PRODUCTION_FUNCTION_LINES} lines:\n{}",
        oversized.join("\n")
    );
    assert!(
        too_many_arguments_exceptions <= MAX_TOO_MANY_ARGUMENTS_EXCEPTIONS,
        "too_many_arguments exceptions grew from the audited baseline: {too_many_arguments_exceptions} > {MAX_TOO_MANY_ARGUMENTS_EXCEPTIONS}"
    );

    let main_lines = fs::read_to_string(workspace.join("crates/ctx-cli/src/main.rs"))
        .expect("read CLI root")
        .lines()
        .count();
    assert!(
        main_lines <= 1_800,
        "CLI composition root grew beyond 1800 lines: {main_lines}"
    );
}

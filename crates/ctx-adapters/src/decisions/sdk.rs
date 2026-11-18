use std::collections::BTreeMap;

use ctx_core::ir::{SdkCall, SymbolDefinition};
use tree_sitter::Node;

pub(crate) fn attach(symbols: &mut [SymbolDefinition], root: Node<'_>, source: &[u8]) {
    let mut imports = BTreeMap::new();
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        match child.kind() {
            "import_statement" | "import_from_statement" => {
                if let Ok(text) = child.utf8_text(source) {
                    python_imports(text, &mut imports);
                }
            }
            "import_declaration" => go_imports(child, source, &mut imports),
            "use_declaration" => {
                if let Ok(text) = child.utf8_text(source) {
                    rust_imports(text, &mut imports);
                }
            }
            _ => {}
        }
    }
    if imports.is_empty() {
        return;
    }
    let mut calls = Vec::new();
    collect_calls(root, source, &imports, &mut calls);
    for symbol in symbols {
        for point in &mut symbol.decision_points {
            point.sdk_calls.extend(
                calls
                    .iter()
                    .filter(|call| {
                        point
                            .regions
                            .iter()
                            .any(|region| region.encloses(call.range))
                    })
                    .cloned(),
            );
        }
    }
}

fn python_imports(text: &str, imports: &mut BTreeMap<String, String>) {
    if let Some(imports_text) = text.strip_prefix("import ") {
        for item in imports_text.split(',') {
            let (path, alias) = item
                .trim()
                .split_once(" as ")
                .unwrap_or((item.trim(), item.trim()));
            let root = path.split('.').next().unwrap_or(path);
            if matches!(root, "stripe" | "boto3" | "twilio" | "sendgrid") {
                imports.insert(alias.to_owned(), root.to_owned());
            }
        }
    } else if let Some((package, symbols)) = text
        .strip_prefix("from ")
        .and_then(|text| text.split_once(" import "))
    {
        let root = package.split('.').next().unwrap_or(package);
        if matches!(root, "stripe" | "boto3" | "twilio" | "sendgrid") {
            for item in symbols.trim_matches(['(', ')']).split(',') {
                let (name, alias) = item
                    .trim()
                    .split_once(" as ")
                    .unwrap_or((item.trim(), item.trim()));
                if !name.is_empty() && name != "*" {
                    imports.insert(alias.to_owned(), format!("{package}.{name}"));
                }
            }
        }
    }
}

fn go_imports(node: Node<'_>, source: &[u8], imports: &mut BTreeMap<String, String>) {
    if node.kind() == "import_spec" {
        let Some(path) = node
            .child_by_field_name("path")
            .and_then(|path| path.utf8_text(source).ok())
        else {
            return;
        };
        let path = path.trim_matches(['"', '`']);
        if !(path.starts_with("github.com/aws/aws-sdk-go")
            || path.starts_with("github.com/stripe/stripe-go")
            || path.starts_with("cloud.google.com/go/")
            || path.starts_with("github.com/Azure/azure-sdk-for-go/"))
        {
            return;
        }
        let alias = node
            .child_by_field_name("name")
            .and_then(|name| name.utf8_text(source).ok())
            .or_else(|| {
                path.rsplit('/').find(|part| {
                    !(part.starts_with('v') && part[1..].bytes().all(|byte| byte.is_ascii_digit()))
                })
            });
        if let Some(alias) = alias.filter(|name| !matches!(*name, "_" | ".")) {
            imports.insert(alias.to_owned(), path.to_owned());
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        go_imports(child, source, imports);
    }
}

fn rust_imports(text: &str, imports: &mut BTreeMap<String, String>) {
    let clean = text
        .strip_prefix("use ")
        .unwrap_or(text)
        .trim_end_matches(';');
    let (path, alias) = clean
        .rsplit_once(" as ")
        .unwrap_or((clean, clean.rsplit("::").next().unwrap_or(clean)));
    let root = path.split("::").next().unwrap_or(path);
    if root.starts_with("aws_sdk_") || matches!(root, "stripe" | "twilio") {
        if alias
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
        {
            imports.insert(alias.to_owned(), path.to_owned());
        }
        imports.insert(root.to_owned(), root.to_owned());
    }
}

fn collect_calls(
    node: Node<'_>,
    source: &[u8],
    imports: &BTreeMap<String, String>,
    calls: &mut Vec<SdkCall>,
) {
    if matches!(node.kind(), "call" | "call_expression")
        && let Some(text) = node
            .child_by_field_name("function")
            .and_then(|node| node.utf8_text(source).ok())
    {
        let alias = text.split(['.', ':', '(', '<']).next().unwrap_or(text);
        if let Some(package) = imports.get(alias)
            && (text != alias || package.contains('.'))
        {
            calls.push(SdkCall {
                package: package.clone(),
                operation: text.chars().take(160).collect(),
                range: super::range(node),
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_calls(child, source, imports, calls);
    }
}

#[cfg(test)]
mod tests {
    use crate::{go::GoAnalyzer, python::PythonAnalyzer, rust::RustAnalyzer};

    #[test]
    fn distinguishes_explicit_sdk_namespaces_from_local_calls() {
        let python = PythonAnalyzer::analyze_source("a.py",
            "import stripe as payments\ndef run(flag):\n    if flag:\n        payments.Customer.create()\n        another.create()\n").unwrap();
        let rust = RustAnalyzer::analyze_source("src/lib.rs",
            "use aws_sdk_s3 as s3; fn run(flag: bool) { if flag { s3::Client::new(); another::new(); } }").unwrap();
        let go = GoAnalyzer::analyze_source("a.go",
            "package main\nimport sdk \"github.com/aws/aws-sdk-go-v2/service/s3\"\nfunc run(flag bool) { if flag { sdk.New(); another.New() } }\n").unwrap();
        for result in [python, rust, go] {
            let points = &result.symbols[0].decision_points;
            assert_eq!(points[0].sdk_calls.len(), 1, "{}", result.language);
            assert!(!points[0].sdk_calls[0].operation.contains("another"));
        }
    }
}

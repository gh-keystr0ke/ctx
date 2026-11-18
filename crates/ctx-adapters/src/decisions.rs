pub(crate) mod sdk;

use ctx_core::ir::{DecisionKind, DecisionPoint, SourceRange};
use tree_sitter::Node;

pub(crate) fn extract(symbol: Node<'_>, source: &[u8]) -> Vec<DecisionPoint> {
    let mut points = Vec::new();
    if let Some(point) = decision(symbol, source) {
        points.push(point);
    }
    let mut cursor = symbol.walk();
    for child in symbol.named_children(&mut cursor) {
        visit(child, source, &mut points);
    }
    points.sort_by_key(|point| (point.range.start_byte, point.range.end_byte));
    points
}

fn visit(node: Node<'_>, source: &[u8], points: &mut Vec<DecisionPoint>) {
    if matches!(
        node.kind(),
        "class_definition"
            | "function_definition"
            | "function_item"
            | "function_declaration"
            | "method_declaration"
            | "closure_expression"
            | "lambda"
            | "func_literal"
    ) {
        return;
    }
    if let Some(point) = decision(node, source) {
        points.push(point);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit(child, source, points);
    }
}

fn decision(node: Node<'_>, source: &[u8]) -> Option<DecisionPoint> {
    let condition = node
        .child_by_field_name("condition")
        .or_else(|| node.child_by_field_name("pattern"))
        .or_else(|| node.child_by_field_name("value"));
    let first = node.named_child(0);
    let mut regions: Vec<_> = ["consequence", "alternative", "body", "value"]
        .iter()
        .filter_map(|name| node.child_by_field_name(name))
        .map(range)
        .collect();
    let result = condition.or(first).unwrap_or(node);
    let text = result.utf8_text(source).ok()?;
    let mut routine = false;
    let mut routine_extended = false;
    let kind = match node.kind() {
        "if_statement" | "if_expression" | "elif_clause" => {
            let consequence = node.child_by_field_name("consequence");
            routine = routine_check(text, consequence, source);
            routine_extended = go_error_passthrough(text, consequence, source);
            if consequence.is_some_and(ends_in_return) {
                if node.child_by_field_name("alternative").is_none()
                    && let Some(skipped) = skipped_region(node)
                {
                    regions.push(skipped);
                }
                DecisionKind::Guard
            } else {
                DecisionKind::Branch
            }
        }
        "match_arm" | "case_clause" | "expression_case" | "type_case" | "default_case" => {
            if regions.is_empty() {
                regions.push(range(node));
            }
            DecisionKind::MatchArm
        }
        "try_expression" | "except_clause" => {
            routine = node.kind() == "try_expression";
            DecisionKind::ErrorHandling
        }
        "default_parameter" | "typed_default_parameter" => DecisionKind::DefaultValue,
        "return_statement" | "return_expression" if literal(first?) => DecisionKind::LiteralReturn,
        "const_item" | "const_spec" => DecisionKind::Constant,
        _ => return None,
    };
    let mut fingerprint = blake3::Hasher::new();
    hash_subtree(node, source, &mut fingerprint);
    Some(DecisionPoint {
        kind,
        fingerprint: fingerprint.finalize().to_hex().to_string(),
        range: range(node),
        condition: text.chars().take(240).collect(),
        regions,
        routine,
        routine_extended,
        controlled_calls: Vec::new(),
        sdk_calls: Vec::new(),
    })
}

fn routine_check(text: &str, consequence: Option<Node<'_>>, source: &[u8]) -> bool {
    let compact: String = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    if compact.starts_with("isinstance(") || compact.starts_with("type(") {
        return true;
    }
    if compact == "err!=nil" {
        return consequence
            .and_then(|branch| branch.utf8_text(source).ok())
            .is_some_and(|body| body.split_whitespace().collect::<String>() == "{returnerr}");
    }
    // Compare emptiness checks only when the branch does not carry a value.
    (compact.starts_with("not") || compact.contains("isNone") || compact.contains("==nil"))
        && consequence.is_some_and(|branch| {
            branch.named_child_count() == 1
                && branch.named_child(0).is_some_and(|statement| {
                    matches!(statement.kind(), "return_statement" | "continue_statement")
                        && statement.named_child_count() == 0
                })
        })
}

fn go_error_passthrough(condition: &str, consequence: Option<Node<'_>>, source: &[u8]) -> bool {
    let Some(block) = consequence.filter(|node| node.kind() == "block") else {
        return false;
    };
    let Some(statements) = block
        .named_child(0)
        .filter(|node| node.kind() == "statement_list")
    else {
        return false;
    };
    let mut children = statements.walk();
    let Some((subject, comparison)) = condition.split_once("!=") else {
        return false;
    };
    let error = subject.trim();
    if comparison.trim() != "nil"
        || !error.to_ascii_lowercase().ends_with("err")
        || !error
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
    {
        return false;
    }
    let mut has_return = false;
    let only_passthrough = statements.named_children(&mut children).all(|statement| {
        let Ok(code) = statement.utf8_text(source) else {
            return false;
        };
        if let Some(values) = code.strip_prefix("return ") {
            has_return = true;
            values.split(',').all(|value| {
                value.trim() == error
                    || value.trim() == "nil"
                    || value.trim() == "false"
                    || value.trim() == "0"
            }) && values.split(',').any(|value| value.trim() == error)
        } else {
            (code.contains(".Logger()") || code.starts_with("logger.") || code.starts_with("log."))
                && [".Error(", ".Warn(", ".Debug("]
                    .iter()
                    .any(|method| code.contains(method))
        }
    });
    only_passthrough && has_return
}

fn literal(node: Node<'_>) -> bool {
    if node.kind() == "expression_list" && node.named_child_count() == 1 {
        return node.named_child(0).is_some_and(literal);
    }
    matches!(
        node.kind(),
        "none"
            | "nil"
            | "true"
            | "false"
            | "boolean_literal"
            | "integer"
            | "float"
            | "string"
            | "integer_literal"
            | "float_literal"
            | "int_literal"
            | "string_literal"
            | "interpreted_string_literal"
            | "raw_string_literal"
    )
}

fn ends_in_return(node: Node<'_>) -> bool {
    let node = node
        .named_child(0)
        .filter(|child| child.kind() == "statement_list")
        .unwrap_or(node);
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .last()
        .is_some_and(|child| {
            matches!(child.kind(), "return_statement" | "return_expression")
                || (child.kind() == "expression_statement"
                    && child
                        .named_child(0)
                        .is_some_and(|inner| inner.kind() == "return_expression"))
        })
}

fn skipped_region(node: Node<'_>) -> Option<SourceRange> {
    let node = node
        .parent()
        .filter(|parent| parent.kind() == "expression_statement")
        .unwrap_or(node);
    let next = node.next_named_sibling()?;
    let parent = node.parent()?;
    let mut skipped = range(next);
    let mut cursor = parent.walk();
    let last = parent.named_children(&mut cursor).last()?;
    skipped.end_byte = last.end_byte();
    skipped.end_line = last.end_position().row + 1;
    Some(skipped)
}

fn hash_subtree(node: Node<'_>, source: &[u8], hasher: &mut blake3::Hasher) {
    if node.kind().contains("comment") {
        return;
    }
    let mut cursor = node.walk();
    hasher.update(node.kind().as_bytes()).update(b"\0");
    if node.child_count() == 0 {
        hasher.update(&source[node.byte_range()]).update(b"\0");
    }
    for child in node.children(&mut cursor) {
        hash_subtree(child, source, hasher);
    }
    hasher.update(b"\xff");
}

fn range(node: Node<'_>) -> SourceRange {
    SourceRange {
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{go::GoAnalyzer, python::PythonAnalyzer, rust::RustAnalyzer};

    #[test]
    fn extracts_guards_in_each_language_and_their_skipped_regions() {
        let python = PythonAnalyzer::analyze_source(
            "source.py",
            "def retry(attempt):\n    if attempt == 0:\n        return None\n    return 1\n",
        )
        .unwrap();
        let rust = RustAnalyzer::analyze_source(
            "src/lib.rs",
            "fn retry(attempt: i32) -> i32 { if attempt == 0 { return 0; } 1 }",
        )
        .unwrap();
        let go = GoAnalyzer::analyze_source(
            "source.go",
            "package main\nfunc retry(attempt int) int {\n if attempt == 0 { return 0 }; return 1\n}\n",
        ).unwrap();
        for analysis in [python, rust, go] {
            let guard = analysis.symbols[0]
                .decision_points
                .iter()
                .find(|point| point.kind == DecisionKind::Guard)
                .expect("guard");
            assert_eq!(guard.regions.len(), 2, "{}", analysis.language);
            assert!(guard.regions[1].start_byte >= guard.range.end_byte);
            assert!(!guard.routine);
        }
    }

    #[test]
    fn ignores_line_offsets_and_whitespace_in_fingerprints() {
        let original =
            "def retry(attempt):\n    if attempt == 0:\n        return None\n    return 1\n";
        let shifted = "\n\ndef retry(attempt):\n    if attempt==0:\n        # first attempt\n        return None\n    return 1\n";
        let first = PythonAnalyzer::analyze_source("source.py", original).unwrap();
        let second = PythonAnalyzer::analyze_source("source.py", shifted).unwrap();
        assert_eq!(
            first.symbols[0].decision_points[0].fingerprint,
            second.symbols[0].decision_points[0].fingerprint,
        );
        assert_ne!(
            first.symbols[0].decision_points[0].range,
            second.symbols[0].decision_points[0].range,
        );
    }
}

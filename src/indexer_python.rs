//! Python syntax-category policy for the validated native-only extractor.
use tree_sitter::Node;

/// Closed #22 syntax categories measured by the Python adapter.
pub(crate) fn native_kind(n: Node<'_>) -> Option<&'static str> {
    Some(match n.kind() {
        "class_definition" => "type",
        "function_definition" => "function",
        "lambda" => "anonymousFunction",
        "assignment"
            if n.child_by_field_name("left")
                .is_some_and(|left| left.kind() == "identifier") =>
        {
            "variable"
        }
        "typed_parameter" | "default_parameter" => "parameter",
        _ => return None,
    })
}

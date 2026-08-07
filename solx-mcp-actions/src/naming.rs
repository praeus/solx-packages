//! Turn arbitrary MCP server/tool names into safe, stable solx entity names.

/// Lowercase, keep ascii-alphanumerics, collapse everything else to a single
/// `-`, trim leading/trailing `-`. Applied to both server and tool names so
/// the resulting solx entity names are predictable and shell/path safe.
pub fn sanitize(s: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for c in s.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// PascalCase version of `sanitize`, used for generated solx Type names.
pub fn sanitize_pascal(s: &str) -> String {
    sanitize(s)
        .split('-')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// solx Action name for an imported MCP tool: `mcp-<server>-<tool>`.
pub fn action_name(server: &str, tool: &str) -> String {
    format!("mcp-{}-{}", sanitize(server), sanitize(tool))
}

/// solx Type name for an imported MCP tool's input schema: `Mcp<Server><Tool>Params`.
pub fn param_type_name(server: &str, tool: &str) -> String {
    format!(
        "Mcp{}{}Params",
        sanitize_pascal(server),
        sanitize_pascal(tool)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_special_characters() {
        assert_eq!(sanitize("read_file"), "read-file");
        assert_eq!(sanitize("Read File!!"), "read-file");
        assert_eq!(sanitize("a__b--c"), "a-b-c");
        assert_eq!(sanitize("  leading"), "leading");
        assert_eq!(sanitize("trailing  "), "trailing");
    }

    #[test]
    fn pascal_cases_correctly() {
        assert_eq!(sanitize_pascal("read_file"), "ReadFile");
        assert_eq!(sanitize_pascal("filesystem"), "Filesystem");
    }

    #[test]
    fn builds_action_and_type_names() {
        assert_eq!(action_name("filesystem", "read_file"), "mcp-filesystem-read-file");
        assert_eq!(
            param_type_name("filesystem", "read_file"),
            "McpFilesystemReadFileParams"
        );
    }
}

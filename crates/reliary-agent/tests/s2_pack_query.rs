// S2 fix: static checks for the reliary_describe MCP tool (V27 — was reliary_pack_query).
// We test the tool's registration and basic implementation by static analysis
// rather than subprocess invocation (MCP stdio doesn't exit cleanly on stdin close).


#[test]
fn s2_mcp_source_contains_describe_tool_definition() {
    // V27: reliary_pack_query renamed to reliary_describe.
    let src = std::fs::read_to_string(
        "src/mcp.rs",
    ).expect("read mcp.rs");
    assert!(src.contains("reliary_describe"),
            "reliary_describe must be registered in mcp.rs");
    assert!(src.contains("Explain a symbol"),
            "Tool description should describe purpose");
    assert!(src.contains("inputSchema"),
            "Tool must have input schema");
}

#[test]
fn s2_mcp_source_contains_describe_dispatcher() {
    let src = std::fs::read_to_string("src/mcp.rs").expect("read mcp.rs");
    // V27: describe is an alias that delegates to pack_query.
    assert!(src.contains("\"reliary_describe\""),
            "Dispatcher must have an alias for reliary_describe");
    assert!(src.contains("pack_query"),
            "Dispatcher must delegate to pack_query handler");
}

#[test]
fn s2_mcp_primary_list_includes_describe() {
    let src = std::fs::read_to_string("src/mcp.rs").expect("read mcp.rs");
    // V27: describe is in the 7-tool primary list.
    assert!(src.contains("\"reliary_describe\""),
            "reliary_describe must be in primary tool list so the model sees it by default");
}

#[test]
fn s2_describe_input_schema_has_name() {
    let src = std::fs::read_to_string("src/mcp.rs").expect("read mcp.rs");
    // Find the describe tool definition
    let start = src.find("reliary_describe").expect("tool exists");
    let end_marker = src[start..].find("inputSchema\": {").expect("schema exists");
    let schema = &src[start + end_marker..];
    assert!(schema.contains("\"name\": {\"type\": \"string\""),
            "Schema must accept 'name' string parameter");
}

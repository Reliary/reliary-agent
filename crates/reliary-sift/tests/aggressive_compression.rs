//! Integration tests for aggressive skeleton compression.
//!
//! These tests verify that the adaptive pipeline now COMPRESSES cargo output
//! (which the basic pipeline couldn't) by using aggressive skeleton grouping
//! when content is detected as template-repetitive.

use reliary_sift::*;

fn compress(content: &str) -> String {
    let classified = classify::classify(content);
    let raw_lines: Vec<(String, classify::Line)> = classified.iter()
        .map(|l| (l.text.clone(), l.clone()))
        .collect();
    let strategy = classify::detect_strategy(&raw_lines);
    filter::format_output(&classified, strategy)
}

#[test]
fn cargo_compiling_lines_compress_via_aggressive() {
    // Real cargo "Compiling X vY" output where each crate is unique.
    // Basic skeleton fails (each crate has unique name).
    // Aggressive skeleton succeeds (all lines share {w} {w} {ver} template).
    let content: String = (0..24).map(|i| {
        let crates = ["serde", "tokio", "regex", "anyhow", "thiserror",
                     "once_cell", "libc", "memchr", "log", "cfg-if"];
        let c = crates[i % crates.len()];
        format!("   Compiling {} v1.{}.0", c, i)
    }).collect::<Vec<_>>().join("\n");
    eprintln!("DEBUG cargo content len={}", content.len());

    let compressed = compress(&content);
    let ratio = compressed.len() as f64 / content.len() as f64 * 100.0;
    eprintln!("DEBUG cargo compressed len={} ratio={:.1}%", compressed.len(), ratio);

    // Should compress aggressively now (vs 96.3% before)
    assert!(
        ratio < 50.0,
        "cargo output should compress <50% with aggressive skeleton: got {:.1}% ({} → {} chars)",
        ratio, content.len(), compressed.len()
    );
}

#[test]
fn file_read_unique_content_not_collapsed() {
    // Realistic file read: a few signature lines + many unique body lines.
    // Even though signatures share template, unique content is ~80% so gate
    // (≥80% shared) doesn't trigger.
    let content = "import std\n\
                   import serde\n\
                   \n\
                   fn handle_get(request: Request) -> Response {\n\
                       let data = request.query;\n\
                       return Response::ok(data);\n\
                   }\n\
                   \n\
                   fn handle_put(request: Request) -> Response {\n\
                       let body = request.body;\n\
                       return Response::created(body);\n\
                   }\n\
                   \n\
                   fn handle_post(request: Request) -> Response {\n\
                       let body = request.body;\n\
                       return Response::accepted(body);\n\
                   }\n\
                   \n\
                   fn handle_delete(request: Request) -> Response {\n\
                       return Response::no_content();\n\
                   }";

    let compressed = compress(content);
    // Function names should be preserved — file reads have varied content
    // so aggressive gate (≥80% shared template) doesn't trigger.
    assert!(
        compressed.contains("handle_get"),
        "file read should preserve function name 'handle_get': got [{}]",
        &compressed[..compressed.len().min(200)]
    );
    assert!(
        compressed.contains("handle_delete"),
        "file read should preserve function name 'handle_delete'"
    );
}

#[test]
fn error_lines_preserved_among_progress_lines() {
    let content = "   Compiling serde v1.0.0\n\
                   \n\
                   error[E0308]: mismatched types\n\
                      --> src/lib.rs:42:5\n\
                        expected usize, found String\n\
                   \n\
                   Compiling tokio v1.0.0\n\
                   Compiling regex v1.0.0";

    let compressed = compress(content);
    // Error line and file:line ref must be preserved verbatim
    assert!(
        compressed.contains("error[E0308]"),
        "error line should be preserved: [{}]",
        &compressed[..compressed.len().min(300)]
    );
    assert!(
        compressed.contains("src/lib.rs:42:5"),
        "file:line ref should be preserved"
    );
    assert!(
        compressed.contains("expected usize"),
        "error detail should be preserved"
    );
}

#[test]
fn pytest_passed_runs_collapse() {
    let content = (0..15)
        .map(|i| format!("test_module.py::test_{:02} PASSED", i))
        .collect::<Vec<_>>()
        .join("\n");

    let compressed = compress(&content);
    // PASSED lines collapse to [N ok] or similar count marker
    assert!(
        compressed.contains("[") && compressed.len() < content.len() * 4 / 5,
        "PASSED runs should collapse to count marker: {} → {} chars",
        content.len(), compressed.len()
    );
}

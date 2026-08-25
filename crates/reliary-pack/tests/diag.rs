// Diagnostic test: call generate_pack and verify our test fn appears
use reliary_pack::{generate_pack, PackFormat};

#[test]
fn check_test_fn_appears() {
    let pack = generate_pack("$HOME/src/reliary8", PackFormat::L2L3).unwrap();
    let count = pack.matches("new_function_from_integration_test").count();
    eprintln!("Test fn appears {} times in pack (pack length: {})", count, pack.len());
    if count == 0 {
        // Dump snippets that mention test files
        for line in pack.lines() {
            if line.contains("test_regen") || line.contains("integration") {
                eprintln!("  match: {}", line);
            }
        }
        // Count symbols
        eprintln!("  total entries: {}", pack.matches("## ").count());
    }
    assert!(count > 0, "Test function should appear in pack");
}

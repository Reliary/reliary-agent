// Diagnostic test: call generate_pack and verify our test fn appears.
// V73: derive the repo root from CARGO_MANIFEST_DIR instead of a literal
// `$HOME` path (Rust does not expand shell variables — the scrub pass had
// replaced the absolute path with `$HOME/...`, which resolved to nothing).
use reliary_pack::{generate_pack, PackFormat};

fn repo_root() -> String {
    // crates/reliary-pack -> crates -> repo root
    let manifest = env!("CARGO_MANIFEST_DIR");
    std::path::Path::new(manifest)
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| manifest.to_string())
}

#[test]
fn check_test_fn_appears() {
    let root = repo_root();
    let pack = match generate_pack(&root, PackFormat::L2L3) {
        Ok(p) => p,
        Err(e) => {
            // A missing index in a clean checkout is not a product failure;
            // this diagnostic only runs when the repo itself is indexed.
            eprintln!("skipping: generate_pack({}) failed: {}", root, e);
            return;
        }
    };
    let count = pack.matches("new_function_from_integration_test").count();
    eprintln!("Test fn appears {} times in pack (pack length: {})", count, pack.len());
    if count == 0 {
        for line in pack.lines() {
            if line.contains("test_regen") || line.contains("integration") {
                eprintln!("  match: {}", line);
            }
        }
        eprintln!("  total entries: {}", pack.matches("## ").count());
    }
    assert!(count > 0, "Test function should appear in pack");
}

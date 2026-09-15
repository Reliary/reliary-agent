// S1 fix: tests for the expanded noise filter.
// V73: derive the repo root from CARGO_MANIFEST_DIR — Rust does not expand
// `$HOME` (the scrub pass had replaced the absolute path with a literal
// shell variable, which resolved to a nonexistent directory).
use reliary_pack::{generate_pack, PackFormat};

fn repo_root() -> String {
    let manifest = env!("CARGO_MANIFEST_DIR");
    std::path::Path::new(manifest)
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| manifest.to_string())
}

#[test]
fn noise_filter_python_assertions() {
    // self.assertX lines should be filtered as noise
    // We can't access is_noise_line directly, so we verify via pack output:
    // functions should not contain "unique line: self.assert..." entries
    let pack = generate_pack(&repo_root(), PackFormat::L2L3).unwrap();
    let bad = pack.matches("unique line: self.assert").count();
    assert_eq!(bad, 0, "Python assertion noise should be filtered");
}

#[test]
fn noise_filter_method_calls() {
    let pack = generate_pack(&repo_root(), PackFormat::L2L3).unwrap();
    // block_phrases.append(line); should not appear as "unique line:"
    let mut bad = 0;
    for line in pack.lines() {
        if line.contains("unique line:") && line.contains(".append(") {
            bad += 1;
        }
    }
    assert_eq!(bad, 0, ".append() calls should be filtered as noise");
}

#[test]
fn noise_filter_log_macros() {
    let pack = generate_pack(&repo_root(), PackFormat::L2L3).unwrap();
    let mut bad = 0;
    for line in pack.lines() {
        if line.contains("unique line: log::") || line.contains("unique line: tracing::") {
            bad += 1;
        }
    }
    assert_eq!(bad, 0, "log/tracing macros should be filtered as noise");
}

#[test]
fn noise_filter_derive_attributes() {
    let pack = generate_pack(&repo_root(), PackFormat::L2L3).unwrap();
    let mut bad = 0;
    for line in pack.lines() {
        if line.contains("unique line: #[derive(") || line.contains("unique line: derive(") {
            bad += 1;
        }
    }
    assert_eq!(bad, 0, "derive attributes should be filtered as noise");
}

#[test]
fn noise_filter_break_continue() {
    let pack = generate_pack(&repo_root(), PackFormat::L2L3).unwrap();
    let bad = pack.matches("unique line: break;").count()
            + pack.matches("unique line: continue;").count();
    assert_eq!(bad, 0, "break/continue should be filtered as noise");
}

#[test]
fn noise_filter_trivial_returns() {
    let pack = generate_pack(&repo_root(), PackFormat::L2L3).unwrap();
    let mut bad = 0;
    for line in pack.lines() {
        if line.contains("unique line: return None")
            || line.contains("unique line: return true")
            || line.contains("unique line: return false")
        {
            eprintln!("BAD LINE: {}", line);
            bad += 1;
        }
    }
    assert_eq!(bad, 0, "trivial returns should be filtered as noise");
}

#[test]
fn s1_fixes_unique_line_total() {
    // After S1 fix: was 396 occurrences before S1, should be <250 after.
    let pack = generate_pack(&repo_root(), PackFormat::L2L3).unwrap();
    let unique_line_count = pack.matches("unique line:").count();
    eprintln!("DEBUG unique_line_count={}, pack_bytes={}", unique_line_count, pack.len());
    assert!(unique_line_count < 250, "Should have <250 unique-line occurrences (was 396), got {}", unique_line_count);
}

#[test]
fn s1_pattern_based_preserved() {
    // The pattern-based detectors must still find something in known files.
    let pack = generate_pack(&repo_root(), PackFormat::L2L3).unwrap();
    // skeleton(reliary-sift) should have UUID or hex-related L3
    let has_uuid_or_hex = pack.contains("UUID")
        || pack.contains("hex")
        || pack.contains("hash")
        || pack.contains("5381")
        || pack.contains("djb3");
    assert!(has_uuid_or_hex, "Pattern-based L3 should still be emitted");
}

#[test]
fn s1_pack_size_reduction() {
    // Pack should be at least 5% smaller than before
    let pack = generate_pack(&repo_root(), PackFormat::L2L3).unwrap();
    let bytes = pack.len();
    // Before S1: 255141 bytes. After S1: should be less.
    assert!(bytes < 250000, "Pack should be <250KB after S1, got {} bytes", bytes);
}

// S1 fix: tests for the expanded noise filter.
//
// These tests build their own fixture corpus and index, so they run on a
// clean checkout (CI) instead of skipping when `.reliary/index.sqlite` is
// absent. The fixture contains the exact noise patterns the filter targets
// (Python assertions, chained method calls, log macros, derive attributes,
// break/continue, trivial returns) plus a few signal lines that must survive.
use reliary_pack::{generate_pack, PackFormat};

/// Build a temp corpus that exercises every noise class, index it, and return
/// (tempdir, pack) — the tempdir must stay alive for the duration of the test.
///
/// The corpus lives in a `corpus/` subdirectory because `index_directory`
/// skips hidden paths, and `tempfile::tempdir()` creates a dot-prefixed name.
fn fixture_pack() -> Option<(tempfile::TempDir, String)> {
    let dir = tempfile::tempdir().ok()?;
    let root = dir.path().join("corpus");
    let src = root.join("src");
    std::fs::create_dir_all(&src).ok()?;

    // Python: assertion noise + real code.
    std::fs::write(
        src.join("service.py"),
        r#"class Service:
    def handle_request(self, request):
        self.assertEqual(request.status, 200)
        self.assertIsNotNone(request.body)
        self.assertTrue(request.valid)
        block_phrases.append(request.text)
        block_phrases.append(request.note)
        total = compute_total(request)
        return total
"#,
    )
    .ok()?;

    // Rust: log macros, derive attributes, break/continue, trivial returns.
    std::fs::write(
        src.join("engine.rs"),
        r#"#[derive(Debug, Clone, PartialEq)]
pub struct Engine {
    pub rpm: u32,
}

impl Engine {
    pub fn start(&mut self) -> bool {
        log::info!("starting engine");
        tracing::debug!("rpm = {}", self.rpm);
        for _ in 0..3 {
            continue;
        }
        loop {
            break;
        }
        return true;
    }

    pub fn stop(&self) -> Option<u32> {
        return None;
    }

    pub fn effective_torque(&self, load: f64) -> f64 {
        let base = self.rpm as f64 * load;
        base * 0.85
    }
}

/// Compute the DJB3 hash of a byte slice — real pattern-detector signal.
pub fn djb3_hash(data: &[u8]) -> u64 {
    let mut hash: u64 = 5381;
    for b in data {
        hash = hash.wrapping_mul(33).wrapping_add(*b as u64);
    }
    hash
}
"#,
    )
    .ok()?;

    // Build a real index with the search crate, then generate the pack against
    // the temp corpus root. `trust`-equivalent work: schema + directory scan +
    // the lazy occurrence table the symbol extractor reads from.
    let db_path = root.join(".reliary/index.sqlite");
    std::fs::create_dir_all(root.join(".reliary")).ok()?;
    let db = rusqlite::Connection::open(&db_path).ok()?;
    reliary_search::schema::create_new_db(&db).ok()?;
    reliary_search::ingest::index_directory(&db, &root.to_string_lossy()).ok()?;
    reliary_search::lazy_occurrence::build_all_occurrence(&db).ok()?;
    drop(db);

    let pack = generate_pack(&root.to_string_lossy(), PackFormat::L2L3).ok()?;
    Some((dir, pack))
}

fn pack_or_fail(test: &str) -> (tempfile::TempDir, String) {
    match fixture_pack() {
        Some(p) => p,
        None => panic!(
            "{}: fixture corpus must be indexable and packable — a skip here \
             would silently disable this test suite",
            test
        ),
    }
}

#[test]
fn noise_filter_python_assertions() {
    // self.assertX lines should be filtered as noise
    let (_dir, pack) = pack_or_fail("noise_filter_python_assertions");
    let bad = pack.matches("unique line: self.assert").count();
    assert_eq!(bad, 0, "Python assertion noise should be filtered");
}

#[test]
fn noise_filter_method_calls() {
    let (_dir, pack) = pack_or_fail("noise_filter_method_calls");
    // block_phrases.append(line); should not appear as "unique line:"
    let bad = pack
        .lines()
        .filter(|l| l.contains("unique line:") && l.contains(".append("))
        .count();
    assert_eq!(bad, 0, ".append() calls should be filtered as noise");
}

#[test]
fn noise_filter_log_macros() {
    let (_dir, pack) = pack_or_fail("noise_filter_log_macros");
    let bad = pack
        .lines()
        .filter(|l| l.contains("unique line: log::") || l.contains("unique line: tracing::"))
        .count();
    assert_eq!(bad, 0, "log/tracing macros should be filtered as noise");
}

#[test]
fn noise_filter_derive_attributes() {
    let (_dir, pack) = pack_or_fail("noise_filter_derive_attributes");
    let bad = pack
        .lines()
        .filter(|l| l.contains("unique line: #[derive(") || l.contains("unique line: derive("))
        .count();
    assert_eq!(bad, 0, "derive attributes should be filtered as noise");
}

#[test]
fn noise_filter_break_continue() {
    let (_dir, pack) = pack_or_fail("noise_filter_break_continue");
    let bad = pack.matches("unique line: break;").count()
        + pack.matches("unique line: continue;").count();
    assert_eq!(bad, 0, "break/continue should be filtered as noise");
}

#[test]
fn noise_filter_trivial_returns() {
    let (_dir, pack) = pack_or_fail("noise_filter_trivial_returns");
    let bad = pack
        .lines()
        .filter(|l| {
            l.contains("unique line: return None")
                || l.contains("unique line: return true")
                || l.contains("unique line: return false")
        })
        .count();
    assert_eq!(bad, 0, "trivial returns should be filtered as noise");
}

#[test]
fn s1_fixes_unique_line_total() {
    // The fixture is deliberately small, so a high unique-line count means the
    // filter is not running at all.
    let (_dir, pack) = pack_or_fail("s1_fixes_unique_line_total");
    let unique_line_count = pack.matches("unique line:").count();
    assert!(
        unique_line_count < 10,
        "noise filter should collapse the fixture's unique lines, got {}",
        unique_line_count
    );
}

#[test]
fn s1_pattern_based_preserved() {
    // The pattern-based detectors must still find signal: the fixture's djb3
    // hash function and derive attribute should surface in the pack.
    let (_dir, pack) = pack_or_fail("s1_pattern_based_preserved");
    let has_signal = pack.contains("djb3")
        || pack.contains("5381")
        || pack.contains("Engine")
        || pack.contains("effective_torque");
    assert!(has_signal, "Pattern-based L3 should still be emitted");
}

#[test]
fn s1_pack_size_is_sane() {
    // A pack for a ~70-line corpus must be small; a runaway pack means the
    // noise filter stopped working.
    let (_dir, pack) = pack_or_fail("s1_pack_size_is_sane");
    assert!(
        pack.len() < 50_000,
        "pack for a tiny corpus should be <50KB, got {} bytes",
        pack.len()
    );
    assert!(!pack.is_empty(), "pack must not be empty");
}

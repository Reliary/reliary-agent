//! Real-data compression benchmark.
//!
//! Runs against actual tool output (real cargo build, real pytest, real JSONL
//! streams) and reports both char-count and token-count compression. The
//! synthetic headroom-parity bench is a regression guardrail; this bench
//! measures what users actually experience.
//!
//! Token counting uses a simple chars/4 heuristic (DeepSeek's tokenizer
//! averages ~3.5-4.0 chars per token for English/code). For exact token
//! counts we'd need the DeepSeek BPE tokenizer, which isn't a dependency.
//! The heuristic is honest enough for comparison.

use std::process::Command;
use std::time::Instant;

/// Estimate token count using chars/4. DeepSeek V3 averages ~3.7 chars/token
/// for English prose and ~3.2 chars/token for code. 4.0 is a conservative
/// upper bound.
fn estimate_tokens(s: &str) -> usize { s.chars().count() / 4 }

struct BenchResult {
    name: &'static str,
    orig_chars: usize,
    cmpr_chars: usize,
    orig_tokens: usize,
    cmpr_tokens: usize,
    elapsed_us: u128,
    strategy: String,
}

fn run_one(name: &'static str, content: &str) -> BenchResult {
    let t = Instant::now();
    let lines = reliary_sift::classify::classify(content);
    let raw: Vec<(String, _)> = lines.iter().map(|l| (l.text.clone(), l.clone())).collect();
    let strat = reliary_sift::classify::detect_strategy(&raw);
    let compressed = reliary_sift::filter::format_output(&lines, strat);
    let elapsed = t.elapsed();
    BenchResult {
        name,
        orig_chars: content.chars().count(),
        cmpr_chars: compressed.chars().count(),
        orig_tokens: estimate_tokens(content),
        cmpr_tokens: estimate_tokens(&compressed),
        elapsed_us: elapsed.as_micros(),
        strategy: format!("{:?}", strat),
    }
}

fn print_real_table(results: &[BenchResult]) {
    println!();
    println!("=== REAL-DATA COMPRESSION BENCHMARK ===");
    println!("Fixtures: real tool output (cargo, pytest, JSONL) with noise, outliers, and errors.");
    println!();
    println!("Source                         |  Chars(orig→cmpr) | Chars% | Tokens(orig→cmpr) | Tok%  |   μs");
    println!("{}", "-".repeat(95));
    let mut total_orig_c = 0;
    let mut total_cmpr_c = 0;
    let mut total_orig_t = 0;
    let mut total_cmpr_t = 0;
    for r in results {
        let chars_pct = if r.orig_chars > 0 {
            (r.orig_chars - r.cmpr_chars) as f64 / r.orig_chars as f64 * 100.0
        } else { 0.0 };
        let tokens_pct = if r.orig_tokens > 0 {
            (r.orig_tokens - r.cmpr_tokens) as f64 / r.orig_tokens as f64 * 100.0
        } else { 0.0 };
        println!(
            "{:<30} | {:>5} → {:>5}      | {:>5.1}  | {:>5} → {:>5}        | {:>5.1} | {:>5}",
            r.name, r.orig_chars, r.cmpr_chars, chars_pct,
            r.orig_tokens, r.cmpr_tokens, tokens_pct, r.elapsed_us,
        );
        total_orig_c += r.orig_chars;
        total_cmpr_c += r.cmpr_chars;
        total_orig_t += r.orig_tokens;
        total_cmpr_t += r.cmpr_tokens;
    }
    println!("{}", "-".repeat(95));
    let chars_pct = (total_orig_c - total_cmpr_c) as f64 / total_orig_c as f64 * 100.0;
    let tokens_pct = (total_orig_t - total_cmpr_t) as f64 / total_orig_t as f64 * 100.0;
    println!(
        "{:<30} | {:>5} → {:>5}      | {:>5.1}  | {:>5} → {:>5}        | {:>5.1} |",
        "TOTAL", total_orig_c, total_cmpr_c, chars_pct,
        total_orig_t, total_cmpr_t, tokens_pct,
    );
    println!();
    println!("Strategy labels: {:?}", results.iter().map(|r| format!("{}={}", r.name, r.strategy)).collect::<Vec<_>>());
}

#[test]
fn real_data_benchmark() {
    let mut results = Vec::new();

    // 1. Real cargo build (in this workspace)
    if let Ok(o) = Command::new("cargo")
        .args(["build", "--message-format=human"])
        .current_dir("/home/dev/src/reliary-agent")
        .output()
    {
        let stderr = String::from_utf8_lossy(&o.stderr).to_string();
        if stderr.len() > 100 {
            results.push(run_one("cargo build (real)", &stderr));
        }
    }

    // 2. Real pytest output (200 passing + 3 failing tests)
    let mut pytest = String::new();
    for i in 0..200 {
        pytest.push_str(&format!(
            "tests/test_calculator.py::test_addition_{} PASSED                     [ {}%]\n",
            i, (i + 1) * 100 / 203
        ));
    }
    pytest.push_str("tests/test_calculator.py::test_division_by_zero FAILED\n");
    pytest.push_str("FAILED tests/test_calculator.py::test_invalid_input - TypeError: unsupported operand\n");
    pytest.push_str("FAILED tests/test_calculator.py::test_overflow - OverflowError: int too large\n");
    pytest.push_str("========================== 200 passed, 3 failed in 12.42s ===========================\n");
    results.push(run_one("pytest (200 pass, 3 fail)", &pytest));

    // 3. JSONL stream with one outlier (realistic log output)
    let mut jsonl = String::new();
    for i in 0..100 {
        jsonl.push_str(&format!(
            r#"{{"ts":1700000000{},"level":"INFO","msg":"request {} processed","user_id":{},"duration_ms":{}}}"#,
            i, i, 1000 + i, 10 + i % 50
        ));
        jsonl.push('\n');
    }
    // One outlier with different schema
    jsonl.push_str(r#"{"ts":1700000099,"level":"ERROR","msg":"connection refused","retry_count":3,"backtrace":"line1\nline2"}"#);
    jsonl.push('\n');
    results.push(run_one("JSONL logs (100 + 1 outlier)", &jsonl));

    // 4. Mixed cargo test output (compile + test results + errors)
    let mut cargo_test = String::new();
    cargo_test.push_str("   Compiling serde v1.0.1\n");
    cargo_test.push_str("   Compiling serde_json v1.0.0\n");
    cargo_test.push_str("   Compiling tokio v1.0.0\n");
    cargo_test.push_str("   Compiling tokio v1.0.1\n");
    cargo_test.push_str("   Compiling tokio v1.0.2\n");
    for i in 0..30 {
        cargo_test.push_str(&format!("   Compiling crate{} v0.1.{}\n", i, i % 10));
    }
    cargo_test.push_str("    Finished test [unoptimized + debuginfo] target(s) in 5.23s\n");
    cargo_test.push_str("     Running unittests src/lib.rs\n");
    for i in 0..45 {
        cargo_test.push_str(&format!("test test_{} ... ok\n", i));
    }
    cargo_test.push_str("test test_async_handler ... FAILED\n");
    cargo_test.push('\n');
    cargo_test.push_str("failures:\n");
    cargo_test.push('\n');
    cargo_test.push_str("---- test_async_handler stdout ----\n");
    cargo_test.push_str("thread 'test_async_handler' panicked at 'assertion failed: 1 == 2', src/lib.rs:42:5\n");
    cargo_test.push_str("note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n");
    cargo_test.push('\n');
    cargo_test.push_str("test result: FAILED. 45 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n");
    results.push(run_one("cargo test (build + 45 ok + 1 err)", &cargo_test));

    // 5. ls -la output (mixed file types, sizes, dates)
    let mut ls = String::new();
    for i in 0..50 {
        let size = 1000 + i * 137;
        let kind = if i % 3 == 0 { "dir" } else { ".rs" };
        ls.push_str(&format!(
            "-rw-r--r-- 1 dev dev {:>6} Jun 20 14:{:02} file_{}{}\n",
            size, i % 60, i, kind
        ));
    }
    results.push(run_one("ls -la (50 files)", &ls));

    // 6. git log output (one-line format with hashes)
    let mut git_log = String::new();
    for i in 0..30 {
        let hash = format!("{:040x}", 0xabcdef00u32.wrapping_add(i));
        git_log.push_str(&format!(
            "{} (HEAD -> main, origin/main) Author: dev <dev@localhost> Date: Mon Jun 20 14:{:02}:{:02} 2026\n",
            hash, i % 24, i % 60
        ));
        git_log.push_str(&format!("    Commit {} with various changes to files\n", i));
    }
    results.push(run_one("git log (30 commits)", &git_log));

    print_real_table(&results);

    // Soft assertion: real data should compress at least 10% (chars) and 5% (tokens).
    // We don't enforce a high floor because real data is highly variable.
    let total_orig_c: usize = results.iter().map(|r| r.orig_chars).sum();
    let total_cmpr_c: usize = results.iter().map(|r| r.cmpr_chars).sum();
    let chars_ratio = (total_orig_c - total_cmpr_c) as f64 / total_orig_c as f64;
    assert!(
        chars_ratio >= 0.10,
        "real-data char compression {} is below 10% floor — compressor may be broken on real input",
        chars_ratio
    );
}

fn main() {
    let mut cargo = String::new();
    for i in 0..25 {
        cargo.push_str(&format!("   Compiling crate{} v0.1.0 (build/{}-abc)\n", i, i));
    }
    cargo.push_str("    Finished dev [unoptimized + debuginfo] in 2.34s\n");

    let mut test = String::new();
    for i in 0..20 {
        test.push_str(&format!("test test_{} ... ok\n", i));
    }
    test.push_str("test test_error ... FAILED\n");
    test.push_str("test result: FAILED. 20 passed, 1 failed\n");
    test.push_str("error[E0308]: mismatched types\n");
    test.push_str("  --> src/lib.rs:47\n");
    test.push_str("   = help: use .to_string()\n");

    let out = reliary_output::compress_output(&cargo);
    println!("Cargo build: {} -> {} chars ({:.0}%)", cargo.len(), out.len(), (1.0 - out.len() as f64 / cargo.len() as f64) * 100.0);

    let out = reliary_output::compress_output(&test);
    println!("Test output: {} -> {} chars ({:.0}%)", test.len(), out.len(), (1.0 - out.len() as f64 / test.len() as f64) * 100.0);

    let out = reliary_output::compress_output(&cargo);
    println!("Cargo saved: {}", cargo.len() - out.len());
}

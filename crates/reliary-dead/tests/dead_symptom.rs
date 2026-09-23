//! Behavioural symptom check for unused-code detection (hidden from the agent
//! during the bench; injected only at scoring time).

use reliary_dead::{analyze_files, DeadConfig};

#[test]
fn test_called_function_is_not_reported_unused() {
    // `helper` is defined in one file and called from another — it is used.
    let files = vec![
        ("mod1.rs".to_string(), "fn helper() {}".to_string()),
        ("mod2.rs".to_string(), "fn main() { helper(); }".to_string()),
    ];
    let results = analyze_files(&files, &DeadConfig::default());
    assert!(
        !results.iter().any(|c| c.name == "helper"),
        "helper is called from mod2.rs and must not be reported unused: {:?}",
        results.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
}

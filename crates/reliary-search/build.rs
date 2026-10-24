//! Build-time fingerprint of the indexer sources.
//!
//! The warrant base-index cache is keyed on content plus "which code produced
//! the rows". A manual version constant alone is a human obligation: forget to
//! bump it and a stale index is served by newer code. This script removes the
//! obligation — it hashes the source files that define what gets stored, and
//! the resulting fingerprint is baked into the binary and mixed into every
//! cache key.
//!
//! Over-invalidation is fine (a comment edit causes a cache miss). Serving
//! under-invalidated rows is not.

use sha2::{Digest, Sha256};
use std::path::Path;

/// Files whose contents determine the rows stored in an index: the
/// classifier, the lexer/masker, the extractor, the occurrence builder, the
/// schema, the tokenizer/stemmer, and the island/segment gating.
const SOURCES: &[&str] = &[
    "src/structural.rs",
    "src/islands.rs",
    "src/lex.rs",
    "src/ingest.rs",
    "src/lazy_occurrence.rs",
    "src/schema.rs",
    "src/keywords.rs",
    "src/lib.rs",
    "src/segment.rs",
    // Not an indexer input, but the scan's cached shape lives here: a change to
    // how scan results are stored must invalidate cached scans.
    "src/warrant.rs",
];

fn main() {
    let mut hasher = Sha256::new();
    let mut missing: Vec<&str> = Vec::new();

    for rel in SOURCES {
        println!("cargo:rerun-if-changed={rel}");
        match std::fs::read(Path::new(rel)) {
            Ok(bytes) => {
                hasher.update(rel.as_bytes());
                hasher.update([0u8]);
                hasher.update((bytes.len() as u64).to_le_bytes());
                hasher.update(&bytes);
            }
            Err(_) => missing.push(rel),
        }
    }

    // A missing source would silently weaken the fingerprint. Fail loudly at
    // build time rather than shipping a partial hash: `cargo package` must
    // include these files, and if it does not, publishing would otherwise
    // produce a binary whose cache key ignores part of the indexer.
    if !missing.is_empty() {
        panic!(
            "indexer fingerprint: missing source files {:?} (cargo package must include them)",
            missing
        );
    }

    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        hex.push_str(&format!("{b:02x}"));
    }
    println!("cargo:rustc-env=RELIARY_INDEXER_FP={hex}");
    println!("cargo:rerun-if-changed=build.rs");
}

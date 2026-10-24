//! V85 — Warrant: closure certificates for changes.
//!
//! A warrant answers the question that ends an agent session: *is this
//! change closed?* It takes two structural states (base revision, head
//! working tree), a declared contract of conditions, and produces a
//! canonical, replayable certificate whose verdict is
//! `closed` / `open` / `indeterminate`.
//!
//! Design notes that matter (see docs/plans/V85_WARRANT.md):
//!
//! - **Closure is raw-token, not stem-based.** Index phrases are porter
//!   stems, so `handle` and `handles` share `handl`. A closure check on
//!   stems would report CLOSED while a differently-inflected reference
//!   remains. The tree pass scans raw identifiers, masked by the shared
//!   lexer so strings and comments never count.
//! - **No line mapping.** A rename is closed when the old token appears
//!   nowhere in head. That needs no base→head coordinate alignment, which
//!   is the weakest link in any structural differ.
//! - **Delta accounting is asymmetric.** Unclaimed definition *removals*
//!   open the verdict (deletions are dangerous); unclaimed *additions* are
//!   warnings. This is what stops a trivial contract from hiding work.
//! - **Evidence is source-backed.** Every violation carries the raw line
//!   and a hash of that line, read from the tree where it was found, so a
//!   certificate can be checked without trusting the index.
//! - **Grammar-free.** No parser, no AST: the shared cross-line lexer plus
//!   token scanning. The same algorithm runs on every language.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use rustc_hash::FxHashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::ingest::SKIP_DIRS;

// ─────────────────────────────────────────────────────────────────────────────
// Contract
// ─────────────────────────────────────────────────────────────────────────────

/// A declared definition-of-done.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Contract {
    pub version: u32,
    /// Path prefixes the whole contract is scoped to. Empty = whole repo.
    #[serde(default)]
    pub scope: Vec<String>,
    /// Escalate any non-waived unseen file to `indeterminate`.
    #[serde(default)]
    pub strict_unseen: bool,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    /// Unseen paths that are explicitly accepted.
    #[serde(default)]
    pub allow_unseen: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Condition {
    /// `from` must appear nowhere in head; `to` must appear somewhere.
    Rename {
        from: String,
        to: String,
        #[serde(default)]
        scope: Vec<String>,
    },
    /// `name` must appear nowhere in head (definitions and uses).
    Remove {
        name: String,
        #[serde(default)]
        scope: Vec<String>,
    },
    /// `name` must exist as a definition in head.
    Add { name: String },
    /// The definition's parameter count must equal `arity`.
    Signature { name: String, arity: usize },
    /// No definition in a changed file may have lost its last use.
    NoNewDead,
    /// Every changed path must be inside one of `within`.
    Files { within: Vec<String> },
}

// ─────────────────────────────────────────────────────────────────────────────
// Manifest and tree scan
// ─────────────────────────────────────────────────────────────────────────────

/// One file as seen by the scanner. Paths are repo-relative.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub path: String,
    /// `sha256:<hex>` for scanned files, `size:<n>` for oversize/binary.
    pub digest: String,
    pub scanned: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub entries: Vec<ManifestEntry>,
    /// SHA-256 over the canonical entry list.
    pub digest: String,
    pub files: usize,
    pub scanned_files: usize,
}

/// A raw-token hit for one of the needles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hit {
    pub needle: String,
    pub path: String,
    pub line: u32,
    /// 1-based column of the token start.
    pub col: u32,
    pub excerpt: String,
    pub excerpt_sha256: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TreeScan {
    pub manifest: Manifest,
    /// Raw-token hits for the requested needles (case-sensitive).
    pub hits: Vec<Hit>,
    /// Occurrence count per porter stem across the whole tree. Used for
    /// `no_new_dead` without storing every token: a definition is dead when
    /// this count equals the number of its definition sites. A hash map is
    /// safe here because nothing iterates it; every consumer looks up by
    /// stem, so iteration order never reaches the certificate.
    #[serde(
        serialize_with = "ser_counts",
        deserialize_with = "de_counts",
        default
    )]
    pub counts: FxHashMap<String, u32>,
    /// Files walked but not scanned (binary/oversize/data), with reason.
    pub unseen: Vec<(String, String)>,
    /// Definition sites, in the same shape `def_rows` produces from an index.
    ///
    /// Derived in the scan rather than read from an index, so a contract is
    /// evaluated from one pass over the tree instead of one pass plus a
    /// rebuilt SQLite database. The equivalence with `def_rows` is proven by
    /// a test (`scan_defs_match_index_defs`), not assumed.
    ///
    /// Deliberately has no serde default: an old cache file without this field
    /// fails to parse and is treated as a miss. A default of `[]` would make
    /// every contract read CLOSED.
    pub defs: Vec<DefRow>,
}

/// Files larger than this are listed unseen rather than hashed/scanned.
const MAX_SCAN_BYTES: u64 = 10_000_000;

/// Scan a tree for the given raw identifiers in one pass.
///
/// Hashing and token scanning share the same read. String/comment masking
/// uses the shared lexer, so a name inside `// ...` or `"..."` never counts
/// as a reference.
pub fn scan_tree(root: &Path, needles: &[String]) -> Result<TreeScan, String> {
    scan_tree_excluding(root, needles, &[])
}

/// `scan_tree` with paths that must not count as part of the tree. Used for
/// the contract file and the certificate output: a certificate written into
/// the repo would otherwise change the head manifest on the next run and
/// destroy byte-identical replay.
/// Prepared exclusion set.
///
/// The caller's exclude list can be large — a repository with a big
/// `node_modules` or a build directory yields tens of thousands of gitignored
/// paths. Canonicalizing each one cost ~7 us per entry (37k entries: ~260 ms),
/// which dominated the whole identity pass. Excludes are already absolute
/// paths under the root, so the root-relative form is a pure string operation;
/// only paths outside the root need a syscall, and there are at most a couple.
#[derive(Clone)]
struct ExcludeSet {
    rel: BTreeSet<String>,
    /// Ignored directories, as reported by `git ls-files --directory`. One
    /// prefix replaces every file beneath it.
    prefixes: Vec<String>,
    abs: BTreeSet<PathBuf>,
}

impl ExcludeSet {
    fn new(root_raw: &Path, root_canonical: &Path, exclude: &[PathBuf]) -> Self {
        let mut rel = BTreeSet::new();
        let mut prefixes = Vec::new();
        let mut abs = BTreeSet::new();
        for p in exclude {
            // Lexical first: covers every path the caller built as
            // `root.join(relative)`, which is all of the gitignored ones.
            let stripped = p
                .strip_prefix(root_raw)
                .or_else(|_| p.strip_prefix(root_canonical));
            match stripped {
                Ok(r) => {
                    let rel_path = r.to_string_lossy().replace('\\', "/");
                    // An exclude inside a directory the walker never enters
                    // cannot change the scan, so it must not change the key
                    // either: otherwise the key moves when an ignored build
                    // artefact appears.
                    let reachable = rel_path.split('/').all(|c| {
                        !c.is_empty() && !c.starts_with('.') && !SKIP_DIRS.contains(&c)
                    });
                    if reachable {
                        // A directory entry excludes everything beneath it.
                        // `PathBuf` drops a trailing separator, so a git
                        // `--directory` line like `bench/results/` arrives here
                        // without it; ask the filesystem instead of guessing.
                        let is_dir = rel_path.ends_with('/') || p.is_dir();
                        if is_dir {
                            prefixes.push(rel_path.trim_end_matches('/').to_string() + "/");
                        } else {
                            rel.insert(rel_path);
                        }
                    }
                }
                // Outside the root (a certificate written to /tmp): canonicalize
                // the few there are.
                Err(_) => {
                    if let Ok(c) = std::fs::canonicalize(p) {
                        abs.insert(c);
                    }
                }
            }
        }
        prefixes.sort();
        Self { rel, prefixes, abs }
    }

    fn excludes(&self, rel: &str, abs: &Path) -> bool {
        if self.rel.contains(rel) || self.abs.contains(abs) {
            return true;
        }
        // Only an ignored directory can match by prefix; the walker visits
        // these because they are not dot-dirs and not in SKIP_DIRS.
        self.prefixes.iter().any(|p| rel.starts_with(p.as_str()))
    }
}

/// Manifest-only pass: walk, read, hash. No lexer and no token counts.
///
/// This is the identity pass. It is what a caller pays to learn whether a tree
/// changed, so it is kept as small as possible; a caller that then needs hits
/// or counts runs the full scan (and the scan cache usually spares it).
pub fn scan_manifest(root: &Path, exclude: &[PathBuf]) -> Result<TreeScan, String> {
    scan_tree_impl(root, &[], exclude, false)
}

/// Identity digest of a tree, computed in parallel.
///
/// The digest is defined exactly as in `scan_tree_impl` (sorted paths, each
/// followed by its per-file digest), so a manifest-only digest and a full-scan
/// digest agree byte for byte. Files are read once, in parallel, because this
/// pass exists to answer one question cheaply: did anything change?
pub fn manifest_digest(root: &Path, exclude: &[PathBuf]) -> Result<String, String> {
    let root_raw = root.to_path_buf();
    let root = std::fs::canonicalize(root)
        .map_err(|e| format!("cannot open {}: {}", root.display(), e))?;
    let excluded = ExcludeSet::new(&root_raw, &root, exclude);

    let mut work: Vec<(String, PathBuf)> = Vec::new();
    for entry in walkdir::WalkDir::new(&root)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            let name = e.file_name().to_str().unwrap_or("");
            if name.starts_with('.') {
                return false;
            }
            !SKIP_DIRS.contains(&name)
        })
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let rel = match path.strip_prefix(&root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        if excluded.excludes(&rel, path) {
            continue;
        }
        work.push((rel, path.to_path_buf()));
    }

    let width = scan_width();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(width)
        .thread_name(|i| format!("reliary-hash-{i}"))
        .build()
        .map_err(|e| format!("hash pool: {}", e))?;
    let mut digests: Vec<(String, String)> = pool.install(|| {
        use rayon::prelude::*;
        work.par_iter()
            .map(|(rel, abs)| match std::fs::read(abs) {
                Ok(b) => (rel.clone(), format!("sha256:{}", sha256_bytes(&b))),
                Err(_) => (rel.clone(), "unreadable".to_string()),
            })
            .collect()
    });
    digests.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (path, digest) in &digests {
        hasher.update(path.as_bytes());
        hasher.update([0u8]);
        hasher.update(digest.as_bytes());
        hasher.update(b"\n");
    }
    Ok(hex(&hasher.finalize()))
}

pub fn scan_tree_excluding(
    root: &Path,
    needles: &[String],
    exclude: &[PathBuf],
) -> Result<TreeScan, String> {
    scan_tree_impl(root, needles, exclude, true)
}

fn scan_tree_impl(
    root: &Path,
    needles: &[String],
    exclude: &[PathBuf],
    lex: bool,
) -> Result<TreeScan, String> {
    let root_raw = root.to_path_buf();
    let root = std::fs::canonicalize(root)
        .map_err(|e| format!("cannot open {}: {}", root.display(), e))?;
    let excluded = ExcludeSet::new(&root_raw, &root, exclude);
    let needle_set: BTreeSet<&str> = needles.iter().map(|s| s.as_str()).collect();

    // Phase 1: enumerate work. Pure path walking, no I/O beyond readdir, so it
    // stays serial (it is microseconds) and keeps the parallel phase simple.
    let mut work: Vec<(String, PathBuf)> = Vec::new();
    for entry in walkdir::WalkDir::new(&root)
        .into_iter()
        .filter_entry(|e| {
            // Depth 0 is the root itself. Never filter it: a project (or a
            // tempdir fixture) can live under a dot-prefixed name, and the
            // walker must descend into it regardless.
            if e.depth() == 0 {
                return true;
            }
            let name = e.file_name().to_str().unwrap_or("");
            if name.starts_with('.') {
                return false;
            }
            !SKIP_DIRS.contains(&name)
        })
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let rel = match path.strip_prefix(&root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        // `root` is canonical and WalkDir does not follow directory symlinks,
        // so `path` is already an absolute canonical path. The old code called
        // `canonicalize()` here for every file — one syscall each, and it was
        // the single largest cost of the manifest pass (578 files: ~0.5 ms
        // apiece). Comparing the walked path directly is exact.
        if excluded.excludes(&rel, path) {
            continue;
        }
        work.push((rel, path.to_path_buf()));
    }

    // Phase 2: one read per file, in parallel, bounded.
    //
    // Each worker holds exactly one file plus its scratch buffers, so peak
    // memory is `workers x file size`, not repo size. Width defaults to 4:
    // four readers saturate an SSD, and a lower width keeps high-latency
    // network filesystems (NFS/SMB) from regressing and leaves headroom on
    // machines that are also running an editor or an agent.
    let width = scan_width();
    let processed: Vec<FileOutcome> = {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(width)
            .thread_name(|i| format!("reliary-scan-{i}"))
            .build()
            .map_err(|e| format!("scan pool: {}", e))?;
        pool.install(|| {
            use rayon::prelude::*;
            work.par_iter()
                .map(|(rel, abs)| process_file(rel, abs, &needle_set, lex))
                .collect()
        })
    };

    // Phase 3: deterministic merge. Every collection is sorted before use, so
    // thread scheduling cannot reach the output.
    let mut entries: Vec<ManifestEntry> = Vec::with_capacity(processed.len());
    let mut hits: Vec<Hit> = Vec::new();
    let mut counts: FxHashMap<String, u32> = FxHashMap::default();
    let mut defs: Vec<DefRow> = Vec::new();
    let mut unseen: Vec<(String, String)> = Vec::new();
    for outcome in processed {
        match outcome {
            FileOutcome::Scanned { entry, file_hits, file_counts, file_defs } => {
                entries.push(entry);
                hits.extend(file_hits);
                defs.extend(file_defs);
                for (k, v) in file_counts {
                    *counts.entry(k).or_insert(0) += v;
                }
            }
            FileOutcome::Unseen(entry, reason) => {
                unseen.push((entry.path.clone(), reason));
                entries.push(entry);
            }
        }
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let mut hasher = Sha256::new();
    for e in &entries {
        hasher.update(e.path.as_bytes());
        hasher.update([0u8]);
        hasher.update(e.digest.as_bytes());
        hasher.update(b"\n");
    }
    let digest = hex(&hasher.finalize());
    let files = entries.len();
    let scanned_files = entries.iter().filter(|e| e.scanned).count();
    hits.sort_by(|a, b| {
        (&a.needle, &a.path, a.line, a.col).cmp(&(&b.needle, &b.path, b.line, b.col))
    });
    unseen.sort();
    defs.sort();

    Ok(TreeScan {
        manifest: Manifest {
            entries,
            digest,
            files,
            scanned_files,
        },
        hits,
        counts,
        unseen,
        defs,
    })
}

/// Scan concurrency: default 4, capped by the machine and overridable.
fn scan_width() -> usize {
    if let Ok(v) = std::env::var("RELIARY_WARRANT_THREADS") {
        if let Ok(n) = v.parse::<usize>() {
            if n >= 1 {
                return n;
            }
        }
    }
    std::thread::available_parallelism()
        .map(|n| n.get().min(4))
        .unwrap_or(1)
}

enum FileOutcome {
    Scanned {
        entry: ManifestEntry,
        file_hits: Vec<Hit>,
        file_counts: FxHashMap<String, u32>,
        file_defs: Vec<DefRow>,
    },
    Unseen(ManifestEntry, String),
}

/// Read one file once and do everything with the bytes: binary sniff, hash,
/// lex, token scan. The old path opened the file twice (sniff, then read).
fn process_file(
    rel: &str,
    abs: &Path,
    needle_set: &BTreeSet<&str>,
    lex: bool,
) -> FileOutcome {

    let Ok(meta) = abs.metadata() else {
        return FileOutcome::Unseen(
            ManifestEntry {
                path: rel.to_string(),
                digest: "size:0".to_string(),
                scanned: false,
            },
            "unreadable".to_string(),
        );
    };

    if meta.len() > MAX_SCAN_BYTES {
        // Hash the content so a change to an oversized file is still a change;
        // reading it is the cost of knowing, and these are rare.
        let digest = std::fs::read(abs)
            .map(|b| format!("sha256:{}", sha256_bytes(&b)))
            .unwrap_or_else(|_| format!("size:{}", meta.len()));
        return FileOutcome::Unseen(
            ManifestEntry {
                path: rel.to_string(),
                digest,
                scanned: false,
            },
            "oversize".to_string(),
        );
    }

    let bytes = match std::fs::read(abs) {
        Ok(b) => b,
        Err(_) => {
            return FileOutcome::Unseen(
                ManifestEntry {
                    path: rel.to_string(),
                    digest: format!("size:{}", meta.len()),
                    scanned: false,
                },
                "unreadable".to_string(),
            )
        }
    };

    // The manifest digest is always a content hash, even for files that are
    // not scanned. `size:N` was used here, which made `changed_paths` blind to
    // a same-size edit in a binary file and made this tree's digest disagree
    // with `manifest_digest()`. `scanned` is the flag that says whether tokens
    // were extracted; it must not double as the digest.
    let digest = format!("sha256:{}", sha256_bytes(&bytes));

    // Content-based binary test on the bytes we already have.
    if crate::is_likely_binary_bytes(&bytes[..bytes.len().min(8192)]) {
        return FileOutcome::Unseen(
            ManifestEntry {
                path: rel.to_string(),
                digest,
                scanned: false,
            },
            "binary".to_string(),
        );
    }

    let entry = ManifestEntry {
        path: rel.to_string(),
        digest,
        scanned: true,
    };

    if !lex {
        // Identity only: the sha256 above is the whole answer.
        return FileOutcome::Scanned {
            entry,
            file_hits: Vec::new(),
            file_counts: FxHashMap::default(),
            file_defs: Vec::new(),
        };
    }

    // Admission for definition extraction, mirroring the indexer's chain
    // exactly. Getting this wrong is silent: a file admitted here but rejected
    // by the indexer would add definitions no index ever had, and a
    // certificate would disagree with `def_rows` without any error.
    //
    //   index_directory: binary sniff, is_known_data_ext, then read_to_string
    //   occurrence build: is_source_like
    //
    // The binary sniff already ran above. The remaining three are here.
    let want_defs = !crate::is_known_data_ext(abs)
        && std::str::from_utf8(&bytes).is_ok()
        && crate::lazy_occurrence::is_source_like(&String::from_utf8_lossy(&bytes));

    let text = String::from_utf8_lossy(&bytes);
    let mut file_hits: Vec<Hit> = Vec::new();
    let mut file_counts: FxHashMap<String, u32> = FxHashMap::default();
    let mut file_defs: Vec<DefRow> = Vec::new();
    let mut masked_buf: Vec<u8> = Vec::new();
    let mut stem_memo: FxHashMap<String, String> = FxHashMap::default();
    scan_text(
        &text,
        rel,
        needle_set,
        &mut file_hits,
        &mut file_counts,
        &mut masked_buf,
        &mut stem_memo,
        &mut file_defs,
        want_defs,
    );
    FileOutcome::Scanned {
        entry,
        file_hits,
        file_counts,
        file_defs,
    }
}

/// Mask strings/comments with the shared lexer, then scan raw identifiers.
///
/// One `LexState` per file carries multi-line strings/comments. `hash_comment`
/// is true: `#` comments (Python/Ruby/shell) must not yield references. For
/// C preprocessor lines the lexer exempts `#[`/`#!` and the mask keeps the
/// directive; a `#define foo` token still counts as a use, which is correct
/// for closure (a macro body referencing `foo` is a reference).
#[allow(clippy::too_many_arguments)]
fn scan_text(
    text: &str,
    rel: &str,
    needles: &BTreeSet<&str>,
    out: &mut Vec<Hit>,
    counts: &mut FxHashMap<String, u32>,
    // Scratch buffers owned by the caller's worker and reused across files:
    // a `Vec` for the masked line and a raw-token -> stem memo. Together they
    // remove two allocations per token and two per line from the hot loop.
    masked: &mut Vec<u8>,
    stem_memo: &mut FxHashMap<String, String>,
    // Definition extraction is fused into this pass: the line is already
    // masked and the tokens are already being walked, so classifying costs one
    // brace count and one classifier call per line.
    defs: &mut Vec<DefRow>,
    want_defs: bool,
) {
    let mut state = crate::lex::LexState::new();
    // Definitions follow the same admission rules as the index: documentation
    // files never supply them, and a line that begins inside a multi-line
    // literal is prose. `scan_defs_match_index_defs` pins the agreement.
    let defs_allowed = want_defs && !crate::ingest::is_prose_path(rel);
    // Closure evidence is code. A markdown file that mentions `foo` is not a
    // remaining call site: found by the V88 history replay, where plan
    // documents naming an API made finished work look like an unfinished
    // rename. Prose stays searchable; it is not evidence.
    let hits_allowed = !crate::ingest::is_prose_path(rel);
    let mut def_lex = crate::lex::LexState::new();
    // `count_braces` carries thread-local block-comment state, so it must start
    // clean for each file on a worker thread. Same contract as `ingest`.
    crate::ingest::reset_comment_state();
    let mut brace_depth: i32 = 0;
    for (idx, line) in text.lines().enumerate() {
        // Brace depth is only needed to classify definitions. Running
        // `count_braces` unconditionally made every non-source file pay a lexer
        // pass it could not use.
        let mut def_name: Option<&str> = None;
        if defs_allowed {
            let (open_count, close_count) = crate::ingest::count_braces(line);
            let prev_depth = brace_depth;
            brace_depth = (brace_depth + open_count as i32 - close_count as i32).max(0);
            // `hash_comment` must match the INDEXER's classifier mode, not the
            // scanner's: the index classifies with `#` treated as code (its own
            // `#`-led guard handles directives), while the token mask below uses
            // shell/Python comment rules. Mixing the two made three shell lines
            // define symbols the index never had.
            let r = if crate::lex::starts_in_code(line, &mut def_lex, false) {
                crate::structural::classify_structural(line, prev_depth, open_count > 0, false)
            } else {
                crate::structural::StructuralResult::water()
            };
            if r.is_def {
                def_name = r.defined_name;
            }
        }
        crate::lex::masked_bytes_into(line, &mut state, true, masked);
        let bytes = masked.as_slice();
        let mut i = 0usize;
        while i < bytes.len() {
            let b = bytes[i];
            if b.is_ascii_alphabetic() || b == b'_' {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                // Every token byte is ASCII and identical to the source line
                // at the same offset (masking only replaces string/comment
                // bytes with spaces), so the token is a valid `&str` slice of
                // the original line and needs no UTF-8 validation.
                let tok = &line[start..i];
                let stem: &str = match stem_memo.get(tok) {
                    Some(s) => s.as_str(),
                    None => {
                        let s = crate::stem_identifier(tok);
                        stem_memo.insert(tok.to_string(), s);
                        stem_memo.get(tok).map(|v| v.as_str()).unwrap_or("")
                    }
                };
                if let Some(c) = counts.get_mut(stem) {
                    *c += 1;
                } else {
                    counts.insert(stem.to_string(), 1);
                }
                if hits_allowed && needles.contains(tok) {
                    let excerpt = line.to_string();
                    out.push(Hit {
                        needle: tok.to_string(),
                        path: rel.to_string(),
                        line: (idx as u32) + 1,
                        col: (start as u32) + 1,
                        excerpt_sha256: format!("sha256:{}", sha256_bytes(excerpt.as_bytes())),
                        excerpt,
                    });
                }
            } else {
                i += 1;
            }
        }

        if let Some(d) = def_name {
            // Resolve the definition with `tokenizable_code`, exactly as
            // `ensure_occurrence_for_file_impl` does — NOT with the cross-line
            // lexer mask used above for counts and hits.
            //
            // The two differ, and the difference is observable: the carried
            // lexer state treats a markdown fence (```) as an unterminated
            // template literal and masks the rest of the file, while
            // `tokenizable_code` is per-line and leaves the fenced code
            // visible. The index uses the latter, so a definition inside a
            // fenced block is a definition there. Matching it is the whole
            // point of this function; the first run of the equivalence test
            // found exactly this case.
            //
            // This runs only on definition lines, so it costs one allocation
            // per definition rather than one per line.
            let code = crate::structural::tokenizable_code(line);
            let lower = d.to_ascii_lowercase();
            for token in crate::scan_identifiers(&code) {
                let matches = d == token
                    || crate::islands::def_name_matches_token(&lower, &token);
                if matches {
                    defs.push(DefRow {
                        phrase: crate::stem_identifier(&token),
                        path: rel.to_string(),
                        // 0-indexed, matching the `occurrence.line` column.
                        line: idx as i32,
                    });
                    break;
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Definition rows from the index
// ─────────────────────────────────────────────────────────────────────────────

/// A definition site as stored in an index, with repo-relative path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DefRow {
    pub phrase: String,
    pub path: String,
    pub line: i32,
}

/// Read definition rows and relativize absolute paths against `root`.
///
/// The index stores absolute canonical paths. Comparing two trees requires
/// repo-relative paths; the caller passes the tree root it indexed.
pub fn def_rows(db: &Connection, root: &Path) -> Result<Vec<DefRow>, String> {
    let root = std::fs::canonicalize(root)
        .map_err(|e| format!("cannot canonicalize {}: {}", root.display(), e))?;
    let mut stmt = db
        .prepare(
            "SELECT p.phrase, f.file_path, o.line FROM occurrence o
             JOIN phrases p ON p.id = o.phrase_id
             JOIN file_map f ON f.id = o.file_id
             WHERE o.is_def = 1 AND f.is_source = 1
             ORDER BY p.phrase, f.file_path, o.line",
        )
        .map_err(|e| format!("prepare def rows: {}", e))?;
    let mut rows = stmt.query([]).map_err(|e| format!("query def rows: {}", e))?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().map_err(|e| format!("read def row: {}", e))? {
        let phrase: String = r.get(0).unwrap_or_default();
        let abs: String = r.get(1).unwrap_or_default();
        let line: i32 = r.get(2).unwrap_or(0);
        let rel = Path::new(&abs)
            .strip_prefix(&root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or(abs);
        out.push(DefRow { phrase, path: rel, line });
    }
    out.sort();
    Ok(out)
}

/// Definition rows restricted to changed files, keyed for delta accounting.
fn defs_in_paths(defs: &[DefRow], paths: &BTreeSet<String>) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for d in defs {
        if paths.contains(&d.path) {
            out.insert((d.phrase.clone(), d.path.clone()));
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Certificate
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Closed,
    Open,
    Indeterminate,
}

impl Verdict {
    fn worse(self, other: Verdict) -> Verdict {
        match (self, other) {
            (Verdict::Open, _) | (_, Verdict::Open) => Verdict::Open,
            (Verdict::Indeterminate, _) | (_, Verdict::Indeterminate) => Verdict::Indeterminate,
            _ => Verdict::Closed,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    pub symbol: String,
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub excerpt: String,
    pub excerpt_sha256: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConditionResult {
    pub index: usize,
    pub kind: String,
    pub verdict: Verdict,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Delta {
    pub unclaimed_added: Vec<String>,
    pub unclaimed_removed: Vec<String>,
    pub files_added: Vec<String>,
    pub files_removed: Vec<String>,
    pub files_changed: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Certificate {
    pub warrant: u32,
    pub verdict: Verdict,
    pub base: Side,
    pub head: Side,
    pub contract_sha256: String,
    pub conditions: Vec<ConditionResult>,
    pub delta: Delta,
    pub unseen: Vec<UnseenEntry>,
    pub waived_unseen: Vec<String>,
    pub certificate_sha256: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Side {
    pub digest: String,
    pub files: usize,
    pub scanned_files: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnseenEntry {
    pub path: String,
    pub reason: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Canonicalization
// ─────────────────────────────────────────────────────────────────────────────

/// Serialize a certificate canonically: sorted key order is guaranteed by
/// serde struct order; arrays listed in the struct are already sorted by the
/// evaluator. No timestamps, no absolute paths.
pub fn canonical_json(cert: &Certificate) -> Result<String, String> {
    serde_json::to_string_pretty(cert).map_err(|e| format!("serialize certificate: {}", e))
}

/// SHA-256 of the canonical bytes of the certificate with
/// `certificate_sha256` emptied, so the hash covers everything else.
pub fn certificate_digest(cert: &mut Certificate) -> Result<(), String> {
    cert.certificate_sha256.clear();
    let bytes = serde_json::to_vec(cert).map_err(|e| format!("serialize certificate: {}", e))?;
    cert.certificate_sha256 = format!("sha256:{}", sha256_bytes(&bytes));
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Condition evaluation
// ─────────────────────────────────────────────────────────────────────────────

/// Whether `path` matches a contract path selector: exact/prefix, or `*suffix`.
pub fn path_matches(selector: &str, path: &str) -> bool {
    if let Some(star) = selector.find('*') {
        let prefix = &selector[..star];
        let suffix = &selector[star + 1..];
        return path.len() >= prefix.len() + suffix.len()
            && path.starts_with(prefix)
            && path.ends_with(suffix);
    }
    let sel = selector.trim_end_matches('/');
    if sel.is_empty() {
        return true;
    }
    path == sel || path.starts_with(&format!("{}/", sel))
}

fn scoped<'a>(scope: &'a [String], outer: &'a [String], path: &str) -> bool {
    if !outer.is_empty() && !outer.iter().any(|s| path_matches(s, path)) {
        return false;
    }
    scope.is_empty() || scope.iter().any(|s| path_matches(s, path))
}

fn evidence_from_hits(hits: &[&Hit]) -> Vec<Evidence> {
    hits.iter()
        .map(|h| Evidence {
            symbol: h.needle.clone(),
            file: h.path.clone(),
            line: h.line,
            col: h.col,
            excerpt: h.excerpt.clone(),
            excerpt_sha256: h.excerpt_sha256.clone(),
        })
        .collect()
}

fn hits_for<'a>(scan: &'a TreeScan, needle: &str) -> Vec<&'a Hit> {
    scan.hits.iter().filter(|h| h.needle == needle).collect()
}

fn is_whole_identifier(s: &str) -> bool {
    s.len() >= 2
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && s.as_bytes()[0].is_ascii_alphabetic()
}

/// Count top-level commas inside the first paren group on a definition line.
/// Returns `None` when the line has no paren group or it is unbalanced —
/// callers must report INDETERMINATE rather than guess.
pub fn count_params(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let open = bytes.iter().position(|&b| b == b'(')?;
    let mut depth = 0i32;
    // Angle depth covers generic type arguments (`HashMap<K, V>`), whose
    // commas are not parameter separators. `<` only opens when it follows an
    // identifier or `>`/`::` (a type position); `->` is excluded so the arrow
    // never decrements. This is a shape heuristic, and a line this rule
    // cannot read returns None (INDETERMINATE), never a guess.
    let mut angle = 0i32;
    let mut prev = b' ';
    let mut commas = 0usize;
    let mut non_ws = false;
    let mut saw_close = false;
    for &b in &bytes[open + 1..] {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' if depth == 0 && angle == 0 => {
                saw_close = true;
                break;
            }
            b')' | b']' | b'}' => depth -= 1,
            b'<' if depth == 0
                && (prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'>' || prev == b':') =>
            {
                angle += 1;
            }
            b'>' if depth == 0 && angle > 0 && prev != b'-' => angle -= 1,
            b',' if depth == 0 && angle == 0 => commas += 1,
            b if !b.is_ascii_whitespace() => non_ws = true,
            _ => {}
        }
        if !b.is_ascii_whitespace() {
            prev = b;
        }
    }
    if !saw_close {
        return None;
    }
    Some(if non_ws { commas + 1 } else { 0 })
}

/// Evaluate one condition against the head tree scan and the head index.
#[allow(clippy::too_many_arguments)]
fn eval_condition(
    idx: usize,
    cond: &Condition,
    contract_scope: &[String],
    base_scan: &TreeScan,
    head_scan: &TreeScan,
    base_defs: &[DefRow],
    head_defs: &[DefRow],
    head_root: &Path,
) -> ConditionResult {
    let mut result = ConditionResult {
        index: idx,
        kind: match cond {
            Condition::Rename { .. } => "rename",
            Condition::Remove { .. } => "remove",
            Condition::Add { .. } => "add",
            Condition::Signature { .. } => "signature",
            Condition::NoNewDead => "no_new_dead",
            Condition::Files { .. } => "files",
        }
        .to_string(),
        verdict: Verdict::Closed,
        detail: None,
        evidence: Vec::new(),
    };

    match cond {
        Condition::Rename { from, to, scope } => {
            if !is_whole_identifier(from) {
                result.verdict = Verdict::Indeterminate;
                result.detail = Some(format!("`from` is not a whole identifier: {:?}", from));
                return result;
            }
            let remaining: Vec<&Hit> = hits_for(head_scan, from)
                .into_iter()
                .filter(|h| scoped(scope, contract_scope, &h.path))
                .collect();
            let target_hits: Vec<&Hit> = hits_for(head_scan, to)
                .into_iter()
                .filter(|h| scoped(scope, contract_scope, &h.path))
                .collect();
            if !remaining.is_empty() {
                result.verdict = Verdict::Open;
                result.detail = Some(format!("{} remaining reference(s) to `{}`", remaining.len(), from));
                result.evidence = evidence_from_hits(&remaining);
            } else if target_hits.is_empty() {
                result.verdict = Verdict::Open;
                result.detail = Some(format!("`{}` does not appear anywhere in head", to));
            }
        }
        Condition::Remove { name, scope } => {
            if !is_whole_identifier(name) {
                result.verdict = Verdict::Indeterminate;
                result.detail = Some(format!("`name` is not a whole identifier: {:?}", name));
                return result;
            }
            let remaining: Vec<&Hit> = hits_for(head_scan, name)
                .into_iter()
                .filter(|h| scoped(scope, contract_scope, &h.path))
                .collect();
            if !remaining.is_empty() {
                result.verdict = Verdict::Open;
                result.detail = Some(format!("{} remaining reference(s) to `{}`", remaining.len(), name));
                result.evidence = evidence_from_hits(&remaining);
            }
        }
        Condition::Add { name } => {
            if !is_whole_identifier(name) {
                result.verdict = Verdict::Indeterminate;
                result.detail = Some(format!("`name` is not a whole identifier: {:?}", name));
                return result;
            }
            let stem = crate::stem_identifier(name);
            let defs: Vec<&DefRow> = head_defs
                .iter()
                .filter(|d| d.phrase == stem && is_token_at_line(head_root, &d.path, d.line, name))
                .collect();
            if defs.is_empty() {
                result.verdict = Verdict::Open;
                result.detail = Some(format!("no definition of `{}` found", name));
            }
        }
        Condition::Signature { name, arity } => {
            if !is_whole_identifier(name) {
                result.verdict = Verdict::Indeterminate;
                result.detail = Some(format!("`name` is not a whole identifier: {:?}", name));
                return result;
            }
            let stem = crate::stem_identifier(name);
            let defs: Vec<&DefRow> = head_defs
                .iter()
                .filter(|d| d.phrase == stem)
                .filter(|d| is_token_at_line(head_root, &d.path, d.line, name))
                .collect();
            if defs.is_empty() {
                result.verdict = Verdict::Open;
                result.detail = Some(format!("no definition of `{}` found", name));
                return result;
            }
            let mut offenders = Vec::new();
            let mut indeterminate = false;
            for d in &defs {
                match read_source_line(head_root, &d.path, d.line) {
                    Some(line) => match count_params(&line) {
                        Some(n) if n == *arity => {}
                        Some(n) => offenders.push((d, line, n)),
                        None => indeterminate = true,
                    },
                    None => indeterminate = true,
                }
            }
            if !offenders.is_empty() {
                result.verdict = Verdict::Open;
                result.detail = Some(format!(
                    "{} definition site(s) of `{}` do not have arity {}",
                    offenders.len(),
                    name,
                    arity
                ));
                result.evidence = offenders
                    .iter()
                    .map(|(d, line, n)| Evidence {
                        symbol: format!("{}(arity={})", name, n),
                        file: d.path.clone(),
                        line: d.line.max(0) as u32,
                        col: 1,
                        excerpt: line.clone(),
                        excerpt_sha256: format!("sha256:{}", sha256_bytes(line.as_bytes())),
                    })
                    .collect();
            } else if indeterminate {
                result.verdict = Verdict::Indeterminate;
                result.detail = Some(format!(
                    "cannot parse the parameter list of `{}` at one or more definition sites",
                    name
                ));
            }
        }
        Condition::NoNewDead => {
            // Definitions in changed files whose last use disappeared. Uses
            // are raw-token occurrences minus definition sites, counted over
            // the whole tree (so a surviving cross-file use keeps the
            // definition alive).
            let changed: BTreeSet<String> = changed_paths(base_scan, head_scan)
                .into_iter()
                .collect();
            let dead = new_dead_in(base_scan, head_scan, &changed, base_defs, head_defs);
            if !dead.is_empty() {
                result.verdict = Verdict::Open;
                result.detail = Some(format!("{} newly dead definition(s)", dead.len()));
                result.evidence = dead
                    .iter()
                    .map(|(phrase, path, line)| {
                        // A newly dead definition may never have been a scan
                        // needle, so read the line directly. One bounded read
                        // per dead symbol, and dead sets are small.
                        let excerpt = read_source_line(head_root, path, *line)
                            .unwrap_or_default();
                        Evidence {
                            symbol: phrase.clone(),
                            file: path.clone(),
                            line: (*line).max(0) as u32,
                            col: 1,
                            excerpt_sha256: format!(
                                "sha256:{}",
                                sha256_bytes(excerpt.as_bytes())
                            ),
                            excerpt,
                        }
                    })
                    .collect();
            }
        }
        Condition::Files { within } => {
            let changed = changed_paths(base_scan, head_scan);
            let offenders: Vec<String> = changed
                .iter()
                .filter(|p| !within.iter().any(|w| path_matches(w, p)))
                .cloned()
                .collect();
            if !offenders.is_empty() {
                result.verdict = Verdict::Open;
                result.detail = Some(format!("{} changed file(s) outside the allowed scope", offenders.len()));
                result.evidence = offenders
                    .iter()
                    .map(|p| Evidence {
                        symbol: p.clone(),
                        file: p.clone(),
                        line: 0,
                        col: 0,
                        excerpt: String::new(),
                        excerpt_sha256: String::new(),
                    })
                    .collect();
            }
        }
    }
    result
}

/// Does the head scan have `token` at `line` in `path`? Used to confirm that a
/// Does `token` appear as a whole identifier on the line the index calls a
/// definition site?
///
/// Reads the line rather than looking for a scan hit. Definition names used to
/// be added to the needle set to make this a hit lookup; that produced ~96k
/// hits per tree for a handful of useful ones, and with definitions now coming
/// from the scan there is no index to source the names from in the first place.
/// The question is about one line, so reading one line is the honest cost.
fn is_token_at_line(root: &Path, path: &str, line: i32, token: &str) -> bool {
    let Some(text) = read_source_line(root, path, line) else {
        return false;
    };
    let code = crate::structural::tokenizable_code(&text);
    crate::scan_identifiers(&code).iter().any(|t| t == token)
}

/// Files whose digest differs or that exist on only one side.
pub fn changed_paths(base: &TreeScan, head: &TreeScan) -> Vec<String> {
    let b: BTreeMap<&str, &str> = base
        .manifest
        .entries
        .iter()
        .map(|e| (e.path.as_str(), e.digest.as_str()))
        .collect();
    let h: BTreeMap<&str, &str> = head
        .manifest
        .entries
        .iter()
        .map(|e| (e.path.as_str(), e.digest.as_str()))
        .collect();
    let mut out: BTreeSet<String> = BTreeSet::new();
    for (p, d) in &h {
        match b.get(p) {
            Some(bd) if bd == d => {}
            _ => {
                out.insert((*p).to_string());
            }
        }
    }
    for p in b.keys() {
        if !h.contains_key(p) {
            out.insert((*p).to_string());
        }
    }
    out.into_iter().collect()
}

/// Newly dead definitions: a definition in a changed file whose stem had at
/// least one use in base and none in head. Uses = raw token occurrences minus
/// definition sites, counted across the whole tree, so a surviving cross-file
/// use keeps the definition alive.
///
/// Grammar-free and LLM-free: the tree scan is the token truth; the index
/// supplies definition sites.
fn new_dead_in(
    base: &TreeScan,
    head: &TreeScan,
    changed: &BTreeSet<String>,
    base_defs: &[DefRow],
    head_defs: &[DefRow],
) -> Vec<(String, String, i32)> {
    let base_sites = |stem: &str| base_defs.iter().filter(|d| d.phrase == stem).count() as u32;
    let head_sites = |stem: &str| head_defs.iter().filter(|d| d.phrase == stem).count() as u32;
    let base_uses = |stem: &str| {
        base.counts.get(stem).copied().unwrap_or(0).saturating_sub(base_sites(stem))
    };
    let head_uses = |stem: &str| {
        head.counts.get(stem).copied().unwrap_or(0).saturating_sub(head_sites(stem))
    };
    let mut out = Vec::new();
    for d in head_defs {
        // `changed` is accepted for signature stability and future scoping;
        // the definition's own file need not change — its last use can be
        // deleted in a different file.
        let _ = changed;
        let stem = d.phrase.as_str();
        if base_uses(stem) > 0 && head_uses(stem) == 0 {
            out.push((d.phrase.clone(), d.path.clone(), d.line));
        }
    }
    out.sort();
    out.dedup();
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Delta accounting
// ─────────────────────────────────────────────────────────────────────────────

/// Compute the delta between the two indexes restricted to changed paths, and
/// mark additions/removals claimed by the contract.
fn delta_accounting(
    contract: &Contract,
    base_scan: &TreeScan,
    head_scan: &TreeScan,
    base_defs: &BTreeSet<(String, String)>,
    head_defs: &BTreeSet<(String, String)>,
    changed: &BTreeSet<String>,
) -> Delta {
    let mut d = Delta::default();
    let base_changed: BTreeSet<(String, String)> = base_defs
        .iter()
        .filter(|(_, p)| changed.contains(p))
        .cloned()
        .collect();
    let head_changed: BTreeSet<(String, String)> = head_defs
        .iter()
        .filter(|(_, p)| changed.contains(p))
        .cloned()
        .collect();

    let removed: Vec<&(String, String)> = base_changed.difference(&head_changed).collect();
    let added: Vec<&(String, String)> = head_changed.difference(&base_changed).collect();

    // Claims: rename claims (stem(from) -> stem(to)); remove claims stem(name);
    // add claims stem(name).
    let mut claimed_removed: BTreeSet<String> = BTreeSet::new();
    let mut claimed_added: BTreeSet<String> = BTreeSet::new();
    for c in &contract.conditions {
        match c {
            Condition::Rename { from, to, .. } => {
                claimed_removed.insert(crate::stem_identifier(from));
                claimed_added.insert(crate::stem_identifier(to));
            }
            Condition::Remove { name, .. } => {
                claimed_removed.insert(crate::stem_identifier(name));
            }
            Condition::Add { name } => {
                claimed_added.insert(crate::stem_identifier(name));
            }
            _ => {}
        }
    }

    for (phrase, _path) in removed {
        if !claimed_removed.contains(phrase) {
            d.unclaimed_removed.push(phrase.clone());
        }
    }
    for (phrase, _path) in added {
        if !claimed_added.contains(phrase) {
            d.unclaimed_added.push(phrase.clone());
        }
    }
    d.unclaimed_added.sort();
    d.unclaimed_added.dedup();
    d.unclaimed_removed.sort();
    d.unclaimed_removed.dedup();

    let (added_files, removed_files, changed_files) = classify_manifest_changes(base_scan, head_scan);
    d.files_added = added_files;
    d.files_removed = removed_files;
    d.files_changed = changed_files;

    d
}

/// Split changed paths into added / removed / modified by manifest
/// membership. Names match the certificate fields; definition presence is
/// deliberately not used here (a new file with no definitions is still an
/// added file).
fn classify_manifest_changes(base: &TreeScan, head: &TreeScan) -> (Vec<String>, Vec<String>, Vec<String>) {
    let b: BTreeMap<&str, &str> = base
        .manifest
        .entries
        .iter()
        .map(|e| (e.path.as_str(), e.digest.as_str()))
        .collect();
    let h: BTreeMap<&str, &str> = head
        .manifest
        .entries
        .iter()
        .map(|e| (e.path.as_str(), e.digest.as_str()))
        .collect();
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    for (p, d) in &h {
        match b.get(p) {
            None => added.push((*p).to_string()),
            Some(bd) if bd == d => {}
            Some(_) => changed.push((*p).to_string()),
        }
    }
    for p in b.keys() {
        if !h.contains_key(p) {
            removed.push((*p).to_string());
        }
    }
    added.sort();
    removed.sort();
    changed.sort();
    (added, removed, changed)
}

// ─────────────────────────────────────────────────────────────────────────────
// Orchestration
// ─────────────────────────────────────────────────────────────────────────────

// ─────────────────────────────────────────────────────────────────────────────
// Scan cache
// ─────────────────────────────────────────────────────────────────────────────

/// Stable, order-independent identity of the needle set. `evaluate` builds
/// needles from a `BTreeSet`, so the order is already sorted; hashing the
/// sorted list keeps the key stable if that ever changes.
pub fn needles_key(needles: &[String]) -> String {
    let mut sorted: Vec<&str> = needles.iter().map(|s| s.as_str()).collect();
    sorted.sort_unstable();
    let mut h = Sha256::new();
    for n in sorted {
        h.update(n.as_bytes());
        h.update([0u8]);
    }
    hex(&h.finalize())
}

/// Cache key for one tree scan: content identity plus what was looked for.
///
/// `counts` is needle-independent, but `hits` are not, so the pair is keyed
/// together. A different contract on the same tree recomputes the scan; the
/// same contract on the same tree is free.
fn scan_cache_key(digest: &str, needles: &[String], exclude: &ExcludeSet) -> String {
    let mut h = Sha256::new();
    h.update(digest.as_bytes());
    h.update([0u8]);
    h.update(SCAN_FORMAT.to_le_bytes());
    h.update(INDEXER_FINGERPRINT.as_bytes());
    h.update([0u8]);
    h.update(needles_key(needles).as_bytes());
    h.update([0u8]);
    // The exclude set changes which files are scanned without changing a git
    // identity, so it is part of the key. Both containers are sorted, so the
    // key is order-independent.
    for e in &exclude.rel {
        h.update(b"f");
        h.update(e.as_bytes());
        h.update([0u8]);
    }
    for p in &exclude.prefixes {
        h.update(b"d");
        h.update(p.as_bytes());
        h.update([0u8]);
    }
    for a in &exclude.abs {
        h.update(b"a");
        h.update(a.to_string_lossy().as_bytes());
        h.update([0u8]);
    }
    hex(&h.finalize())
}

/// Load a cached `TreeScan`, or compute and store one.
///
/// The cache is advisory: any I/O, parse or schema problem falls back to a
/// fresh scan. It can only make the tool slower (a miss), never wrong.
pub fn scan_tree_cached(
    root: &Path,
    needles: &[String],
    exclude: &[PathBuf],
    cache_dir: Option<&Path>,
    // Identity of the tree, when the caller already knows it: a git commit id
    // for an immutable base, or the manifest digest for a working tree. With a
    // hint the cache is consulted before any file is read. Without one the
    // caller pays a manifest-only pass to derive the digest first.
    key_hint: Option<&str>,
) -> Result<TreeScan, String> {
    let Some(cache_dir) = cache_dir else {
        return scan_tree_excluding(root, needles, exclude);
    };

    let identity = match key_hint {
        Some(id) => id.to_string(),
        None => manifest_digest(root, exclude)?,
    };

    // Key excludes exactly as the scan applies them: root-relative, with
    // unreachable entries dropped and ignored directories collapsed. Keying a
    // wider set would turn every ignored build artefact into a cache miss.
    let root_raw = root.to_path_buf();
    let root_canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let excl = ExcludeSet::new(&root_raw, &root_canon, exclude);
    let key = scan_cache_key(&identity, needles, &excl);
    let path = cache_dir.join(format!("scan-{key}.json"));
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(scan) = serde_json::from_slice::<TreeScan>(&bytes) {
            return Ok(scan);
        }
    }

    let scan = scan_tree_excluding(root, needles, exclude)?;

    if std::fs::create_dir_all(cache_dir).is_ok() {
        if let Ok(bytes) = serde_json::to_vec(&scan) {
            let tmp = cache_dir.join(format!("scan-{key}.tmp.{}", std::process::id()));
            if std::fs::write(&tmp, &bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
    Ok(scan)
}

fn ser_counts<S: serde::Serializer>(
    v: &FxHashMap<String, u32>,
    s: S,
) -> Result<S::Ok, S::Error> {
    // Sorted so the cache file is byte-stable; the map itself never reaches
    // the certificate, so this is for reproducible cache bytes only.
    let mut pairs: Vec<(&String, &u32)> = v.iter().collect();
    pairs.sort_unstable();
    s.collect_seq(pairs.into_iter().map(|(k, v)| (k, *v)))
}

fn de_counts<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<FxHashMap<String, u32>, D::Error> {
    let v: Vec<(String, u32)> = Vec::deserialize(d)?;
    Ok(v.into_iter().collect())
}

// ─────────────────────────────────────────────────────────────────────────────
// Index cache (A5)
// ─────────────────────────────────────────────────────────────────────────────

/// Bump whenever anything that shapes stored index rows changes: the
/// classifier, the tokenizer, `stem_identifier`, the schema migration set, or
/// the occurrence builder. The cache key includes this, so a bump orphans
/// every stale entry instead of silently serving rows produced by different
/// code. This constant is the entire safety argument for reusing an index
/// built by an older binary.
pub const INDEXER_VERSION: u32 = 1;

/// On-disk shape of a cached `TreeScan`.
///
/// The scan cache stores a struct, not just index rows, so a field added to
/// `TreeScan` changes the meaning of an existing file. `defs` is required
/// (no serde default) so a missing field already fails to parse, but a future
/// field that IS defaulted would not, and an empty `defs` reads as every
/// contract being CLOSED. Bumping this constant on any shape change is cheap
/// insurance; the fingerprint cannot cover it because `warrant.rs` is not part
/// of the indexer source set.
pub const SCAN_FORMAT: u32 = 2;

/// Fingerprint of the indexer sources, computed by `build.rs` and baked into
/// the binary. This is the mechanical half of the cache-safety argument: any
/// edit to the classifier, lexer, extractor, occurrence builder, schema,
/// tokenizer or segment gating changes this value, so a cache entry produced
/// by different code can never be served — without anyone remembering to bump
/// a constant. `INDEXER_VERSION` stays as the coarse, human-visible marker.
pub const INDEXER_FINGERPRINT: &str = env!("RELIARY_INDEXER_FP");

fn classifier_mode() -> &'static str {
    if crate::segment::segment_enabled() { "island" } else { "legacy" }
}

/// Content-addressed cache key for one tree.
///
/// Everything that can change the *stored rows* is mixed in: the manifest
/// digest (the content), the schema version, the indexer version, and the
/// classifier mode. Path is deliberately absent — the same content at a
/// different root is the same index (which is why `index_revision_relative`
/// exists).
pub fn cache_key(manifest_digest: &str) -> String {
    let mut h = Sha256::new();
    h.update(manifest_digest.as_bytes());
    h.update([0u8]);
    h.update(crate::schema::SCHEMA_VERSION.to_le_bytes());
    h.update(INDEXER_VERSION.to_le_bytes());
    h.update([0u8]);
    h.update(INDEXER_FINGERPRINT.as_bytes());
    h.update([0u8]);
    h.update(classifier_mode().as_bytes());
    hex(&h.finalize())
}

/// Open a cached index for `root`, or build one.
///
/// The build is written to a temporary file and atomically renamed, so a
/// killed process cannot leave a half-written database under a valid key.
/// Returns the connection and whether it came from the cache.
pub fn open_or_build_cached_index(
    root: &Path,
    cache_dir: &Path,
    key: &str,
    exclude_rel: &BTreeSet<String>,
) -> Result<(Connection, bool), String> {
    let final_path = cache_dir.join(format!("{key}.sqlite"));
    if final_path.exists() {
        if let Ok(db) = Connection::open(&final_path) {
            let version: i32 = db
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap_or(-1);
            if version == crate::schema::SCHEMA_VERSION {
                return Ok((db, true));
            }
        }
        // Wrong schema or unreadable: rebuild rather than serve it.
        let _ = std::fs::remove_file(&final_path);
    }

    std::fs::create_dir_all(cache_dir)
        .map_err(|e| format!("create cache dir {}: {}", cache_dir.display(), e))?;
    let tmp_path = cache_dir.join(format!("{key}.tmp.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp_path);
    // Build, then close before renaming so no open handle follows the file.
    crate::struct_diff::index_revision_relative_excluding(
        root.to_str().ok_or("root is not valid UTF-8")?,
        tmp_path.to_str().ok_or("cache path is not valid UTF-8")?,
        exclude_rel,
    )?;
    // fsync the file so the rename cannot expose an empty file after a crash.
    if let Ok(f) = std::fs::File::open(&tmp_path) {
        let _ = f.sync_all();
    }
    std::fs::rename(&tmp_path, &final_path)
        .map_err(|e| format!("commit cache {}: {}", final_path.display(), e))?;
    let db = Connection::open(&final_path)
        .map_err(|e| format!("open {}: {}", final_path.display(), e))?;
    Ok((db, false))
}

/// Keep the newest `keep` entries and drop anything beyond `max_bytes`.
/// Called after every run; bounded growth, no manual cleanup needed.
pub fn prune_cache(cache_dir: &Path, keep: usize, max_bytes: u64) {
    let Ok(read) = std::fs::read_dir(cache_dir) else { return };
    let mut entries: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    for e in read.flatten() {
        let p = e.path();
        // Index databases and scan caches are both regenerable; leave anything
        // else in the directory alone.
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let is_cache = p.extension().and_then(|x| x.to_str()) == Some("sqlite")
            || name.starts_with("scan-")
            || name.contains(".tmp.");
        if !is_cache {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        entries.push((mtime, meta.len(), p));
    }
    entries.sort_by_key(|e| std::cmp::Reverse(e.0)); // newest first
    let mut total = 0u64;
    for (i, (_mtime, len, path)) in entries.iter().enumerate() {
        total += *len;
        let over_count = i >= keep;
        let over_size = total > max_bytes;
        if over_count || over_size {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Read one source line (0-based `line`) from `root/rel`.
///
/// Used for evidence excerpts on paths where the line is not guaranteed to be
/// a scan hit (newly dead definitions). One bounded read; the file is small
/// by construction because it is in the manifest.
pub fn read_source_line(root: &Path, rel: &str, line: i32) -> Option<String> {
    let text = std::fs::read_to_string(root.join(rel)).ok()?;
    text.lines().nth(line.max(0) as usize).map(|s| s.to_string())
}

pub struct WarrantInput<'a> {
    pub base_root: &'a Path,
    pub head_root: &'a Path,
    /// Definitions from the scan, or from an index when the island classifier
    /// is active (`RELIARY_SEGMENTER=1`), which the scan does not reproduce.
    /// `None` means "use the scan's own defs".
    pub base_defs: Option<&'a [DefRow]>,
    pub head_defs: Option<&'a [DefRow]>,
    pub contract: &'a Contract,
    /// Paths excluded from both scans (contract file, certificate output).
    pub exclude: &'a [PathBuf],
    /// Where scan results may be cached. `None` disables the scan cache.
    pub cache_dir: Option<&'a Path>,
    /// Immutable identity of the base tree (a git commit id, or a manifest
    /// digest for a plain directory). Keys the base scan cache.
    pub base_identity: String,
    /// Identity of the head tree: its manifest digest.
    pub head_identity: String,
}

/// Evaluate a contract against two indexed trees and produce a certificate.
pub fn evaluate(input: WarrantInput<'_>) -> Result<Certificate, String> {
    if input.contract.version != 1 {
        return Err(format!(
            "unsupported contract version {} (expected 1)",
            input.contract.version
        ));
    }

    // Collect every identifier the contract mentions, plus every definition
    // name (so delta/unseen stay honest), then scan both trees once.
    let mut needles: BTreeSet<String> = BTreeSet::new();
    for c in &input.contract.conditions {
        match c {
            Condition::Rename { from, to, .. } => {
                needles.insert(from.clone());
                needles.insert(to.clone());
            }
            Condition::Remove { name, .. } | Condition::Add { name } => {
                needles.insert(name.clone());
            }
            Condition::Signature { name, .. } => {
                needles.insert(name.clone());
            }
            _ => {}
        }
    }

    let needle_vec: Vec<String> = needles.into_iter().collect();

    let timing = std::env::var("RELIARY_WARRANT_TIMING").is_ok();
    let t = std::time::Instant::now();
    let base_scan = scan_tree_cached(
        input.base_root,
        &needle_vec,
        input.exclude,
        input.cache_dir,
        Some(&input.base_identity),
    )?;
    if timing {
        eprintln!(
            "[warrant] scan-base: {:?} files={} hits={}",
            t.elapsed(),
            base_scan.manifest.files,
            base_scan.hits.len()
        );
    }
    let t = std::time::Instant::now();
    let head_scan = scan_tree_cached(
        input.head_root,
        &needle_vec,
        input.exclude,
        input.cache_dir,
        Some(&input.head_identity),
    )?;
    // Definitions come from the tree scan. The scan reads and classifies every
    // byte anyway (counts and hits are derived from the same pass), so a
    // separate SQLite index bought nothing but a second pipeline to keep in
    // step. Equivalence with `def_rows` is asserted by
    // `scan_defs_match_index_defs`; the index path remains available for the
    // island classifier, which the scan does not reproduce.
    let base_defs_all: Vec<DefRow> = match input.base_defs {
        Some(d) => d.to_vec(),
        None => base_scan.defs.clone(),
    };
    let head_defs_all: Vec<DefRow> = match input.head_defs {
        Some(d) => d.to_vec(),
        None => head_scan.defs.clone(),
    };
    if timing {
        eprintln!(
            "[warrant] scan-head: {:?} files={} hits={}",
            t.elapsed(),
            head_scan.manifest.files,
            head_scan.hits.len()
        );
    }

    // Evaluate conditions.
    let t_cond = std::time::Instant::now();
    let mut results = Vec::with_capacity(input.contract.conditions.len());
    for (i, c) in input.contract.conditions.iter().enumerate() {
        results.push(eval_condition(
            i,
            c,
            &input.contract.scope,
            &base_scan,
            &head_scan,
            &base_defs_all,
            &head_defs_all,
            input.head_root,
        ));
    }

    if timing {
        eprintln!("[warrant] conditions: {:?}", t_cond.elapsed());
    }
    let t_delta = std::time::Instant::now();
    // Delta accounting over changed paths.
    let changed: BTreeSet<String> = changed_paths(&base_scan, &head_scan).into_iter().collect();
    let base_defs = defs_in_paths(&base_defs_all, &changed);
    let head_defs = defs_in_paths(&head_defs_all, &changed);
    let delta = delta_accounting(
        input.contract,
        &base_scan,
        &head_scan,
        &base_defs,
        &head_defs,
        &changed,
    );

    if timing {
        eprintln!("[warrant] delta: {:?}", t_delta.elapsed());
    }
    // Verdict aggregation.
    let mut verdict = Verdict::Closed;
    for r in &results {
        verdict = verdict.worse(r.verdict);
    }
    if !delta.unclaimed_removed.is_empty() {
        verdict = verdict.worse(Verdict::Open);
    }

    // Unseen region: head files that were walked but not scanned, plus
    // scanned files that produced no definition rows (empty of symbols).
    let head_indexed: BTreeSet<&str> = head_defs_all.iter().map(|d| d.path.as_str()).collect();
    let mut unseen: Vec<UnseenEntry> = head_scan
        .unseen
        .iter()
        .map(|(p, r)| UnseenEntry { path: p.clone(), reason: r.clone() })
        .collect();
    for e in &head_scan.manifest.entries {
        if e.scanned && !head_indexed.contains(e.path.as_str()) && !is_known_data(&e.path) {
            unseen.push(UnseenEntry {
                path: e.path.clone(),
                reason: "no-definitions".to_string(),
            });
        }
    }
    unseen.sort_by(|a, b| a.path.cmp(&b.path));
    unseen.dedup_by(|a, b| a.path == b.path);

    let mut waived_unseen: Vec<String> = Vec::new();
    let mut active_unseen: Vec<UnseenEntry> = Vec::new();
    for u in unseen {
        if input.contract.allow_unseen.iter().any(|s| path_matches(s, &u.path)) {
            waived_unseen.push(u.path);
        } else {
            active_unseen.push(u);
        }
    }

    let must_escalate = input.contract.strict_unseen
        || results.iter().any(|r| r.verdict == Verdict::Indeterminate);
    let unseen_open = input.contract.strict_unseen && !active_unseen.is_empty();
    if unseen_open {
        verdict = verdict.worse(Verdict::Indeterminate);
    }
    if must_escalate && !active_unseen.is_empty() && verdict == Verdict::Closed {
        // A closed verdict with an unseen region is only honest when the
        // contract accepts it; otherwise report the region as indeterminate.
        verdict = verdict.worse(Verdict::Indeterminate);
    }

    let mut cert = Certificate {
        warrant: 1,
        verdict,
        base: Side {
            digest: base_scan.manifest.digest.clone(),
            files: base_scan.manifest.files,
            scanned_files: base_scan.manifest.scanned_files,
        },
        head: Side {
            digest: head_scan.manifest.digest.clone(),
            files: head_scan.manifest.files,
            scanned_files: head_scan.manifest.scanned_files,
        },
        contract_sha256: format!(
            "sha256:{}",
            sha256_bytes(
                serde_json::to_vec(input.contract)
                    .map_err(|e| format!("serialize contract: {}", e))?
                    .as_slice()
            )
        ),
        conditions: results,
        delta,
        unseen: active_unseen,
        waived_unseen,
        certificate_sha256: String::new(),
    };
    certificate_digest(&mut cert)?;
    Ok(cert)
}

/// Data formats we never treat as "unseen source": the index deliberately
/// skips them, and their absence of definitions is not a coverage gap.
fn is_known_data(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "toml" | "lock" | "json" | "yaml" | "yml" | "ini" | "csv" | "svg" | "md" | "mdx"
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Hashing helpers
// ─────────────────────────────────────────────────────────────────────────────

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, content).unwrap();
    }

    fn needles(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn manifest_is_deterministic_and_content_addressed() {
        let d = tmp();
        write(d.path(), "a.rs", "fn a() {}\n");
        write(d.path(), "b/c.py", "def c():\n    pass\n");
        let s1 = scan_tree(d.path(), &needles(&["a"])).unwrap();
        let s2 = scan_tree(d.path(), &needles(&["a"])).unwrap();
        assert_eq!(s1.manifest.digest, s2.manifest.digest);
        write(d.path(), "a.rs", "fn a() { let x = 1; }\n");
        let s3 = scan_tree(d.path(), &needles(&["a"])).unwrap();
        assert_ne!(s1.manifest.digest, s3.manifest.digest, "content change must change digest");
    }

    #[test]
    fn raw_scan_is_case_sensitive_and_whole_token() {
        let d = tmp();
        write(d.path(), "x.rs", "fn handling() { handled(); Handle(); }\n");
        let scan = scan_tree(d.path(), &needles(&["handling", "handled", "Handle"])).unwrap();
        // Whole-token scanning: three distinct tokens, no substring bleed.
        assert_eq!(hits_for(&scan, "handling").len(), 1);
        assert_eq!(hits_for(&scan, "handled").len(), 1);
        assert_eq!(hits_for(&scan, "Handle").len(), 1);
        // Stem collision negative control, verified against the shipped
        // stemmer: `handling` and `handled` both reduce to `handl`. Raw
        // scanning must still distinguish them.
        assert_eq!(
            crate::stem_identifier("handling"),
            crate::stem_identifier("handled"),
            "this pair is the stem-collision fixture; if the stemmer changes, pick a new pair"
        );
    }

    /// Serial and parallel scans must produce identical bytes. The merge
    /// sorts every collection, so scheduling cannot reach the output — this
    /// test pins it rather than trusting it.
    #[test]
    fn parallel_scan_matches_serial_output() {
        let d = tmp();
        for i in 0..40 {
            write(
                d.path(),
                &format!("src/f{i:02}.rs"),
                &format!("pub fn f{i}() {{ f{}(); }}\n", (i + 1) % 40),
            );
        }
        let ns = needles(&["f0", "f7", "f39"]);
        std::env::set_var("RELIARY_WARRANT_THREADS", "1");
        let serial = scan_tree(d.path(), &ns).unwrap();
        std::env::set_var("RELIARY_WARRANT_THREADS", "8");
        let parallel = scan_tree(d.path(), &ns).unwrap();
        std::env::remove_var("RELIARY_WARRANT_THREADS");

        assert_eq!(serial.manifest.digest, parallel.manifest.digest);
        assert_eq!(serial.hits.len(), parallel.hits.len());
        assert_eq!(
            serial.hits.iter().map(|h| (&h.needle, &h.path, h.line, h.col)).collect::<Vec<_>>(),
            parallel.hits.iter().map(|h| (&h.needle, &h.path, h.line, h.col)).collect::<Vec<_>>()
        );
        let mut a: Vec<_> = serial.counts.iter().collect();
        let mut b: Vec<_> = parallel.counts.iter().collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }

    /// Cache key stability and sensitivity: same content + same code = same
    /// key; the classifier mode must change it, because it changes stored rows.
    #[test]
    fn cache_key_is_content_and_indexer_addressed() {
        let k1 = cache_key("sha256:abc");
        let k2 = cache_key("sha256:abc");
        assert_eq!(k1, k2, "same inputs must produce the same key");
        assert_ne!(k1, cache_key("sha256:abd"), "content must change the key");
        assert_eq!(k1.len(), 64, "hex sha256");
    }

    /// The fingerprint must be a real 64-hex digest and must participate in
    /// the cache key. Printed so CI logs record which fingerprint a build
    /// carries; verified by a negative control during development (edit a
    /// source file in SOURCES -> the value changes).
    #[test]
    fn indexer_fingerprint_is_a_real_digest_and_keys_the_cache() {
        eprintln!("indexer fingerprint = {}", INDEXER_FINGERPRINT);
        assert_eq!(
            INDEXER_FINGERPRINT.len(),
            64,
            "build.rs must emit a sha256 hex digest, got {:?}",
            INDEXER_FINGERPRINT
        );
        assert!(
            INDEXER_FINGERPRINT.bytes().all(|b| b.is_ascii_hexdigit()),
            "fingerprint must be hex"
        );
        let k = cache_key("sha256:abc");
        let mut other = Sha256::new();
        other.update("sha256:abc");
        other.update([0u8]);
        other.update(crate::schema::SCHEMA_VERSION.to_le_bytes());
        other.update(INDEXER_VERSION.to_le_bytes());
        other.update([0u8]);
        other.update(b"0000000000000000000000000000000000000000000000000000000000000000");
        other.update([0u8]);
        other.update(classifier_mode().as_bytes());
        assert_ne!(k, hex(&other.finalize()), "fingerprint must change the key");
    }

    /// Scan-cache entries must be pruned with the indexes, otherwise a long
    /// session accumulates one JSON per tree state, unbounded.
    /// Exclusions must change the scan key, and must do so order-independently
    /// and without depending on a path's absolute prefix. Ignored directories
    /// collapse to one prefix; unreachable ones (a dot-dir) are dropped, because
    /// they cannot change what is scanned and must not invalidate a cache.
    #[test]
    fn scan_key_tracks_excludes_that_matter() {
        let d = tmp();
        let root = d.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let none = ExcludeSet::new(root, root, &[]);
        let one = ExcludeSet::new(root, root, &[root.join("src/a.rs")]);
        let two = ExcludeSet::new(root, root, &[root.join("src/b.rs")]);
        // Not a name in SKIP_DIRS: excluding a directory the walker never
        // enters (like `target/`) is correctly a no-op, so use a real one.
        std::fs::create_dir_all(root.join("generated")).unwrap();
        let dir = ExcludeSet::new(root, root, &[root.join("generated/")]);
        let dot = ExcludeSet::new(root, root, &[root.join(".reliary/index.sqlite")]);

        assert_ne!(
            scan_cache_key("m", &[], &none),
            scan_cache_key("m", &[], &one),
            "excluding a file must change the key"
        );
        assert_ne!(
            scan_cache_key("m", &[], &one),
            scan_cache_key("m", &[], &two),
            "different files must produce different keys"
        );
        assert_ne!(
            scan_cache_key("m", &[], &none),
            scan_cache_key("m", &[], &dir),
            "an ignored directory must change the key"
        );
        assert_eq!(
            scan_cache_key("m", &[], &none),
            scan_cache_key("m", &[], &dot),
            "an exclude the walker never reaches must not invalidate the cache"
        );
        assert_eq!(
            scan_cache_key("m", &[], &one),
            scan_cache_key("m", &[], &one.clone()),
            "the key must be stable"
        );
    }

    /// A cached scan that predates the `defs` field must be treated as a miss,
    /// not deserialized into an empty def set. An empty set means every
    /// contract reads CLOSED, which is the one failure mode that looks like
    /// success. `defs` is deliberately not `#[serde(default)]`; this pins it.
    #[test]
    fn stale_scan_cache_without_defs_is_rejected() {
        let stale = r#"{
            "manifest": {"entries": [], "digest": "d", "files": 0, "scanned_files": 0},
            "hits": [],
            "counts": [],
            "unseen": []
        }"#;
        let parsed = serde_json::from_str::<TreeScan>(stale);
        assert!(
            parsed.is_err(),
            "a scan cache without `defs` must not parse — otherwise every \
             contract silently reads CLOSED"
        );
    }

    #[test]
    fn prune_cache_also_removes_scan_entries() {
        let d = tmp();
        let dir = d.path().join("cache");
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..5 {
            std::fs::write(dir.join(format!("scan-k{i}.json")), b"{}").unwrap();
        }
        std::fs::write(dir.join("keep.txt"), b"not a cache").unwrap();
        prune_cache(&dir, 1, 1024 * 1024);
        let mut scans: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("scan-"))
            .collect();
        scans.sort();
        assert_eq!(scans.len(), 1, "only the newest scan entry survives");
        assert!(
            dir.join("keep.txt").exists(),
            "unrelated files must not be pruned"
        );
    }

    #[test]
    fn prune_cache_keeps_newest_and_respects_the_size_cap() {
        let d = tmp();
        let dir = d.path().join("cache");
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..6 {
            let p = dir.join(format!("k{i}.sqlite"));
            std::fs::write(&p, vec![0u8; 1000]).unwrap();
            // Distinct mtimes: newest is k5.
            let t = std::time::SystemTime::now() - std::time::Duration::from_secs(60 - i as u64);
            let f = std::fs::File::options().write(true).open(&p).unwrap();
            let _ = f.set_modified(t);
        }
        prune_cache(&dir, 3, 1024 * 1024);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        left.sort();
        assert_eq!(left, vec!["k3.sqlite", "k4.sqlite", "k5.sqlite"]);
    }

    #[test]
    fn scan_masks_strings_and_comments() {
        let d = tmp();
        write(
            d.path(),
            "x.rs",
            "fn real() {}\n// mentions ghost_old\nlet s = \"ghost_old\";\n",
        );
        let scan = scan_tree(d.path(), &needles(&["ghost_old"])).unwrap();
        assert!(
            scan.hits.is_empty(),
            "a name in a comment or string must not count as a reference: {:?}",
            scan.hits
        );
    }

    #[test]
    fn scan_masks_python_comments_and_triple_quotes() {
        let d = tmp();
        write(
            d.path(),
            "x.py",
            "def real():\n    pass\n# ghost in comment\n\"\"\"ghost in docstring\"\"\"\n",
        );
        let scan = scan_tree(d.path(), &needles(&["ghost"])).unwrap();
        assert!(scan.hits.is_empty(), "python comment/docstring must be masked");
    }

    #[test]
    fn delta_rename_claims_removed_and_added() {
        let contract = Contract {
            version: 1,
            scope: vec![],
            strict_unseen: false,
            conditions: vec![Condition::Rename {
                from: "old_name".into(),
                to: "new_name".into(),
                scope: vec![],
            }],
            allow_unseen: vec![],
        };
        let base: BTreeSet<(String, String)> =
            [("old_name".to_string(), "src/a.rs".to_string())].into();
        let head: BTreeSet<(String, String)> =
            [("new_name".to_string(), "src/a.rs".to_string())].into();
        let changed: BTreeSet<String> = ["src/a.rs".to_string()].into();
        let empty = TreeScan::default();
        let d = delta_accounting(&contract, &empty, &empty, &base, &head, &changed);
        assert!(d.unclaimed_removed.is_empty());
        assert!(d.unclaimed_added.is_empty());
    }

    #[test]
    fn delta_unclaimed_removal_is_reported() {
        let contract = Contract {
            version: 1,
            scope: vec![],
            strict_unseen: false,
            conditions: vec![],
            allow_unseen: vec![],
        };
        let base: BTreeSet<(String, String)> =
            [("deleted_fn".to_string(), "src/a.rs".to_string())].into();
        let head: BTreeSet<(String, String)> = BTreeSet::new();
        let changed: BTreeSet<String> = ["src/a.rs".to_string()].into();
        let empty = TreeScan::default();
        let d = delta_accounting(&contract, &empty, &empty, &base, &head, &changed);
        assert_eq!(d.unclaimed_removed, vec!["deleted_fn".to_string()]);
    }

    #[test]
    fn count_params_counts_top_level_commas_only() {
        assert_eq!(count_params("fn f() {}"), Some(0));
        assert_eq!(count_params("fn f(a) {}"), Some(1));
        assert_eq!(count_params("fn f(a: HashMap<K, V>, b) {}"), Some(2));
        assert_eq!(count_params("fn f(a: (u8, u8), b: [u8; 2]) {}"), Some(2));
        assert_eq!(count_params("fn f(a"), None, "unbalanced must be indeterminate");
        assert_eq!(count_params("fn f"), None);
    }

    #[test]
    fn path_selectors() {
        assert!(path_matches("src/", "src/a/b.rs"));
        assert!(path_matches("src/a/b.rs", "src/a/b.rs"));
        assert!(!path_matches("src/a", "src/ab/c.rs"));
        assert!(path_matches("*.lock", "vendor/x.lock"));
        assert!(path_matches("", "anything"));
    }

    // ── Scan-derived definitions must equal index-derived definitions ─────
    //
    // The whole pivot rests on this. `warrant` used to read definitions from a
    // freshly built SQLite index; it now derives them in the tree scan. If the
    // two disagree by even one row, a certificate can differ, so the equality
    // is asserted against a real index rather than reasoned about.
    //
    // The fixture deliberately includes the shapes where the two pipelines are
    // most likely to drift: a nested definition, a macro body, a comment that
    // mentions a name, a noise-keyword definition (`fn new`), a markdown file
    // whose fenced code passes the source gate, a JSON file (deny-list), a
    // definition-free file, and a Python file with docstrings.

    fn scan_defs_of(root: &Path) -> Vec<DefRow> {
        let mut d = scan_tree_excluding(root, &[], &[]).unwrap().defs;
        d.sort();
        d
    }

    fn index_defs_of(root: &Path) -> Vec<DefRow> {
        let db_path = root.join(".reliary/probe.sqlite");
        std::fs::create_dir_all(root.join(".reliary")).unwrap();
        let db = crate::struct_diff::index_revision(
            root.to_str().unwrap(),
            db_path.to_str().unwrap(),
        )
        .unwrap();
        let mut d = def_rows(&db, root).unwrap();
        d.sort();
        d
    }

    #[test]
    fn scan_defs_match_index_defs() {
        let d = tmp();
        let root = d.path().join("corpus");
        fs::create_dir_all(&root).unwrap();
        write(&root, "src/lib.rs", "#![allow(dead_code)]
\
pub fn outer(x: i32) -> i32 {
\
    fn nested(y: i32) -> i32 { y + 1 }
\
    // inner_ghost is only mentioned here
\
    nested(x)
\
}

\
macro_rules! shout {
\
    ($name:ident) => { fn $name() {} };
\
}

\
pub struct Thing { pub field: i32 }
impl Thing {
\
    pub fn new() -> Self { Thing { field: 0 } }
\
}
\
pub type Alias = Thing;
");
        write(&root, "src/empty.rs", "// nothing defined here
");
        write(&root, "docs/guide.md", "# Guide

```rust
pub fn in_fence() {}
```

Prose with `inline_code`.
");
        write(&root, "data/config.json", "{\n  \"fn_like\": \"pub fn nope() {}\"\n}\n");
        write(&root, "py/mod.py", "\"\"\"Module docstring mentioning ghost_fn.\"\"\"\n\nclass Widget:\n    def render(self):\n        pass\n\ndef free_function(a, b):\n    return a + b\n");

        let index = index_defs_of(&root);
        let scan = scan_defs_of(&root);
        assert_eq!(
            scan, index,
            "scan-derived defs must equal index-derived defs\n  only in scan:  {:?}\n  only in index: {:?}",
            scan.iter().filter(|d| !index.contains(d)).collect::<Vec<_>>(),
            index.iter().filter(|d| !scan.contains(d)).collect::<Vec<_>>()
        );
        // Non-vacuity: the fixture really produces definitions. Phrases are
        // stems, so `outer` is stored as `out` and `nested` as `nest`.
        assert!(scan.iter().any(|d| d.phrase == "out"), "outer missing: {:?}", scan);
        assert!(scan.iter().any(|d| d.phrase == "nest"), "nested missing");
        assert!(scan.iter().any(|d| d.phrase == "widget"), "widget missing");
        assert!(scan.iter().any(|d| d.phrase == "shout"), "macro name missing");
        // And the deny-listed file contributes nothing.
        assert!(!scan.iter().any(|d| d.path.ends_with("config.json")));
        // Both sides must exclude documentation. Equality alone cannot catch a
        // regression where *both* pipelines start admitting markdown again, so
        // the exclusion is asserted directly.
        assert!(
            !scan.iter().any(|d| d.path.ends_with(".md")),
            "scan must not define symbols in documentation: {:?}",
            scan.iter().filter(|d| d.path.ends_with(".md")).collect::<Vec<_>>()
        );
        assert!(
            !index.iter().any(|d| d.path.ends_with(".md")),
            "index must not define symbols in documentation: {:?}",
            index.iter().filter(|d| d.path.ends_with(".md")).collect::<Vec<_>>()
        );
    }

    /// Real-corpus gate. Run with:
    ///   RELIARY_WARRANT_GATE=/abs/path cargo test scan_defs_real_corpus -- --nocapture
    /// Skips when unset so CI does not depend on a local checkout.
    #[test]
    fn scan_defs_real_corpus() {
        let Ok(root) = std::env::var("RELIARY_WARRANT_GATE") else {
            return;
        };
        let root = PathBuf::from(root);
        let index = index_defs_of(&root);
        let scan = scan_defs_of(&root);
        let only_scan: Vec<&DefRow> = scan.iter().filter(|d| !index.contains(d)).collect();
        let only_index: Vec<&DefRow> = index.iter().filter(|d| !scan.contains(d)).collect();
        eprintln!(
            "gate {}: scan={} index={} only_scan={} only_index={}",
            root.display(), scan.len(), index.len(), only_scan.len(), only_index.len()
        );
        for d in only_scan.iter().take(15) { eprintln!("  ONLY SCAN  {}:{} {}", d.path, d.line, d.phrase); }
        for d in only_index.iter().take(15) { eprintln!("  ONLY INDEX {}:{} {}", d.path, d.line, d.phrase); }
        assert_eq!(scan, index, "scan and index defs diverged on a real corpus");
    }

    /// Admission gates in the scan must match the indexer's. Each case here is
    /// one gate, and each is chosen so that removing that gate changes the
    /// answer (a gate whose removal changes nothing would be untested):
    ///
    /// - `codedata.json` holds source-shaped text under a deny-listed
    ///   extension. The indexer skips it by extension before reading; the scan
    ///   must skip it too, even though the content passes `is_source_like`.
    /// - `latin1.rs` is a real definition in a non-UTF-8 file. The indexer
    ///   reads with `read_to_string` and never indexes it; the scan tolerates
    ///   lossy UTF-8 for counts, so it must refuse defs here explicitly.
    ///
    /// A same-size edit to a binary file must be visible as a change.
    ///
    /// The manifest used to store `size:N` for binary files, so flipping a byte
    /// left the digest identical and `changed_paths` reported nothing — a
    /// contract could read CLOSED with a changed binary in the tree.
    #[test]
    fn binary_content_change_changes_the_manifest() {
        let d = tmp();
        let root = d.path().join("corpus");
        fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("blob.bin"), vec![0u8; 64]).unwrap();
        let before = scan_tree_excluding(&root, &[], &[]).unwrap();
        std::fs::write(root.join("blob.bin"), vec![1u8; 64]).unwrap();
        let after = scan_tree_excluding(&root, &[], &[]).unwrap();
        assert_eq!(before.manifest.files, after.manifest.files, "same file count");
        assert_ne!(
            before.manifest.digest, after.manifest.digest,
            "a same-size binary edit must change the tree digest"
        );
        assert!(
            !changed_paths(&before, &after).is_empty(),
            "changed_paths must report the binary edit"
        );
        assert!(after.manifest.entries.iter().all(|e| !e.scanned));
    }

    /// The scan digest and the identity pass must agree for the same tree.
    /// They diverged whenever a binary file existed, because the scan used
    /// `size:N` while `manifest_digest` hashed the content.
    #[test]
    fn scan_and_manifest_digests_agree_with_binaries_present() {
        let d = tmp();
        let root = d.path().join("corpus");
        fs::create_dir_all(&root).unwrap();
        write(&root, "src/a.rs", "pub fn alpha() {}\n");
        std::fs::write(root.join("blob.bin"), vec![7u8; 32]).unwrap();
        let scan = scan_tree_excluding(&root, &[], &[]).unwrap();
        let digest = manifest_digest(&root, &[]).unwrap();
        assert_eq!(scan.manifest.digest, digest);
    }

    #[test]
    fn scan_defs_admission_matches_indexer() {
        let d = tmp();
        let root = d.path().join("corpus");
        fs::create_dir_all(&root).unwrap();
        write(&root, "data/codedata.json", "pub fn ghost_json() {}\n");
        write(&root, "src/latin1.rs", "pub fn ghost_latin1() {}\n");
        // Append a byte that is not valid UTF-8, so read_to_string fails.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(root.join("src/latin1.rs"))
                .unwrap();
            f.write_all(&[0xff, 0xfe, b'\n']).unwrap();
        }

        let scan = scan_defs_of(&root);
        let index = index_defs_of(&root);
        assert_eq!(
            scan, index,
            "scan defs must match the index, whose admission chain is the reference"
        );
        assert!(
            !scan.iter().any(|d| d.path.contains("codedata.json")),
            "deny-listed extension must not contribute definitions: {:?}",
            scan
        );
        assert!(
            !scan.iter().any(|d| d.path.contains("latin1.rs")),
            "non-UTF-8 file must not contribute definitions: {:?}",
            scan
        );
        // Prove the fixture would produce defs without those two gates, so the
        // assertions above are not passing by accident.
        assert!(
            crate::lazy_occurrence::is_source_like("pub fn ghost_json() {}\n"),
            "the deny-list case must be source-like, else it tests nothing"
        );
    }

    // ── End-to-end: contract evaluation on two indexed trees ──────────────

    struct Tree {
        dir: tempfile::TempDir,
        /// Definitions as the index derives them. Kept so every end-to-end
        /// test exercises the index agreement as well as the contract.
        index_defs: Vec<DefRow>,
    }

    impl Tree {
        fn root(&self) -> PathBuf {
            self.dir.path().join("corpus")
        }
    }

    fn build_tree(files: &[(&str, &str)]) -> Tree {
        let dir = tmp();
        // tempfile::tempdir() names the dir `.tmpXXXX`; index_directory skips
        // dot-prefixed entries at every depth, INCLUDING the root, so indexing
        // the tempdir directly yields an empty index. Use a plain subdir.
        let root = dir.path().join("corpus");
        fs::create_dir_all(&root).unwrap();
        for (p, c) in files {
            write(&root, p, c);
        }
        let db_path = root.join(".reliary/index.sqlite");
        fs::create_dir_all(root.join(".reliary")).unwrap();
        // The index is no longer part of warrant's path, but these tests were
        // written against it. Building one anyway proves the scan-derived
        // definitions agree with the index on every case here — the same
        // guarantee `scan_defs_match_index_defs` asserts on larger corpora.
        let db = crate::struct_diff::index_revision(
            root.to_str().unwrap(),
            db_path.to_str().unwrap(),
        )
        .unwrap();
        let index_defs = def_rows(&db, &root).unwrap();
        // Leak the connection deliberately: dropping it would close the file,
        // and the tree outlives this function. Only the defs are needed.
        std::mem::forget(db);
        Tree { dir, index_defs }
    }

    fn eval(base: &Tree, head: &Tree, contract: Contract) -> Certificate {
        evaluate(WarrantInput {
            base_root: &base.root(),
            head_root: &head.root(),
            base_defs: Some(&base.index_defs),
            head_defs: Some(&head.index_defs),
            contract: &contract,
            exclude: &[],
            cache_dir: None,
            base_identity: String::new(),
            head_identity: String::new(),
        })
        .unwrap()
    }

    fn base_contract() -> Contract {
        Contract {
            version: 1,
            scope: vec![],
            strict_unseen: false,
            conditions: vec![],
            allow_unseen: vec![],
        }
    }

    #[test]
    fn e2e_complete_rename_is_closed() {
        let base = build_tree(&[
            ("src/lib.rs", "pub fn old_name() {}\n"),
            ("src/use.rs", "fn call() { old_name(); }\n"),
        ]);
        let head = build_tree(&[
            ("src/lib.rs", "pub fn new_name() {}\n"),
            ("src/use.rs", "fn call() { new_name(); }\n"),
        ]);
        let mut c = base_contract();
        c.conditions.push(Condition::Rename {
            from: "old_name".into(),
            to: "new_name".into(),
            scope: vec![],
        });
        let cert = eval(&base, &head, c);
        assert_eq!(cert.verdict, Verdict::Closed, "certificate: {:?}", cert);
    }

    #[test]
    fn e2e_def_only_rename_is_open_with_the_missed_site() {
        let base = build_tree(&[
            ("src/lib.rs", "pub fn old_name() {}\n"),
            ("src/use.rs", "fn call() { old_name(); }\n"),
        ]);
        let head = build_tree(&[
            ("src/lib.rs", "pub fn new_name() {}\n"),
            ("src/use.rs", "fn call() { old_name(); }\n"),
        ]);
        let mut c = base_contract();
        c.conditions.push(Condition::Rename {
            from: "old_name".into(),
            to: "new_name".into(),
            scope: vec![],
        });
        let cert = eval(&base, &head, c);
        assert_eq!(cert.verdict, Verdict::Open);
        let ev = &cert.conditions[0].evidence;
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].file, "src/use.rs");
        assert_eq!(ev[0].line, 1);
        assert!(ev[0].excerpt.contains("old_name"));
        assert!(!ev[0].excerpt_sha256.is_empty());
    }

    #[test]
    fn e2e_comment_mention_does_not_block_closure() {
        let base = build_tree(&[("src/lib.rs", "pub fn old_name() {}\n")]);
        let head = build_tree(&[("src/lib.rs", "// old_name was renamed\npub fn new_name() {}\n")]);
        let mut c = base_contract();
        c.conditions.push(Condition::Rename {
            from: "old_name".into(),
            to: "new_name".into(),
            scope: vec![],
        });
        let cert = eval(&base, &head, c);
        assert_eq!(cert.verdict, Verdict::Closed, "comment mention must be masked");
    }

    #[test]
    fn e2e_undeclared_removal_opens_the_verdict() {
        let base = build_tree(&[
            ("src/lib.rs", "pub fn gone_fn() {}\npub fn kept() {}\n"),
            ("src/use.rs", "fn call() { gone_fn(); }\n"),
        ]);
        let head = build_tree(&[
            ("src/lib.rs", "pub fn kept() {}\n"),
            ("src/use.rs", "fn call() {}\n"),
        ]);
        let cert = eval(&base, &head, base_contract());
        assert_eq!(cert.verdict, Verdict::Open);
        assert!(cert.delta.unclaimed_removed.iter().any(|s| s == "gone_fn"));
    }

    #[test]
    fn e2e_certificate_is_byte_identical_across_runs() {
        let base = build_tree(&[("src/lib.rs", "pub fn old_name() {}\n")]);
        let head = build_tree(&[("src/lib.rs", "pub fn new_name() {}\n")]);
        let mut c = base_contract();
        c.conditions.push(Condition::Rename {
            from: "old_name".into(),
            to: "new_name".into(),
            scope: vec![],
        });
        let a = canonical_json(&eval(&base, &head, c.clone())).unwrap();
        let b = canonical_json(&eval(&base, &head, c)).unwrap();
        assert_eq!(a, b, "certificate must be deterministic");
    }

    #[test]
    fn e2e_signature_mismatch_is_open() {
        let base = build_tree(&[("src/lib.rs", "pub fn parse(a: u8) {}\n")]);
        let head = build_tree(&[("src/lib.rs", "pub fn parse(a: u8, b: u8) {}\n")]);
        let mut c = base_contract();
        c.conditions
            .push(Condition::Signature { name: "parse".into(), arity: 2 });
        let cert = eval(&base, &head, c);
        assert_eq!(cert.verdict, Verdict::Closed, "arity 2 declared and present");

        let mut c2 = base_contract();
        c2.conditions
            .push(Condition::Signature { name: "parse".into(), arity: 3 });
        let cert2 = eval(&base, &head, c2);
        assert_eq!(cert2.verdict, Verdict::Open);

        // Single-character names are rejected as unsafe to close over (they
        // would match every `x` in the tree). The condition reports
        // INDETERMINATE rather than pretending to verify.
        let mut c3 = base_contract();
        c3.conditions
            .push(Condition::Signature { name: "x".into(), arity: 1 });
        assert_eq!(eval(&base, &head, c3).verdict, Verdict::Indeterminate);
    }

    #[test]
    fn e2e_files_scope_violation_is_open() {
        let base = build_tree(&[("src/lib.rs", "pub fn a() {}\n")]);
        let head = build_tree(&[
            ("src/lib.rs", "pub fn a() {}\npub fn b() {}\n"),
            ("other/x.rs", "pub fn y() {}\n"),
        ]);
        let mut c = base_contract();
        c.conditions.push(Condition::Files {
            within: vec!["src/".into()],
        });
        let cert = eval(&base, &head, c);
        assert_eq!(cert.verdict, Verdict::Open);
        assert!(cert.conditions[0]
            .evidence
            .iter()
            .any(|e| e.file == "other/x.rs"));
    }

    #[test]
    fn e2e_no_new_dead_detects_lost_last_use() {
        let base = build_tree(&[
            ("src/lib.rs", "pub fn orphan() {}\n"),
            ("src/use.rs", "fn call() { orphan(); }\n"),
        ]);
        let head = build_tree(&[
            ("src/lib.rs", "pub fn orphan() {}\n"),
            ("src/use.rs", "fn call() {}\n"),
        ]);
        let mut c = base_contract();
        c.conditions.push(Condition::NoNewDead);
        let cert = eval(&base, &head, c);
        assert_eq!(cert.verdict, Verdict::Open, "orphan lost its only use");
    }

    #[test]
    fn e2e_unseen_is_reported_and_waivable() {
        let base = build_tree(&[("src/lib.rs", "pub fn a() {}\n")]);
        let head = build_tree(&[
            ("src/lib.rs", "pub fn a() {}\n"),
            ("src/notes.txt", "not code at all\n"),
        ]);
        let cert = eval(&base, &head, base_contract());
        assert!(
            cert.unseen.iter().any(|u| u.path == "src/notes.txt"),
            "unseen region must be reported: {:?}",
            cert.unseen
        );
        let mut waived = base_contract();
        waived.allow_unseen = vec!["src/*.txt".into()];
        let cert2 = eval(&base, &head, waived);
        assert!(
            cert2.unseen.iter().all(|u| u.path != "src/notes.txt"),
            "waived file must leave the active unseen list"
        );
    }
}

# Ground truth for the 10 long-bench queries against tokio v1 corpus.
# Built by inspecting /tmp/tokio-corpus/tokio/src/ on 2026-07-13.

# q1: What types have a `consume` method?
q1_consume_impls = {
    "type": "symbol_list",
    "question": "What types have a `consume` method in tokio?",
    "correct": [
        {"name": "Take::consume",        "file": "io/util/take.rs",       "line": 121},
        {"name": "Empty::consume",       "file": "io/util/empty.rs",      "line": 89},
        {"name": "Chain::consume",       "file": "io/util/chain.rs",      "line": 128},
        {"name": "BufWriter::consume",   "file": "io/util/buf_writer.rs", "line": 284},
        {"name": "BufStream::consume",   "file": "io/util/buf_stream.rs", "line": 194},
        {"name": "BufReader::consume",   "file": "io/util/buf_reader.rs", "line": 140},
        {"name": "AsyncBufRead::consume","file": "io/async_buf_read.rs", "line": 62},
        {"name": "AsyncBufRead::consume","file": "io/async_buf_read.rs", "line": 71},
        {"name": "AsyncBufRead::consume","file": "io/async_buf_read.rs", "line": 94},
        {"name": "AsyncBufRead::consume","file": "io/async_buf_read.rs", "line": 106},
    ],
    "min_correct_to_pass": 4,
}

# q2: Where is `consume` called from?
q2_consume_callers = {
    "type": "symbol_list",
    "question": "Which functions call `.consume(amt)` on a tokio type?",
    "correct": [
        {"name": "Take::poll_read",            "file": "io/util/take.rs",       "line": 126},
        {"name": "BufStream::poll_write",      "file": "io/util/buf_stream.rs", "line": 195},
        {"name": "BufReader::poll_fill_buf",   "file": "io/util/buf_reader.rs", "line": 117},
        {"name": "BufWriter::poll_write",      "file": "io/util/buf_writer.rs", "line": 285},
        {"name": "Chain::poll_fill_buf",       "file": "io/util/chain.rs",      "line": 131},
        {"name": "Chain::poll_fill_buf",       "file": "io/util/chain.rs",      "line": 133},
    ],
    "min_correct_to_pass": 2,
}

# q3: Where is Runtime::block_on defined?
q3_block_on_def = {
    "type": "single_answer",
    "question": "Where is `Runtime::block_on` defined?",
    "correct": [
        {"name": "Runtime::block_on", "file": "runtime/runtime.rs", "line": 340},
    ],
    "min_correct_to_pass": 1,
}

# q4: What functions does Runtime::block_on call (chain)?
q4_block_on_chain = {
    "type": "call_chain",
    "question": "What is the call chain from `Runtime::block_on`?",
    "correct": [
        {"name": "Runtime::block_on", "file": "runtime/runtime.rs", "line": 340},
        {"name": "Runtime::block_on_inner", "file": "runtime/runtime.rs", "line": 353},
        {"name": "Handle::block_on", "file": "runtime/handle.rs", "line": 234},
        {"name": "BasicScheduler::block_on", "file": "runtime/scheduler/multi_thread/mod.rs", "line": 87},
        {"name": "LocalSet::block_on", "file": "task/local.rs", "line": 673},
    ],
    "must_include": ["block_on_inner", "scheduler"],
    "min_correct_to_pass": 2,
}

# q5: Who calls spawn()?
q5_spawn_callers = {
    "type": "symbol_list",
    "question": "Which functions call `spawn`?",
    "correct": [
        {"name": "JoinSet::spawn",      "file": "task/join_set.rs",      "line": 142},
        {"name": "JoinSet::spawn_with_id","file": "task/join_set.rs",    "line": 722},
        {"name": "LocalSet::spawn",     "file": "task/local.rs",         "line": 1028},
        {"name": "task::spawn",         "file": "task/spawn.rs",         "line": 174},
        {"name": "Handle::spawn",       "file": "runtime/handle.rs",     "line": 197},
    ],
    "min_correct_to_pass": 2,
}

# q6: Where is Sleep defined?
q6_sleep_def = {
    "type": "single_answer",
    "question": "Where is `Sleep` defined (the struct)?",
    "correct": [
        {"name": "Sleep", "file": "time/sleep.rs", "line": 225},
    ],
    "min_correct_to_pass": 1,
}

# q7: What methods does Sleep have?
q7_sleep_methods = {
    "type": "symbol_list",
    "question": "What methods does the Sleep struct expose?",
    "correct": [
        {"name": "far_future",   "file": "time/sleep.rs", "line": 299},
        {"name": "deadline",     "file": "time/sleep.rs", "line": 304},
        {"name": "is_elapsed",   "file": "time/sleep.rs", "line": 311},
        {"name": "reset",        "file": "time/sleep.rs", "line": 344},
        {"name": "reset_without_timer","file": "time/sleep.rs", "line": 385},
        {"name": "poll_elapsed", "file": "time/sleep.rs", "line": 391},
        {"name": "poll",         "file": "time/sleep.rs", "line": 464},
    ],
    "min_correct_to_pass": 3,
}

# q8: What's BufWriter::poll_write call chain?
q8_bufwriter_write = {
    "type": "call_chain",
    "question": "What does BufWriter::poll_write do?",
    "correct": [
        {"name": "BufWriter::poll_write",   "file": "io/util/buf_writer.rs", "line": 235},
        {"name": "BufWriter::flush_buf",    "file": "io/util/buf_writer.rs", "line": 58},
        {"name": "BufWriter::inner_write",  "file": "io/util/buf_writer.rs", "line": 150},
        {"name": "W::poll_write",            "file": "io/util/buf_writer.rs", "line": 145},
    ],
    "must_include": ["flush_buf", "inner"],
    "min_correct_to_pass": 2,
}

# q9: Re-check — same as q1
q9_consume_impls_recheck = {
    "type": "symbol_list",
    "question": "Re-check: which types in io/util implement the consume method on AsyncBufRead?",
    "correct": [
        {"name": "Take::consume",       "file": "io/util/take.rs",       "line": 121},
        {"name": "Empty::consume",      "file": "io/util/empty.rs",      "line": 89},
        {"name": "Chain::consume",      "file": "io/util/chain.rs",      "line": 128},
        {"name": "BufWriter::consume",  "file": "io/util/buf_writer.rs", "line": 284},
        {"name": "BufStream::consume",  "file": "io/util/buf_stream.rs", "line": 194},
        {"name": "BufReader::consume",  "file": "io/util/buf_reader.rs", "line": 140},
    ],
    "min_correct_to_pass": 3,
}

# q10: What code is dead/unused?
q10_dead_code = {
    "type": "dead_symbol_list",
    "question": "Find unused/dead functions in io/util.",
    "correct": [
        # From V8+ bench results: 7 dead fns in io/util (path-filtered dead_symbols)
        # But ground truth is what the model should find.
        # Conservative: any function with zero cross-file callers.
        {"name": "Empty::poll_write",  "file": "io/util/empty.rs", "line": 89, "category": "dead"},
        {"name": "Empty::poll_flush",  "file": "io/util/empty.rs", "line": 100, "category": "dead"},
        {"name": "Take::poll_write",   "file": "io/util/take.rs",  "line": 145, "category": "dead"},
        {"name": "Chain::poll_write",  "file": "io/util/chain.rs", "line": 142, "category": "dead"},
    ],
    "min_correct_to_pass": 1,
    "acceptable_in_answer": ["dead", "unused", "consume", "empty", "chain", "take"],
}

# ── Scoring weights ──
# Map F1 to 0-3 score, mirroring the existing keyword rubric:
#   F1 >= 0.70  →  3
#   F1 >= 0.40  →  2
#   F1 >= 0.15  →  1
#   F1 <  0.15  →  0
SCORE_THRESHOLDS = [(0.70, 3), (0.40, 2), (0.15, 1), (0.0, 0)]
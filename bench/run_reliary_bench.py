"""Run reliary self-benchmark (Option E)."""
import sys
import os
sys.path.insert(0, os.path.dirname(__file__))

# Monkey-patch to use reliary corpus
import llm_conn
llm_conn.TOKIO_CORPUS = "$HOME/src/reliary8"

import multi_turn_harness as mth
mth.TOKIO_CORPUS = "$HOME/src/reliary8"
mth.ALTBACKEND_PROJECT = "$HOME-src-reliary8"  # correct project for reliary corpus

# Import queries from reliary_bench
from reliary_bench import SESSION_QUERIES, RELIARY_CORPUS

# Patch SESSION_QUERIES in long_session_bench
import long_session_bench
long_session_bench.SESSION_QUERIES = SESSION_QUERIES

# Run the benchmark
long_session_bench.main()
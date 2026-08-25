#!/bin/bash
# reliary SessionStart reminder for Claude Code
# Prints tool guide at session start. Installed by `reliary init`.
# Toggle: RELIARY_GATE=1 (default OFF)

if [ "${RELIARY_GATE:-1}" != "1" ]; then
  exit 0
fi

cat << 'REMINDER'
Code intelligence protocol — use reliary tools:
1. reliary_find_references_with_source(name) — find references to a symbol (returns file:line + source inline)
2. reliary_goto_def(name) — find where a symbol is defined
3. reliary_callgraph(name) — trace callers/callees of a function
4. reliary_methods_on(type_name) — list methods on a type
5. reliary_search(query) — full-text search for unknown symbol names
6. Fall back to Grep/Glob/Read only if reliary lacks the data
REMINDER
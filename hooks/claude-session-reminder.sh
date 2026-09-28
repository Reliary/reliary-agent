#!/bin/sh
# reliary SessionStart reminder for Claude Code
# Prints a tool guide at session start. Installed by `reliary init`.
# Toggle: RELIARY_REMINDER=0 to disable (default ON).

if [ "${RELIARY_REMINDER:-1}" != "1" ]; then
  exit 0
fi

cat << 'REMINDER'
Code intelligence protocol — prefer reliary MCP tools over Grep/Read:
1. reliary_find_references(name) — definitions, callers, implementors
2. reliary_search(query) — full-text search for unknown symbols
3. reliary_call_graph(name) — callers/callees of a function
4. reliary_list_methods(type_name) — methods on a type
5. reliary_describe(symbol) — overview of a symbol
6. reliary_verify(text) — verify a claim about the code
Fall back to Grep/Glob/Read only if reliary lacks the data.
REMINDER

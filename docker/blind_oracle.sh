#!/bin/sh
# Blind-tree ctags parity gate (plan v2, step 4).
# Compares ctags defs vs reliary occurrence is_def=1 rows on an UNSEEN tree.
# Usage: blind_oracle.sh (expects /corpus ro-mounted, /out writable)
# Env: RELIARY_SEGMENTER=1 enables the island path; unset = legacy.
set -eu

CORPUS="${CORPUS:-/corpus}"
OUT="${OUT:-/out}"
WORK="$OUT/work"
DB="$WORK/idx.sqlite"
CTAGS_DEFS="$OUT/ctags_defs.txt"
OUR_DEFS="$OUT/our_defs.txt"
REPORT="$OUT/parity_report.txt"

rm -rf "$WORK"
mkdir -p "$WORK"

echo "== trust: $CORPUS (flag=${RELIARY_SEGMENTER:-0})"
/usr/bin/time -v reliary trust "$CORPUS" 2>"$OUT/trust_time.txt"
echo "trust exit: $?"

# The DB lives at <corpus>/.reliary/index.sqlite.
DB="$(find "$CORPUS/.reliary" -name index.sqlite | head -1)"
echo "db: $DB"

echo "== build-all occurrences"
reliary build-all "$CORPUS" 2>&1 | tail -2 || true

echo "== ctags pass"
# Raw ctags lines are appended unmodified. The previous awk projection
# ("keep the last two fields") destroyed kind+line on every line carrying a
# scope/signature field -- 52% of lines on the blind tokio run. Parsing now
# happens in oracle_report.py, which scans every field.
: > "$CTAGS_DEFS"
find "$CORPUS" -type f \( -name '*.rs' -o -name '*.c' -o -name '*.h' \
  -o -name '*.py' -o -name '*.js' -o -name '*.ts' \
  -o -name '*.rb' -o -name '*.hs' -o -name '*.ml' -o -name '*.mli' \
  -o -name '*.go' -o -name '*.java' -o -name '*.cs' \) \
  -not -path '*/target/*' -not -path '*/node_modules/*' \
  | while IFS= read -r f; do
  ctags -o - --excmd=number --fields=+nKz "$f" 2>/dev/null \
    >> "$CTAGS_DEFS" || true
done
wc -l "$CTAGS_DEFS"

echo "== our defs pass"
sqlite3 "$DB" \
  "SELECT p.phrase || char(9) || f.file_path || ':' || o.line
   FROM occurrence o
   JOIN phrases p ON p.id = o.phrase_id
   JOIN file_map f ON f.id = o.file_id
   WHERE o.is_def = 1;" > "$OUR_DEFS" 2>/dev/null || echo "QUERY FAILED" > "$OUR_DEFS"
wc -l "$OUR_DEFS"

echo "== report"
python3 /usr/local/bin/oracle_report.py "$CTAGS_DEFS" "$OUR_DEFS" "$REPORT"
cat "$REPORT"

//! V70 P5: `reliary map` — deterministic self-contained SVG overview.
//!
//! Renders files as cards in a grid; symbols inside each card are listed with
//! their caller counts (bold when hot, dimmed when dead). Pure SVG, inline
//! styles only, zero JS, no external assets. Byte-identical across runs for
//! the same index: all queries carry ORDER BY and the layout is computed from
//! sorted data.

use rusqlite::Connection;
use std::collections::BTreeMap;

/// One symbol line on the map.
#[derive(Debug, Clone)]
pub struct MapSymbol {
    pub name: String,
    pub callers: usize,
    pub dead: bool,
}

/// One file card.
#[derive(Debug, Clone)]
pub struct MapFile {
    pub path: String,
    pub display: String,
    pub symbols: Vec<MapSymbol>,
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Collect map data: defs per source file with caller counts, capped.
pub fn collect_map(db: &Connection, max_files: usize, max_symbols_per_file: usize) -> Vec<MapFile> {
    // Symbols: phrase, file, caller count, dead flag. All FROM source files.
    let mut by_file: BTreeMap<String, Vec<MapSymbol>> = BTreeMap::new();
    {
        // V74: caller count must be source-only — a doc/comment mention was
        // marking dead symbols as alive, and non-source files inflated counts.
        let sql = "SELECT p.phrase, f.file_path,
                          (SELECT COUNT(*) FROM occurrence o2
                            JOIN file_map f2 ON f2.id = o2.file_id
                            WHERE o2.phrase_id = o.phrase_id AND o2.is_def = 0
                              AND f2.is_source = 1) AS callers
                   FROM occurrence o
                   JOIN phrases p ON p.id = o.phrase_id
                   JOIN file_map f ON f.id = o.file_id
                   WHERE o.is_def = 1 AND o.tag = 1 AND f.is_source = 1
                   GROUP BY p.phrase, f.file_path
                   ORDER BY p.phrase, f.file_path";
        let mut stmt = match db.prepare_cached(sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let mut rows = match stmt.query([]) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        while let Ok(Some(r)) = rows.next() {
            let name: String = r.get(0).unwrap_or_default();
            let path: String = r.get(1).unwrap_or_default();
            let callers: i64 = r.get(2).unwrap_or(0);
            if name.is_empty() {
                continue;
            }
            by_file.entry(path).or_default().push(MapSymbol {
                name,
                callers: callers.max(0) as usize,
                dead: callers == 0,
            });
        }
    }

    // Rank symbols within a file by (callers desc, name asc); cap.
    let mut files: Vec<MapFile> = by_file
        .into_iter()
        .map(|(path, mut syms)| {
            syms.sort_by(|a, b| b.callers.cmp(&a.callers).then_with(|| a.name.cmp(&b.name)));
            syms.truncate(max_symbols_per_file);
            let display = path.rsplit('/').next().unwrap_or(&path).to_string();
            MapFile { path, display, symbols: syms }
        })
        .collect();

    // Rank files by total callers (hot first), then path asc; cap.
    files.sort_by(|a, b| {
        let ta: usize = a.symbols.iter().map(|s| s.callers).sum();
        let tb: usize = b.symbols.iter().map(|s| s.callers).sum();
        tb.cmp(&ta).then_with(|| a.path.cmp(&b.path))
    });
    files.truncate(max_files);
    files
}

/// Render the map as a standalone SVG document.
pub fn render_svg(files: &[MapFile], title: &str) -> String {
    const CARD_W: i32 = 300;
    const CARD_H: i32 = 210;
    const COLS: i32 = 4;
    const GAP: i32 = 16;
    const MARGIN: i32 = 24;

    let cards = files.len() as i32;
    let rows = if cards == 0 { 1 } else { (cards + COLS - 1) / COLS };
    let width = MARGIN * 2 + COLS * CARD_W + (COLS - 1) * GAP;
    let height = MARGIN * 2 + rows * CARD_H + (rows - 1) * GAP + 40;

    let mut s = String::with_capacity(16 * 1024);
    s.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" \
         viewBox=\"0 0 {} {}\" font-family=\"monospace\">\n",
        width, height, width, height
    ));
    s.push_str(&format!(
        "<rect width=\"{}\" height=\"{}\" fill=\"#0d1117\"/>\n",
        width, height
    ));
    s.push_str(&format!(
        "<text x=\"{}\" y=\"28\" fill=\"#e6edf3\" font-size=\"16\">{} ({} files)</text>\n",
        MARGIN,
        xml_escape(title),
        files.len()
    ));

    for (i, f) in files.iter().enumerate() {
        let col = (i as i32) % COLS;
        let row = (i as i32) / COLS;
        let x = MARGIN + col * (CARD_W + GAP);
        let y = 48 + MARGIN + row * (CARD_H + GAP);

        s.push_str(&format!(
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"6\" \
             fill=\"#161b22\" stroke=\"#30363d\"/>\n",
            x, y, CARD_W, CARD_H
        ));
        // Title: basename + parent dir hint.
        let parent = f
            .path
            .rsplit_once('/')
            .map(|(p, _)| p.rsplit('/').next().unwrap_or(""))
            .unwrap_or("");
        s.push_str(&format!(
            "<text x=\"{}\" y=\"{}\" fill=\"#e6edf3\" font-size=\"12\" font-weight=\"bold\">{}</text>\n",
            x + 10,
            y + 20,
            xml_escape(&f.display)
        ));
        if !parent.is_empty() {
            s.push_str(&format!(
                "<text x=\"{}\" y=\"{}\" fill=\"#8b949e\" font-size=\"10\">{}/</text>\n",
                x + 10,
                y + 34,
                xml_escape(parent)
            ));
        }
        for (j, sym) in f.symbols.iter().enumerate() {
            let ty = y + 54 + j as i32 * 15;
            if ty > y + CARD_H - 12 {
                break;
            }
            let (name_color, count_color) = if sym.dead {
                ("#6e7681", "#6e7681") // dimmed — dead code
            } else if sym.callers >= 10 {
                ("#e6edf3", "#f0883e") // hot — orange count
            } else {
                ("#c9d1d9", "#8b949e")
            };
            let weight = if sym.callers >= 10 { "bold" } else { "normal" };
            s.push_str(&format!(
                "<text x=\"{}\" y=\"{}\" fill=\"{}\" font-size=\"11\" font-weight=\"{}\">{}</text>\n",
                x + 12,
                ty,
                name_color,
                weight,
                xml_escape(&sym.name)
            ));
            if sym.callers > 0 {
                s.push_str(&format!(
                    "<text x=\"{}\" y=\"{}\" fill=\"{}\" font-size=\"10\" text-anchor=\"end\">{}</text>\n",
                    x + CARD_W - 10,
                    ty,
                    count_color,
                    sym.callers
                ));
            }
        }
    }

    // Legend.
    s.push_str(&format!(
        "<text x=\"{}\" y=\"{}\" fill=\"#8b949e\" font-size=\"10\">\
         number = call sites · orange/bold = hot (&gt;=10) · dimmed = dead (0 callers)</text>\n",
        MARGIN,
        height - MARGIN / 2
    ));
    s.push_str("</svg>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, syms: Vec<(&str, usize)>) -> MapFile {
        MapFile {
            path: format!("/x/src/{}", name),
            display: name.to_string(),
            symbols: syms
                .into_iter()
                .map(|(n, c)| MapSymbol { name: n.to_string(), callers: c, dead: c == 0 })
                .collect(),
        }
    }

    #[test]
    fn empty_map_renders() {
        let svg = render_svg(&[], "empty");
        assert!(svg.starts_with("<svg"));
        assert!(svg.ends_with("</svg>\n"));
        assert!(svg.contains("0 files"));
    }

    #[test]
    fn unicode_names_escaped() {
        let f = file("a.rs", vec![("café_λ", 3), ("<script>", 0)]);
        let svg = render_svg(&[f], "unicode");
        assert!(svg.contains("café_λ"));
        assert!(!svg.contains("<script>"));
        assert!(svg.contains("&lt;script&gt;"));
    }

    #[test]
    fn deterministic_output() {
        let files = vec![
            file("b.rs", vec![("b_fn", 1)]),
            file("a.rs", vec![("a_fn", 9)]),
        ];
        let one = render_svg(&files, "t");
        let two = render_svg(&files, "t");
        assert_eq!(one, two);
    }

    #[test]
    fn escaping_amp_and_quote() {
        assert_eq!(xml_escape("a&b\"c'd<e>"), "a&amp;b&quot;c&apos;d&lt;e&gt;");
    }
}

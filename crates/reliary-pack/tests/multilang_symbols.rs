// V80: end-to-end coverage that `generate_pack` extracts symbols from
// non-JS/TS/Rust source. Before this, `is_source_file` had no `cs` extension
// and the definition filter was Rust/JS/TS-shaped, so a .NET repo produced
// "(0 symbols)" and a mixed repo silently dropped its whole C# backend.
use reliary_pack::{generate_pack, PackFormat};

fn tmpdir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!("reliary_pack_{}_{}", tag, nanos));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Build an index for `dir` at the path `generate_pack` reads, then pack it.
fn pack_for(dir: &std::path::Path) -> String {
    let rdir = dir.join(".reliary");
    std::fs::create_dir_all(&rdir).unwrap();
    let db = rusqlite::Connection::open(rdir.join("index.sqlite")).unwrap();
    reliary_search::schema::create_new_db(&db).unwrap();
    reliary_search::ingest::index_directory(&db, dir.to_str().unwrap()).unwrap();
    reliary_search::lazy_occurrence::build_all_occurrence(&db).unwrap();
    drop(db);
    generate_pack(dir.to_str().unwrap(), PackFormat::Full)
        .expect("generate_pack")
}

#[test]
fn pack_extracts_csharp_symbols() {
    let d = tmpdir("cs");
    std::fs::write(
        d.join("RoleController.cs"),
        "namespace App {\n\
         public sealed class RoleController : ControllerBase\n\
         {\n\
             private readonly IRepo _repo;\n\
             public async Task<IActionResult> GetAsync(Guid id)\n\
             {\n\
                 return Ok();\n\
             }\n\
         }\n}\n",
    )
    .unwrap();
    let pack = pack_for(&d);

    assert!(pack.contains("RoleController"), "pack must contain the class:\n{}", pack);
    assert!(pack.contains("GetAsync"), "pack must contain the method:\n{}", pack);
    // The namespace must NOT be a bogus symbol header.
    assert!(!pack.contains("## App\n"), "namespace must not be a symbol:\n{}", pack);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn pack_extracts_go_and_java_symbols() {
    let d = tmpdir("gojava");
    std::fs::write(
        d.join("svc.go"),
        "package svc\n\nfunc ProcessRequest(id int) int {\n\treturn id + 1\n}\n",
    )
    .unwrap();
    std::fs::write(
        d.join("Svc.java"),
        "public class Svc {\n    public int handleRequest(int id) {\n        return id;\n    }\n}\n",
    )
    .unwrap();
    let pack = pack_for(&d);

    assert!(pack.contains("ProcessRequest"), "Go func missing:\n{}", pack);
    assert!(pack.contains("handleRequest"), "Java method missing:\n{}", pack);
    let _ = std::fs::remove_dir_all(&d);
}

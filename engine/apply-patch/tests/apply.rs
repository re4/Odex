use std::fs;
use std::path::Path;

use odex_apply_patch::{apply, apply_patch_text, parse_patch, preview, summarize, ChangeKind, PatchError};
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn write(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn read(dir: &Path, rel: &str) -> String {
    fs::read_to_string(dir.join(rel)).unwrap()
}

fn run(dir: &Path, patch: &str) -> Result<Vec<odex_apply_patch::FileChangePreview>, PatchError> {
    apply(&parse_patch(patch).unwrap(), dir)
}

fn update_patch(path: &str, body: &str) -> String {
    format!("*** Begin Patch\n*** Update File: {path}\n{body}\n*** End Patch")
}

#[test]
fn add_update_delete_and_move() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "path/old.txt", "bye\n");
    write(dir, "src/app.py", "import os\n\ndef greet():\n    context line\n    old line\n\nmore context\nx\n");
    let patch = "*** Begin Patch
*** Add File: path/new.txt
+line one
+line two
*** Delete File: path/old.txt
*** Update File: src/app.py
*** Move to: src/main.py
@@ def greet():
     context line
-    old line
+    new line
@@
 more context
-x
+y
*** End of File
*** End Patch";
    let changes = run(dir, patch).unwrap();
    assert_eq!(changes.len(), 3);
    assert_eq!(read(dir, "path/new.txt"), "line one\nline two\n");
    assert!(!dir.join("path/old.txt").exists());
    assert!(!dir.join("src/app.py").exists());
    assert_eq!(
        read(dir, "src/main.py"),
        "import os\n\ndef greet():\n    context line\n    new line\n\nmore context\ny\n"
    );

    assert_eq!(changes[0].kind, ChangeKind::Add);
    assert_eq!(changes[0].additions, 2);
    assert!(changes[0].unified_diff.starts_with("--- /dev/null\n+++ b/path/new.txt\n"), "{}", changes[0].unified_diff);
    assert_eq!(changes[1].kind, ChangeKind::Delete);
    assert_eq!((changes[1].additions, changes[1].deletions), (0, 1));
    assert!(changes[1].unified_diff.contains("+++ /dev/null"));
    assert_eq!(changes[2].kind, ChangeKind::Update);
    assert!(changes[2].path.is_absolute());
    assert!(changes[2].move_to.as_ref().unwrap().ends_with("src/main.py"));
    assert_eq!((changes[2].additions, changes[2].deletions), (2, 2));
    assert!(
        changes[2].unified_diff.starts_with("--- a/src/app.py\n+++ b/src/main.py\n"),
        "{}",
        changes[2].unified_diff
    );

    let summary = summarize(&changes, dir);
    assert_eq!(
        summary,
        "Success. Updated the following files:\nA path/new.txt\nD path/old.txt\nR src/app.py -> src/main.py"
    );
}

#[test]
fn preview_does_not_write() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "a.txt", "one\ntwo\n");
    let patch = parse_patch(&update_patch("a.txt", "@@\n-two\n+TWO\n*** Add File: b.txt\n+b")).unwrap();
    let changes = preview(&patch, dir).unwrap();
    assert_eq!(changes[0].new_contents.as_deref(), Some("one\nTWO\n"));
    assert_eq!(changes[0].old_contents.as_deref(), Some("one\ntwo\n"));
    assert_eq!(changes[0].unified_diff, "--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n one\n-two\n+TWO\n");
    assert_eq!(read(dir, "a.txt"), "one\ntwo\n");
    assert!(!dir.join("b.txt").exists());
}

#[test]
fn fuzzy_levels_apply() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    // trailing whitespace in file
    write(dir, "t1.txt", "alpha   \nbeta\t\n");
    run(dir, &update_patch("t1.txt", "@@\n alpha\n-beta\n+BETA")).unwrap();
    assert_eq!(read(dir, "t1.txt"), "alpha   \nBETA\n");
    // indentation differences
    write(dir, "t2.txt", "    if x:\n        y()\n");
    run(dir, &update_patch("t2.txt", "@@\n if x:\n-  y()\n+        z()")).unwrap();
    assert_eq!(read(dir, "t2.txt"), "    if x:\n        z()\n");
    // unicode punctuation in file, ASCII in patch
    write(dir, "t3.txt", "say(\u{201C}hello\u{201D}) \u{2014} done\nnext\u{00A0}line\n");
    run(dir, &update_patch("t3.txt", "@@\n-say(\"hello\") - done\n+say(\"bye\")\n next line")).unwrap();
    assert_eq!(read(dir, "t3.txt"), "say(\"bye\")\nnext\u{00A0}line\n");
    // and the reverse: smart quotes in patch, ASCII in file
    write(dir, "t4.txt", "it's here\n");
    run(dir, &update_patch("t4.txt", "@@\n-it\u{2019}s here\n+it is here")).unwrap();
    assert_eq!(read(dir, "t4.txt"), "it is here\n");
}

#[test]
fn crlf_and_bom_are_preserved() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "w.txt", "\u{feff}first\r\nsecond\r\nthird\r\n");
    let changes = run(dir, &update_patch("w.txt", "@@\n first\n-second\n+SECOND\n+inserted")).unwrap();
    assert_eq!(read(dir, "w.txt"), "\u{feff}first\r\nSECOND\r\ninserted\r\nthird\r\n");
    // Diff is LF-normalised and BOM-free.
    assert!(!changes[0].unified_diff.contains('\r'));
    assert!(!changes[0].unified_diff.contains('\u{feff}'));
    // A CRLF patch applies to an LF file and keeps LF.
    write(dir, "u.txt", "a\nb\n");
    run(dir, &update_patch("u.txt", "@@\n-a\n+A").replace('\n', "\r\n")).unwrap();
    assert_eq!(read(dir, "u.txt"), "A\nb\n");
}

#[test]
fn final_newline_presence_is_preserved() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "n.txt", "a\nb");
    run(dir, &update_patch("n.txt", "@@\n a\n-b\n+c\n*** End of File")).unwrap();
    assert_eq!(read(dir, "n.txt"), "a\nc");
    write(dir, "m.txt", "a\nb\n");
    run(dir, &update_patch("m.txt", "@@\n a\n-b\n+c\n*** End of File")).unwrap();
    assert_eq!(read(dir, "m.txt"), "a\nc\n");
}

#[test]
fn eof_anchor_picks_last_occurrence() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "e.txt", "}\nfn x() {\n}\n");
    run(dir, &update_patch("e.txt", "@@\n-}\n+} // end\n*** End of File")).unwrap();
    assert_eq!(read(dir, "e.txt"), "}\nfn x() {\n} // end\n");
}

#[test]
fn pure_insertions() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "p.py", "class A:\n    pass\n\ndef f():\n    return 1\n");
    run(dir, &update_patch("p.py", "@@ class A:\n+    x = 1\n@@\n+\n+def g():\n+    return 2")).unwrap();
    assert_eq!(
        read(dir, "p.py"),
        "class A:\n    x = 1\n    pass\n\ndef f():\n    return 1\n\ndef g():\n    return 2\n"
    );
}

#[test]
fn multiple_chunks_apply_in_order_with_headers() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "m.py", "def a():\n    return 0\n\ndef b():\n    return 0\n\ndef c():\n    return 0\n");
    let body = "@@ def b():\n-    return 0\n+    return 2\n@@ def c():\n-    return 0\n+    return 3";
    run(dir, &update_patch("m.py", body)).unwrap();
    assert_eq!(read(dir, "m.py"), "def a():\n    return 0\n\ndef b():\n    return 2\n\ndef c():\n    return 3\n");

    // Without headers, identical chunks hit successive occurrences.
    write(dir, "r.txt", "x\nx\nx\n");
    run(dir, &update_patch("r.txt", "@@\n-x\n+1\n@@\n-x\n+2")).unwrap();
    assert_eq!(read(dir, "r.txt"), "1\n2\nx\n");
}

#[test]
fn context_error_names_file_chunk_and_closest_match() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "src/lib.rs", "fn one() {}\nfn two() {\n    let a = 1;\n    let b = 2;\n}\n");
    let body = "@@\n-fn one() {}\n+fn uno() {}\n@@\n fn two() {\n     let a = 1;\n-    let b = 3;\n+    let b = 4;";
    let err = run(dir, &update_patch("src/lib.rs", body)).unwrap_err();
    let PatchError::ContextNotFound(details) = &err else { panic!("{err}") };
    assert_eq!(details.path, "src/lib.rs");
    assert_eq!((details.chunk, details.chunk_count), (2, 2));
    assert_eq!(details.expected, vec!["fn two() {", "    let a = 1;", "    let b = 3;"]);
    let closest = details.closest.as_ref().unwrap();
    assert_eq!((closest.line, closest.matched), (2, 2));
    let msg = err.to_string();
    assert!(msg.contains("src/lib.rs: chunk 2 of 2 failed"), "{msg}");
    assert!(msg.contains("    let b = 3;"), "{msg}");
    assert!(msg.contains("line 2 (2 of 3 lines similar)"), "{msg}");
    assert!(msg.contains("     4 |     let b = 2;"), "{msg}");
    // Nothing was written (chunk 1 would have succeeded).
    assert!(read(dir, "src/lib.rs").starts_with("fn one() {}"));
}

#[test]
fn context_error_without_similar_region() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "a.txt", "hello\n");
    let err = run(dir, &update_patch("a.txt", "@@ fn main\n-zzz\n+yyy")).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("after `@@ fn main`"), "{msg}");
    assert!(msg.contains("No similar region"), "{msg}");
}

#[test]
fn missing_files_are_reported() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let err = run(dir, &update_patch("nope.txt", "@@\n-a\n+b")).unwrap_err();
    assert!(matches!(err, PatchError::FileNotFound { action: "update", .. }));
    assert_eq!(err.to_string(), "nope.txt: file not found (cannot update a file that does not exist)");
    let err = run(dir, "*** Begin Patch\n*** Delete File: gone.txt\n*** End Patch").unwrap_err();
    assert!(matches!(err, PatchError::FileNotFound { action: "delete", .. }));
}

#[test]
fn no_partial_writes_on_failure() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "keep.txt", "original\n");
    write(dir, "del.txt", "x\n");
    let patch = "*** Begin Patch
*** Add File: new/dir/created.txt
+hi
*** Update File: keep.txt
@@
-original
+changed
*** Delete File: del.txt
*** Update File: keep.txt
@@
-does not exist
+boom
*** End Patch";
    assert!(run(dir, patch).is_err());
    assert_eq!(read(dir, "keep.txt"), "original\n");
    assert!(dir.join("del.txt").exists());
    assert!(!dir.join("new").exists());
    // No temp files left behind.
    let leftovers: Vec<_> = fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).collect();
    assert_eq!(leftovers.len(), 2);
}

#[test]
fn sequential_ops_on_same_file_see_each_other() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let patch = "*** Begin Patch
*** Add File: f.txt
+one
+two
*** Update File: f.txt
@@
 one
-two
+2
*** End Patch";
    let changes = run(dir, patch).unwrap();
    assert_eq!(read(dir, "f.txt"), "one\n2\n");
    assert_eq!(changes[1].old_contents.as_deref(), Some("one\ntwo\n"));
}

#[test]
fn add_overwrites_existing_file_keeping_style() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "o.txt", "\u{feff}old\r\n");
    let changes = run(dir, "*** Begin Patch\n*** Add File: o.txt\n+new\n+lines\n*** End Patch").unwrap();
    assert_eq!(read(dir, "o.txt"), "\u{feff}new\r\nlines\r\n");
    assert_eq!(changes[0].old_contents.as_deref(), Some("\u{feff}old\r\n"));
    assert!(changes[0].unified_diff.starts_with("--- a/o.txt\n"));
}

#[test]
fn absolute_paths_and_parent_creation() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let abs = dir.join("deep").join("er").join("x.txt");
    let patch = format!("*** Begin Patch\n*** Add File: {}\n+x\n*** End Patch", abs.display());
    let other = TempDir::new().unwrap();
    let changes = run(other.path(), &patch).unwrap();
    assert_eq!(fs::read_to_string(&abs).unwrap(), "x\n");
    assert_eq!(changes[0].path, abs);
}

#[test]
fn move_onto_existing_file_overwrites_it() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "a.txt", "a\n");
    write(dir, "b.txt", "b\n");
    run(dir, "*** Begin Patch\n*** Update File: a.txt\n*** Move to: b.txt\n*** End Patch").unwrap();
    assert!(!dir.join("a.txt").exists());
    assert_eq!(read(dir, "b.txt"), "a\n");
}

#[test]
fn directories_and_binary_files_are_rejected() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    fs::create_dir_all(dir.join("sub")).unwrap();
    let err = run(dir, &update_patch("sub", "@@\n-a\n+b")).unwrap_err();
    assert!(matches!(err, PatchError::IsDirectory { .. }), "{err}");
    fs::write(dir.join("bin.dat"), [0xff, 0xfe, 0x00, 0x81]).unwrap();
    let err = run(dir, &update_patch("bin.dat", "@@\n-a\n+b")).unwrap_err();
    assert!(matches!(err, PatchError::NotUtf8 { .. }), "{err}");
}

#[test]
fn apply_patch_text_accepts_heredoc_wrapper() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "h.txt", "x\n");
    let text = "apply_patch <<'EOF'\r\n*** Begin Patch\r\n*** Update File: h.txt\r\n@@\r\n-x\r\n+y\r\n*** End Patch\r\nEOF\r\n";
    apply_patch_text(text, dir).unwrap();
    assert_eq!(read(dir, "h.txt"), "y\n");
}

#[test]
fn unchanged_update_produces_empty_diff() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "same.txt", "a\n");
    let changes = run(dir, &update_patch("same.txt", "@@\n-a\n+a")).unwrap();
    assert_eq!(changes[0].unified_diff, "");
    assert_eq!((changes[0].additions, changes[0].deletions), (0, 0));
}

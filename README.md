# simple-review

A desktop app for understanding and reviewing code changes commit by commit, built with
[egui](https://github.com/emilk/egui). See [PROJECT_SPEC.md](PROJECT_SPEC.md) for the full plan.

## Status: Phase 1 (browse and diff)

- Open a local repository (dialog, recent list, or a path on the command line).
- Lazily paged commit list with author, date, committer-differs note, merge and **AI** badges
  (`Co-Authored-By: Claude`), and an "AI only" filter.
- Changed files per commit, with renames/copies detected and a persisted "reviewed" checkbox.
- Side-by-side and unified diffs: syntax highlighting, word-level highlights, ignore-whitespace,
  moved-block detection, a change overview strip, and merge commits against either parent.
- Placeholders for binary files, images, Git LFS pointers, submodules and very large files;
  ignored/generated paths (`vendor/`, `storage/`, lock files, …) are collapsed until opened.
- Notices for line-ending changes (CRLF ↔ LF), non-UTF-8 files and a missing final newline.
- Back/forward history and keyboard shortcuts for everything (press `?` in the app).

Everything is read from git objects; the working copy is never touched.

## Running

```sh
cargo run --release -p simple-review -- /path/to/repo   # path is optional
```

Try it on the fixture repository used by the tests:

```sh
cargo run -p review-core --example make_fixture -- /tmp/review-fixture
cargo run --release -p simple-review -- /tmp/review-fixture
```

## Layout

| Crate | Contents |
| --- | --- |
| `crates/core` (`review-core`) | `git` (gix: commits, trees, renames, blobs), `diff` (imara-diff, alignment, word diff, moves, content classification), `file_view`, `highlight` (syntect), `settings` (SQLite), `tasks` (worker pool, commit pager, LRU cache), `fixture` |
| `crates/app` (`simple-review`) | eframe/egui binary: state, navigation history, widgets |

## Tests

```sh
cargo test                     # unit tests, fixture-repo tests, headless UI smoke test
SNAPSHOT_DIR=/tmp cargo test -p simple-review   # also saves screenshots of the UI
```

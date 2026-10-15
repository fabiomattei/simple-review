# simple-review

A desktop app for understanding and reviewing code changes commit by commit. It is written in
Rust with [egui](https://github.com/emilk/egui) and runs on macOS, Windows and Linux.

## Why

More and more code is written with AI assistants like Claude Code, and someone still has to read
it, understand it and decide whether it is right. A pull request view shows *what* changed, but
answering *why* and *how it fits in* usually means jumping between the diff, an editor, `git
blame` and the terminal.

simple-review puts all of that in one place. You pick a commit and see the old version (the
parent) next to the new one, and you can move through the code the way you would in an IDE: jump
to definitions, find references, see who wrote each line and whether it was AI-assisted. When
something needs fixing, you leave a comment on the lines and send the comments back to Claude
Code as instructions.

The guiding idea is to **help you understand code you did not write yourself, quickly and with
confidence**. It was built first for Laravel (PHP) projects, so code navigation and the
project-aware views target PHP; diffs, blame, history and comments work for any repository.

It is read-only by design: everything comes from git objects, the working copy is never checked
out or modified, and the app writes into a repository only when you explicitly export comments.

## Main features

- **Commit-by-commit diffs**: side-by-side or unified, with syntax highlighting, word-level
  changes, moved-block detection, ignore-whitespace, rename tracking and merge-commit support.
- **Code navigation in the right version**: go to definition, find references and browse any
  file, resolved against the parent or the commit depending on which side you clicked.
- **Authorship**: a blame gutter with author vs committer, AI-assisted commits marked, and line
  or function history back to the original author.
- **Review workflow**: line comments with threads and status that follow the code across later
  commits, reviewed marks with progress, and range reviews (branch vs `main`, like a pull
  request).
- **Hand-off to Claude Code**: export open comments as `REVIEW_FEEDBACK.md` or a prompt, and see
  the new commits appear live as Claude Code works.
- **Quality signals**: possible secrets, complexity changes per function, test coverage of added
  lines, and only the PHPStan/Pint warnings a commit introduced.
- **History tools**: pickaxe search (when did this text appear?), and a bisect helper that never
  checks anything out.
- **Laravel views**: database schema at any commit rebuilt from migrations, `composer.lock`
  package changes and advisories, route changes, and risk badges for commits touching
  migrations, routes, config or dependencies.
- **Comfort**: several repositories in tabs, bookmarks, light/dark themes, and keyboard
  shortcuts for everything (press `?` in the app).

The sections below describe each feature in detail, in the order they were built.

## Phase 1 (browse and diff)

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

## Phase 2 (understanding the code)

- **Go to definition** (Cmd/Ctrl+click or F12) for PHP classes, interfaces, traits, enums,
  functions, methods, properties and constants. It uses a tree-sitter index of the version you
  clicked: the left pane resolves against the parent, the right pane against the commit. Results
  appear in a peek panel under the diff; `o` opens the file at that line.
- **Find references** (Shift+F12) in the same version. Calls on receivers of unknown type
  (`$user->save()`) are listed as name-only matches.
- **All files** (`t`): browse any file at the selected commit, not only changed ones.
- **Blame gutter** (`b`) on both panes: author initials with a colour per person, `*` when the
  committer differs, `^` for AI-assisted commits. Hover for details, click to open the commit.
  Shift+B switches to committer mode. The Blame tab in the peek panel shows the cursor line and
  the share of lines per person. Blame honours `.git-blame-ignore-revs` (read from HEAD),
  `.mailmap`, and move/copy detection.

Resolution is name-based (imports, namespaces, `$this`/`self`/`parent`, inheritance in the
index); type-aware navigation is planned via a language server in Phase 4.

## Phase 3 (reviewing)

- **Comments**: select lines in either pane (drag, or Shift+click) and press `c`. Threads have
  Markdown replies and Open / Resolved / Won't fix status, and show as numbered badges in the
  gutter. The Comments tab of the peek panel lists them by file, by review, or for the whole
  repository; `.` and `,` step through them.
- **Re-anchoring**: each thread keeps a snapshot of its lines and context. In later commits it
  follows the code (moved, re-indented or lightly edited); if the code is gone it is shown as
  **outdated**, never dropped.
- **Review state**: "reviewed" marks per file, per commit or per range, with progress.
- **Range review**: Compare… reviews `base...tip` (from the merge base, like a pull request),
  `base..tip`, or the current branch against the base branch.
- **Export**: write open comments to `REVIEW_FEEDBACK.md` (in the repository root by default;
  configurable) as instructions for Claude Code, copy them as a prompt, or save a review report.
  Writing the feedback file is the only thing the app ever writes into a repository, and only
  when you ask.
- **Live refresh**: new commits (e.g. from a Claude Code session) appear automatically.
- **Settings** per repository: base branch, ignored paths, feedback file, live refresh.

Comments and review state live in SQLite in the app data directory.

## Phase 4 (quality signals)

The Quality tab (`q`) and gutter markers show:

- **Possible secrets** added by the commit (AWS/GitHub/Stripe/Slack/Google keys, private keys,
  JWTs, Laravel `APP_KEY`, and random-looking values assigned to `*_SECRET`, `*PASSWORD`,
  `*TOKEN`, `*API_KEY`…), with a banner on the commit. Values are shown masked.
- **Complexity changes** per function: lines and cyclomatic complexity, parent vs commit.
- **Coverage** from a Clover XML report (`coverage.xml` by default; set in Settings): added
  lines that tests don't run get a dashed bar (`v` toggles it).
- **New warnings only**: PHPStan and Pint run on the changed files of both versions; only
  warnings the commit introduced are shown, inline with `!`.
- **Precise navigation** via a language server (Phpactor by default) for go to definition and
  references, falling back to the tree-sitter index when it fails.

PHPStan, Pint, the language server and `laravel-ide-helper` execute code from the repository, so
they only run for repositories marked **trusted** in Settings, and analysis runs on demand.
They run in a snapshot of the commit written to the app's cache directory (your repository is
not modified); `vendor/`, `node_modules/` and `.env` are linked from the working copy, and the
app warns when the commit's `composer.lock` differs from the working copy's. Secret detection,
complexity and coverage never run project code.

## Phase 5 (history and Laravel views)

- **Pickaxe** (History tab): which commits added or removed a piece of text or a regex
  (`git log -S/-G`), with +/− per file.
- **Line and function history** (`h`): every commit that changed the selected lines, or the
  function at the cursor (`git log -L`, follows moves and renames), and who originally wrote it.
- **Bisect** (toolbar): mark commits good/bad/skip; the app narrows the candidates and suggests
  the next commit to test. Nothing is checked out.
- **Risk badges** in the commit list: migrations, routes, config, `.env.example`, dependencies.
- **Laravel tab** (`l`):
  - *Schema* at the commit, rebuilt from `database/migrations` (create/table/drop/rename,
    column helpers, renames, drops, indexes, foreign keys), and what the commit changed.
    Migrations are read, never run; raw SQL and conditionals are not interpreted.
  - *Packages*: `composer.lock` changes, with major upgrades and downgrades flagged; an opt-in
    button runs `composer audit --no-plugins` (contacts Packagist, runs no project code).
  - *Routes*: added/removed/changed routes (groups, prefixes, names, middleware, resources),
    read statically from `routes/*.php`; for trusted repositories `php artisan route:list` gives
    exact results. Routes link to their controller method and to tests named after the controller.

## Phase 6 (comfort)

- **Tabs**: several repositories at once (Cmd/Ctrl+T, Cmd/Ctrl+W, Ctrl+Tab). Each tab keeps its
  own selection, history and caches; open tabs are restored at the next start.
- **Bookmarks** (Cmd/Ctrl+D on a line): named places at a version, with an editable note, listed
  in the Bookmarks tab (Cmd/Ctrl+Shift+D).
- **Preferences** (Cmd/Ctrl+,): light/dark/system theme, interface zoom (also Cmd/Ctrl +/−),
  code font size, and defaults for diff layout, blame and coverage.

## Sharing comments

Export → "Share comments to .reviews/" writes one Markdown file per thread (front matter, the
commented code, the replies) into `.reviews/` in the repository, so threads can be committed and
read in a pull request. "Import comments from .reviews/" merges files from teammates: new
threads and replies are added, the most recently updated status wins, nothing is duplicated.
SQLite stays the source of truth; files are only written when you ask.

## Diagnostics

```sh
# Every signal for one commit (add --tools, --audit, --lsp "phpactor language-server")
cargo run -p review-core --example check_repo -- /path/to/repo [commit]
# Timings for opening and browsing a repository
cargo run --release -p review-core --example bench -- /path/to/repo
```

Measured on a generated repository with 50,000 commits and 2,000 PHP files: first 300 commits
in 30 ms, whole history in 0.2 s, first diff in about 1 ms, symbol index in 51 ms.

## Packaging

Installers for macOS (`.app`, `.dmg`), Windows (`.msi`, NSIS) and Linux (`.AppImage`, `.deb`) are
built with cargo-packager; signing, notarization and the optional auto-updater are described in
[docs/RELEASING.md](docs/RELEASING.md).

Everything is read from git objects; the working copy is never touched. Blame runs
`git blame --porcelain -M -C`, so the `git` command must be installed.

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
| `crates/core` (`review-core`) | `git` (gix: commits, trees, renames, blobs, mailmap), `blame` (git CLI), `symbols` (tree-sitter PHP index), `diff` (imara-diff, alignment, word diff, moves, content classification), `file_view`, `highlight` (syntect), `settings` (SQLite), `tasks` (worker pool, commit pager, LRU cache), `fixture` |
| `crates/app` (`simple-review`) | eframe/egui binary: state, navigation history, widgets |

## Tests

```sh
cargo test                     # unit tests, fixture-repo tests, headless UI smoke test
SNAPSHOT_DIR=/tmp cargo test -p simple-review   # also saves screenshots of the UI
```

## License

Released under the [MIT License](LICENSE).

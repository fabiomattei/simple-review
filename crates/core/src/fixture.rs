//! Builds a small Laravel-shaped repository with a known history, for tests and manual trying.
//!
//! Run `cargo run -p review-core --example make_fixture -- <dir>` to create one on disk.
//! Requires the `git` CLI. Only ever writes inside the target directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Commit ids of the fixture, by the names listed in [`build`].
#[derive(Debug, Clone)]
pub struct Fixture {
    pub path: PathBuf,
    pub commits: BTreeMap<&'static str, String>,
}

impl Fixture {
    pub fn commit(&self, name: &str) -> &str {
        self.commits.get(name).unwrap_or_else(|| panic!("no fixture commit '{name}'"))
    }
}

struct Builder {
    dir: PathBuf,
    day: u32,
}

#[derive(Clone, Copy)]
struct Person {
    name: &'static str,
    email: &'static str,
}

const ALICE: Person = Person { name: "Alice Admin", email: "alice@example.com" };
const BOB: Person = Person { name: "Bob Backend", email: "bob@example.com" };
const CAROL: Person = Person { name: "Carol Coder", email: "carol@example.com" };
const PINT: Person = Person { name: "Pint Bot", email: "pint@example.com" };

impl Builder {
    fn git(&self, args: &[&str], author: Person, committer: Person) -> Result<String> {
        let date = format!("2024-01-{:02}T10:00:00+01:00", self.day);
        let committer_date = if author.email == committer.email {
            date.clone()
        } else {
            format!("2024-01-{:02}T18:30:00+01:00", self.day)
        };
        let out = Command::new("git")
            .args(["-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
            .args(args)
            .current_dir(&self.dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", author.name)
            .env("GIT_AUTHOR_EMAIL", author.email)
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_NAME", committer.name)
            .env("GIT_COMMITTER_EMAIL", committer.email)
            .env("GIT_COMMITTER_DATE", &committer_date)
            .output()
            .context("running git (is it installed?)")?;
        if !out.status.success() && args[0] != "merge" {
            bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn write(&self, path: &str, content: impl AsRef<[u8]>) -> Result<()> {
        let full = self.dir.join(path);
        std::fs::create_dir_all(full.parent().expect("file has a parent"))?;
        std::fs::write(full, content)?;
        Ok(())
    }

    fn commit(&mut self, message: &str, author: Person, committer: Person) -> Result<String> {
        self.day += 1;
        self.git(&["add", "-A"], author, committer)?;
        self.git(&["commit", "-q", "-m", message], author, committer)?;
        self.git(&["rev-parse", "HEAD"], author, committer)
    }
}

const USER_CONTROLLER: &str = r#"<?php

namespace App\Http\Controllers;

use App\Models\User;

class UserController extends Controller
{
    public function index()
    {
        return User::all();
    }

    public function show(User $user)
    {
        return $user;
    }
}
"#;

const INVOICE_SERVICE: &str = r#"<?php

namespace App\Services;

use App\Models\User;

class InvoiceService
{
    public function total(array $lines): float
    {
        $sum = 0;
        foreach ($lines as $line) {
            $sum += $line['price'] * $line['quantity'];
        }
        return $this->round($sum);
    }

    private function round(float $value): float
    {
        $rounded = round($value, 2);
        $normalized = max($rounded, 0.0);
        return $normalized;
    }

    public function forUser(User $user): array
    {
        return $user->invoices()->get()->all();
    }
}
"#;

/// The same class with `round` moved below `forUser`.
const INVOICE_SERVICE_MOVED: &str = r#"<?php

namespace App\Services;

use App\Models\User;

class InvoiceService
{
    public function total(array $lines): float
    {
        $sum = 0;
        foreach ($lines as $line) {
            $sum += $line['price'] * $line['quantity'];
        }
        return $this->round($sum);
    }

    public function forUser(User $user): array
    {
        return $user->invoices()->get()->all();
    }

    private function round(float $value): float
    {
        $rounded = round($value, 2);
        $normalized = max($rounded, 0.0);
        return $normalized;
    }
}
"#;

fn config(timezone: &str) -> String {
    format!("<?php\n\nreturn [\n    'name' => env('APP_NAME', 'Laravel'),\n    'timezone' => '{timezone}',\n    'locale' => 'en',\n];\n")
}

const ROUTES: &str = "<?php\n\nuse Illuminate\\Support\\Facades\\Route;\n\nRoute::get('/users', [UserController::class, 'index']);\nRoute::get('/users/{user}', [UserController::class, 'show']);\n";

/// Build the fixture in `dir`, which must be empty or not exist.
///
/// Commits on `main`, oldest first:
/// `initial`, `invoice` (adds a binary), `rename` (rename + edit), `move` (moved block),
/// `pint` (whitespace-only reformat), `ignore_revs` (adds `.git-blame-ignore-revs`),
/// `delete` (removes a file), `rebased` (author ≠ committer), `ai` (Co-Authored-By: Claude),
/// `main_tz`, then `feature_tz` on a branch, `merge` (conflict resolved by hand), `crlf`.
pub fn build(dir: &Path) -> Result<Fixture> {
    std::fs::create_dir_all(dir)?;
    if std::fs::read_dir(dir)?.next().is_some() {
        bail!("{} is not empty", dir.display());
    }
    let mut b = Builder { dir: dir.to_path_buf(), day: 0 };
    let mut commits = BTreeMap::new();
    b.git(&["init", "-q"], ALICE, ALICE)?;

    b.write("composer.json", "{\n    \"name\": \"acme/shop\",\n    \"require\": {\n        \"laravel/framework\": \"^11.0\"\n    }\n}\n")?;
    b.write("app/Models/User.php", "<?php\n\nnamespace App\\Models;\n\nclass User extends Model\n{\n    protected $fillable = ['name', 'email'];\n}\n")?;
    b.write("app/Http/Controllers/UserController.php", USER_CONTROLLER)?;
    b.write("app/Helpers/legacy.php", "<?php\n\nfunction legacy_format($v)\n{\n    return trim($v);\n}\n")?;
    b.write("config/app.php", config("UTC"))?;
    b.write("routes/web.php", ROUTES)?;
    b.write(".mailmap", "Bob Backend <bob@example.com> <bob@old-laptop.local>\n")?;
    commits.insert("initial", b.commit("Initial Laravel skeleton", ALICE, ALICE)?);

    b.write("app/Services/InvoiceService.php", INVOICE_SERVICE)?;
    b.write("public/logo.png", [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D', b'R'])?;
    b.write("storage/app/data.bin", [0u8, 1, 2, 3, 255, 254])?;
    commits.insert("invoice", b.commit("Add invoice service and logo", BOB, BOB)?);

    b.git(&["mv", "app/Http/Controllers/UserController.php", "app/Http/Controllers/AccountController.php"], ALICE, ALICE)?;
    b.write(
        "app/Http/Controllers/AccountController.php",
        USER_CONTROLLER.replace("class UserController", "class AccountController"),
    )?;
    commits.insert("rename", b.commit("Rename UserController to AccountController", ALICE, ALICE)?);

    b.write("app/Services/InvoiceService.php", INVOICE_SERVICE_MOVED)?;
    commits.insert("move", b.commit("Move round() below forUser()", BOB, BOB)?);

    b.write("app/Services/InvoiceService.php", INVOICE_SERVICE_MOVED.replace("        $sum = 0;", "        $sum    =  0;").replace("    {\n        return $user", "    {\n\n        return $user"))?;
    b.write("app/Models/User.php", "<?php\n\nnamespace App\\Models;\n\nclass User extends Model\n{\n    protected $fillable = [\n        'name',\n        'email',\n    ];\n}\n")?;
    commits.insert("pint", b.commit("Apply Pint formatting", PINT, PINT)?);

    b.write(".git-blame-ignore-revs", format!("# Pint formatting\n{}\n", commits["pint"]))?;
    commits.insert("ignore_revs", b.commit("Ignore formatting commit in blame", ALICE, ALICE)?);

    std::fs::remove_file(dir.join("app/Helpers/legacy.php"))?;
    commits.insert("delete", b.commit("Remove legacy helper", BOB, BOB)?);

    b.write("routes/web.php", ROUTES.replace("UserController", "AccountController"))?;
    commits.insert("rebased", b.commit("Point routes at AccountController", CAROL, ALICE)?);

    b.write(
        "app/Services/InvoiceService.php",
        INVOICE_SERVICE_MOVED.replace(
            "        return $this->round($sum);",
            "        $sum -= $this->discount($sum);\n        return $this->round($sum);",
        ).replace(
            "    public function forUser",
            "    private function discount(float $sum): float\n    {\n        return $sum > 100 ? $sum * 0.1 : 0.0;\n    }\n\n    public function forUser",
        ),
    )?;
    commits.insert(
        "ai",
        b.commit(
            "Add volume discount\n\nTen percent off orders above 100.\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>",
            ALICE,
            ALICE,
        )?,
    );

    b.git(&["branch", "feature"], ALICE, ALICE)?;
    b.write("config/app.php", config("Europe/Rome"))?;
    commits.insert("main_tz", b.commit("Use Rome timezone", ALICE, ALICE)?);

    b.git(&["checkout", "-q", "feature"], BOB, BOB)?;
    b.write("config/app.php", config("Europe/Paris"))?;
    b.write("app/Models/User.php", "<?php\n\nnamespace App\\Models;\n\nclass User extends Model\n{\n    protected $fillable = [\n        'name',\n        'email',\n        'timezone',\n    ];\n}\n")?;
    commits.insert("feature_tz", b.commit("Use Paris timezone and store user timezone", BOB, BOB)?);

    b.git(&["checkout", "-q", "main"], ALICE, ALICE)?;
    b.day += 1;
    b.git(&["merge", "-q", "--no-ff", "feature", "-m", "Merge branch 'feature'"], ALICE, ALICE)?;
    b.write("config/app.php", config("Europe/Berlin"))?;
    commits.insert("merge", b.commit("Merge branch 'feature'\n\nResolved timezone conflict.", ALICE, ALICE)?);

    b.write("routes/web.php", ROUTES.replace("UserController", "AccountController").replace('\n', "\r\n"))?;
    commits.insert("crlf", b.commit("Convert routes to CRLF", BOB, BOB)?);

    Ok(Fixture { path: dir.to_path_buf(), commits })
}

//! Changes that deserve extra attention in a Laravel project.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Risk {
    Migration,
    Routes,
    Config,
    EnvExample,
    Dependencies,
}

impl Risk {
    pub fn label(self) -> &'static str {
        match self {
            Risk::Migration => "migration",
            Risk::Routes => "routes",
            Risk::Config => "config",
            Risk::EnvExample => ".env",
            Risk::Dependencies => "deps",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Risk::Migration => "Changes the database schema (database/migrations)",
            Risk::Routes => "Changes routes (routes/)",
            Risk::Config => "Changes configuration (config/)",
            Risk::EnvExample => "Changes .env.example: new or changed environment variables",
            Risk::Dependencies => "Changes Composer dependencies (composer.json / composer.lock)",
        }
    }
}

pub fn risk_of(path: &str) -> Option<Risk> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if path.starts_with("database/migrations/") {
        Some(Risk::Migration)
    } else if path.starts_with("routes/") {
        Some(Risk::Routes)
    } else if path.starts_with("config/") {
        Some(Risk::Config)
    } else if name == ".env.example" {
        Some(Risk::EnvExample)
    } else if path == "composer.json" || path == "composer.lock" {
        Some(Risk::Dependencies)
    } else {
        None
    }
}

/// Distinct risks of a set of changed paths, in a stable order.
pub fn risks<'a>(paths: impl IntoIterator<Item = &'a str>) -> Vec<Risk> {
    let mut out: Vec<Risk> = paths.into_iter().filter_map(risk_of).collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_paths() {
        let r = risks([
            "database/migrations/2024_01_01_create_users_table.php",
            "routes/api.php",
            "config/app.php",
            ".env.example",
            "composer.lock",
            "app/Models/User.php",
            "routes/web.php",
        ]);
        assert_eq!(r, vec![Risk::Migration, Risk::Routes, Risk::Config, Risk::EnvExample, Risk::Dependencies]);
        assert!(risks(["app/Http/Kernel.php"]).is_empty());
    }
}

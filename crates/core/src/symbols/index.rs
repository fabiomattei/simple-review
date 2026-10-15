//! A symbol index for one commit's tree, and definition/reference lookup in it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use gix::ObjectId;

use super::parse::{FileSymbols, RefKind, Reference, Symbol, SymbolKind, is_php_path, parse_php};
use crate::git::{EntryType, Repo};

/// Parsed files by blob id, shared between indexes: unchanged files are parsed once.
#[derive(Clone, Default)]
pub struct ParseCache(Arc<Mutex<HashMap<ObjectId, Arc<FileSymbols>>>>);

const PARSE_CACHE_LIMIT: usize = 100_000;

impl ParseCache {
    fn get(&self, id: &ObjectId) -> Option<Arc<FileSymbols>> {
        self.0.lock().unwrap().get(id).cloned()
    }

    fn insert(&self, id: ObjectId, symbols: Arc<FileSymbols>) {
        let mut map = self.0.lock().unwrap();
        if map.len() >= PARSE_CACHE_LIMIT {
            map.clear();
        }
        map.insert(id, symbols);
    }
}

/// A definition and the file it is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub path: String,
    pub symbol: Symbol,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefHit {
    pub path: String,
    pub reference: Reference,
    /// False when the receiver's class is unknown (`$user->save()`), so the match is by name only.
    pub certain: bool,
}

/// Result of a go-to-definition lookup.
#[derive(Debug, Clone, Default)]
pub struct Lookup {
    /// The identifier that was clicked.
    pub name: String,
    pub candidates: Vec<Located>,
    /// The clicked position is itself a definition.
    pub on_definition: bool,
}

pub struct SymbolIndex {
    pub commit: ObjectId,
    files: HashMap<String, Arc<FileSymbols>>,
    /// lower-case name -> (path, symbol index)
    by_name: HashMap<String, Vec<(String, usize)>>,
}

impl SymbolIndex {
    /// Parse every PHP file in `commit` for which `include` returns true.
    pub fn build(
        repo: &Repo,
        commit: ObjectId,
        include: &(dyn Fn(&str) -> bool + Sync),
        cache: &ParseCache,
        progress: &(dyn Fn(f32) + Sync),
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Option<Self>> {
        let files: Vec<_> = repo
            .list_files(commit)?
            .into_iter()
            .filter(|f| f.entry != EntryType::Submodule && is_php_path(&f.path) && include(&f.path))
            .collect();
        let total = files.len().max(1);
        let done = std::sync::atomic::AtomicUsize::new(0);
        let workers = std::thread::available_parallelism().map_or(2, |n| n.get()).min(8);
        let chunk = files.len().div_ceil(workers).max(1);
        let parsed: Vec<(String, Arc<FileSymbols>)> = std::thread::scope(|s| {
            let handles: Vec<_> = files
                .chunks(chunk)
                .map(|part| {
                    let done = &done;
                    s.spawn(move || {
                        let mut out = Vec::with_capacity(part.len());
                        for f in part {
                            if cancelled() {
                                break;
                            }
                            let symbols = match cache.get(&f.id) {
                                Some(s) => s,
                                None => {
                                    let bytes = repo.read_blob(f.id).unwrap_or_default();
                                    let text = String::from_utf8_lossy(&bytes);
                                    let s = Arc::new(parse_php(&text));
                                    cache.insert(f.id, s.clone());
                                    s
                                }
                            };
                            out.push((f.path.clone(), symbols));
                            let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                            if n.is_multiple_of(50) {
                                progress(n as f32 / total as f32);
                            }
                        }
                        out
                    })
                })
                .collect();
            handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
        });
        if cancelled() {
            return Ok(None);
        }
        Ok(Some(Self::from_files(commit, parsed)))
    }

    pub fn from_files(commit: ObjectId, files: Vec<(String, Arc<FileSymbols>)>) -> Self {
        let mut by_name: HashMap<String, Vec<(String, usize)>> = HashMap::new();
        for (path, f) in &files {
            for (i, s) in f.symbols.iter().enumerate() {
                by_name.entry(s.name.to_ascii_lowercase()).or_default().push((path.clone(), i));
            }
        }
        SymbolIndex { commit, files: files.into_iter().collect(), by_name }
    }

    pub fn file(&self, path: &str) -> Option<&FileSymbols> {
        self.files.get(path).map(Arc::as_ref)
    }

    /// All indexed files with their symbols.
    pub fn files(&self) -> impl Iterator<Item = (&String, &FileSymbols)> {
        self.files.iter().map(|(p, f)| (p, f.as_ref()))
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    fn named(&self, name: &str) -> impl Iterator<Item = Located> + '_ {
        self.by_name.get(&name.to_ascii_lowercase()).into_iter().flatten().map(|(path, i)| Located {
            path: path.clone(),
            symbol: self.files[path].symbols[*i].clone(),
        })
    }

    fn class(&self, fqn: &str) -> Option<Located> {
        let short = fqn.rsplit('\\').next().unwrap_or(fqn);
        self.named(short).find(|l| l.symbol.kind.is_class_like() && l.symbol.fqn.eq_ignore_ascii_case(fqn))
    }

    /// `class` and its ancestors (parents, interfaces, traits), nearest first.
    fn lineage(&self, class: &str) -> Vec<String> {
        let mut out = vec![class.to_string()];
        let mut i = 0;
        while i < out.len() && out.len() < 32 {
            if let Some(c) = self.class(&out[i]) {
                for s in c.symbol.supertypes {
                    if !out.iter().any(|o| o.eq_ignore_ascii_case(&s)) {
                        out.push(s);
                    }
                }
            }
            i += 1;
        }
        out
    }

    fn members(&self, name: &str, kinds: &[SymbolKind], class: Option<&str>) -> Vec<Located> {
        let all: Vec<Located> = self.named(name).filter(|l| kinds.contains(&l.symbol.kind)).collect();
        if let Some(class) = class {
            for ancestor in self.lineage(class) {
                let found: Vec<_> = all
                    .iter()
                    .filter(|l| l.symbol.container.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(&ancestor)))
                    .cloned()
                    .collect();
                if !found.is_empty() {
                    return found;
                }
            }
        }
        all
    }

    fn resolve(&self, r: &Reference) -> Vec<Located> {
        match &r.kind {
            RefKind::Class { fqn } => match self.class(fqn) {
                Some(c) => vec![c],
                None => self.named(&r.name).filter(|l| l.symbol.kind.is_class_like()).collect(),
            },
            RefKind::Function { candidates } => {
                let all: Vec<Located> = self.named(&r.name).filter(|l| l.symbol.kind == SymbolKind::Function).collect();
                for c in candidates {
                    let exact: Vec<_> = all.iter().filter(|l| l.symbol.fqn.eq_ignore_ascii_case(c)).cloned().collect();
                    if !exact.is_empty() {
                        return exact;
                    }
                }
                all
            }
            RefKind::Method { class } => self.members(&r.name, &[SymbolKind::Method], class.as_deref()),
            RefKind::Property { class } => self.members(&r.name, &[SymbolKind::Property], class.as_deref()),
            RefKind::Constant { class } => {
                self.members(&r.name, &[SymbolKind::Constant, SymbolKind::EnumCase], class.as_deref())
            }
        }
    }

    /// Go to definition from a position in `path` (0-based line, byte column).
    pub fn definition_at(&self, path: &str, line: u32, col: u32) -> Option<Lookup> {
        let file = self.file(path)?;
        if let Some(sym) = file.symbol_at(line, col) {
            return Some(Lookup {
                name: sym.name.clone(),
                candidates: vec![Located { path: path.to_string(), symbol: sym.clone() }],
                on_definition: true,
            });
        }
        let r = file.reference_at(line, col)?;
        let mut candidates = self.resolve(r);
        // Same file first, then by path, for a stable order.
        candidates.sort_by_key(|l| (l.path != path, l.path.clone(), l.symbol.span.line));
        Some(Lookup { name: r.name.clone(), candidates, on_definition: false })
    }

    /// All references to `target` in this version.
    pub fn references(&self, target: &Located) -> Vec<RefHit> {
        let sym = &target.symbol;
        let mut hits = Vec::new();
        let mut paths: Vec<&String> = self.files.keys().collect();
        paths.sort();
        for path in paths {
            for r in &self.files[path].refs {
                if !r.name.eq_ignore_ascii_case(&sym.name) {
                    continue;
                }
                let verdict = match (&r.kind, sym.kind) {
                    (RefKind::Class { fqn }, k) if k.is_class_like() => fqn.eq_ignore_ascii_case(&sym.fqn).then_some(true),
                    (RefKind::Function { candidates }, SymbolKind::Function) => {
                        // Only count it if this definition is what the call resolves to.
                        self.resolve(r).iter().any(|l| l.symbol.fqn == sym.fqn).then_some(candidates.iter().any(|c| c.eq_ignore_ascii_case(&sym.fqn)))
                    }
                    (RefKind::Method { class }, SymbolKind::Method)
                    | (RefKind::Property { class }, SymbolKind::Property)
                    | (RefKind::Constant { class }, SymbolKind::Constant | SymbolKind::EnumCase) => match class {
                        Some(c) => self.resolve(r).iter().any(|l| l.symbol.fqn.eq_ignore_ascii_case(&sym.fqn)).then_some(true).or_else(|| {
                            // The receiver's class may be a subclass that inherits the member.
                            self.lineage(c).iter().any(|a| sym.container.as_deref().is_some_and(|sc| sc.eq_ignore_ascii_case(a))).then_some(true)
                        }),
                        None => Some(false),
                    },
                    _ => None,
                };
                if let Some(certain) = verdict {
                    hits.push(RefHit { path: path.clone(), reference: r.clone(), certain });
                }
            }
        }
        hits.sort_by_key(|h| (!h.certain, h.path.clone(), h.reference.span.line));
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(files: &[(&str, &str)]) -> SymbolIndex {
        let parsed = files.iter().map(|(p, src)| (p.to_string(), Arc::new(parse_php(src)))).collect();
        SymbolIndex::from_files(ObjectId::null(gix::hash::Kind::Sha1), parsed)
    }

    fn pos(src: &str, needle: &str, nth: usize) -> (u32, u32) {
        src.lines()
            .enumerate()
            .flat_map(|(i, line)| line.match_indices(needle).map(move |(c, _)| (i as u32, c as u32 + 1)))
            .nth(nth)
            .unwrap_or_else(|| panic!("{needle} #{nth} not found"))
    }

    const BASE: &str = "<?php\nnamespace App;\nabstract class Base {\n    public function save(): void {}\n}\n";
    const USER: &str = "<?php\nnamespace App;\nclass User extends Base {\n    public function name(): string { return ''; }\n}\n";
    const CTRL: &str = "<?php\nnamespace App\\Http;\nuse App\\User;\nclass Ctrl {\n    public function show(User $u) {\n        $u->save();\n        $u->name();\n        User::query();\n        return new User();\n    }\n}\n";

    #[test]
    fn goes_to_class_and_inherited_method() {
        let idx = index(&[("app/Base.php", BASE), ("app/User.php", USER), ("app/Http/Ctrl.php", CTRL)]);
        let (l, c) = pos(CTRL, "User", 2);
        let lookup = idx.definition_at("app/Http/Ctrl.php", l, c).unwrap();
        assert_eq!(lookup.candidates.len(), 1);
        assert_eq!(lookup.candidates[0].path, "app/User.php");
        assert_eq!(lookup.candidates[0].symbol.fqn, "App\\User");

        // `$u->save()`: receiver unknown, matched by name.
        let (l, c) = pos(CTRL, "save", 0);
        let lookup = idx.definition_at("app/Http/Ctrl.php", l, c).unwrap();
        assert_eq!(lookup.candidates[0].symbol.fqn, "App\\Base::save");

        // Clicking the definition itself.
        let (l, c) = pos(BASE, "save", 0);
        assert!(idx.definition_at("app/Base.php", l, c).unwrap().on_definition);
    }

    #[test]
    fn finds_references() {
        let idx = index(&[("app/Base.php", BASE), ("app/User.php", USER), ("app/Http/Ctrl.php", CTRL)]);
        let (l, c) = pos(USER, "User", 0);
        let user = idx.definition_at("app/User.php", l, c).unwrap().candidates.remove(0);
        let hits = idx.references(&user);
        assert_eq!(hits.len(), 4, "{hits:#?}"); // use, type hint, User::query, new User
        assert!(hits.iter().all(|h| h.certain && h.path == "app/Http/Ctrl.php"));

        let (l, c) = pos(BASE, "save", 0);
        let save = idx.definition_at("app/Base.php", l, c).unwrap().candidates.remove(0);
        let hits = idx.references(&save);
        assert_eq!(hits.len(), 1);
        assert!(!hits[0].certain);
    }
}

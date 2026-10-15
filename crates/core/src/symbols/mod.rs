//! PHP symbol index built with tree-sitter: go to definition and find references, per commit.

pub mod index;
pub mod parse;

pub use index::{Located, Lookup, ParseCache, RefHit, SymbolIndex};
pub use parse::{FileSymbols, Symbol, SymbolKind, is_php_path};

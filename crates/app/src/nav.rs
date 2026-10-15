//! Back/forward navigation history.

use review_core::ObjectId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub commit: ObjectId,
    pub parent_idx: usize,
    pub path: Option<String>,
    /// Set when `path` was opened as a whole file at this commit rather than as a diff.
    pub browse: Option<ObjectId>,
    /// Range base when the location is a whole-range diff.
    pub base: Option<ObjectId>,
}

#[derive(Default)]
pub struct History {
    entries: Vec<Location>,
    /// Index of the current entry.
    pos: usize,
}

const MAX_ENTRIES: usize = 200;

impl History {
    /// Record a new location, dropping any forward entries.
    pub fn push(&mut self, loc: Location) {
        if self.entries.get(self.pos) == Some(&loc) {
            return;
        }
        // Selecting a file in the commit we just navigated to refines that entry instead of adding one.
        if let Some(cur) = self.entries.get_mut(self.pos)
            && cur.commit == loc.commit
            && cur.parent_idx == loc.parent_idx
            && cur.path.is_none()
            && loc.browse.is_none()
        {
            *cur = loc;
            self.entries.truncate(self.pos + 1);
            return;
        }
        if !self.entries.is_empty() {
            self.entries.truncate(self.pos + 1);
        }
        self.entries.push(loc);
        if self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
        self.pos = self.entries.len() - 1;
    }

    pub fn back(&mut self) -> Option<Location> {
        if self.pos == 0 || self.entries.is_empty() {
            return None;
        }
        self.pos -= 1;
        self.entries.get(self.pos).cloned()
    }

    pub fn forward(&mut self) -> Option<Location> {
        if self.pos + 1 >= self.entries.len() {
            return None;
        }
        self.pos += 1;
        self.entries.get(self.pos).cloned()
    }

    pub fn can_back(&self) -> bool {
        self.pos > 0
    }

    pub fn can_forward(&self) -> bool {
        self.pos + 1 < self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(n: u8, path: Option<&str>) -> Location {
        Location { commit: ObjectId::from_bytes_or_panic(&[n; 20]), parent_idx: 0, path: path.map(String::from), browse: None, base: None }
    }

    #[test]
    fn back_and_forward() {
        let mut h = History::default();
        h.push(loc(1, None));
        h.push(loc(1, Some("a")));
        h.push(loc(2, None));
        h.push(loc(2, Some("b")));
        h.push(loc(2, Some("c")));
        assert_eq!(h.back(), Some(loc(2, Some("b"))));
        assert_eq!(h.back(), Some(loc(1, Some("a"))));
        assert_eq!(h.back(), None);
        assert_eq!(h.forward(), Some(loc(2, Some("b"))));
        h.push(loc(3, None));
        assert!(!h.can_forward());
        assert_eq!(h.back(), Some(loc(2, Some("b"))));
    }
}

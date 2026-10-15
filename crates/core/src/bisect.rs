//! A bisect helper that never touches the repository: you mark commits good or bad, it narrows
//! the candidates and suggests what to test next.

use std::collections::HashSet;
use std::ops::ControlFlow;

use anyhow::Result;
use gix::ObjectId;

use crate::git::Repo;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bisect {
    pub bad: Option<ObjectId>,
    pub good: Vec<ObjectId>,
    pub skip: HashSet<ObjectId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Mark a bad commit (and at least one good one) to start.
    NeedBad,
    NeedGood,
    /// Test this commit next; `remaining` commits could still be the first bad one.
    Next { commit: ObjectId, remaining: usize },
    /// Only this commit is left: it introduced the problem.
    Found(ObjectId),
    /// Only skipped commits are left.
    OnlySkipped(Vec<ObjectId>),
}

impl Bisect {
    pub fn is_active(&self) -> bool {
        self.bad.is_some() || !self.good.is_empty()
    }

    /// Commits reachable from `bad` but not from any good commit, newest first (the bad
    /// commit included).
    pub fn candidates(&self, repo: &Repo) -> Result<Vec<ObjectId>> {
        let Some(bad) = self.bad else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        repo.walk_range(bad, &self.good, |c| {
            out.push(c.id);
            ControlFlow::Continue(())
        })?;
        Ok(out)
    }

    pub fn verdict(&self, repo: &Repo) -> Result<Verdict> {
        if self.bad.is_none() {
            return Ok(Verdict::NeedBad);
        }
        if self.good.is_empty() {
            return Ok(Verdict::NeedGood);
        }
        let all = self.candidates(repo)?;
        Ok(next(&all, &self.skip))
    }
}

/// Choose the next commit to test from `candidates` (newest first; the first is known bad).
pub fn next(candidates: &[ObjectId], skip: &HashSet<ObjectId>) -> Verdict {
    match candidates {
        [] => Verdict::NeedGood,
        [only] => Verdict::Found(*only),
        _ => {
            // The bad commit itself needs no test; aim for the middle of the rest.
            let testable: Vec<(usize, ObjectId)> =
                candidates.iter().copied().enumerate().skip(1).filter(|(_, c)| !skip.contains(c)).collect();
            if testable.is_empty() {
                return Verdict::OnlySkipped(candidates[1..].to_vec());
            }
            let middle = candidates.len() / 2;
            let (_, commit) = *testable.iter().min_by_key(|(i, _)| i.abs_diff(middle)).expect("non-empty");
            Verdict::Next { commit, remaining: candidates.len() }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: u8) -> Vec<ObjectId> {
        (0..n).map(|i| ObjectId::from_bytes_or_panic(&[i + 1; 20])).collect()
    }

    #[test]
    fn halves_and_finds() {
        let c = ids(9);
        assert_eq!(next(&c, &HashSet::new()), Verdict::Next { commit: c[4], remaining: 9 });
        assert_eq!(next(&c[..1], &HashSet::new()), Verdict::Found(c[0]));
        let skip: HashSet<_> = [c[4]].into();
        let Verdict::Next { commit, .. } = next(&c, &skip) else { panic!() };
        assert!(commit == c[3] || commit == c[5]);
        let all: HashSet<_> = c[1..].iter().copied().collect();
        assert!(matches!(next(&c, &all), Verdict::OnlySkipped(_)));
    }
}

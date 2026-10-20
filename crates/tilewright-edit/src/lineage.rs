//! Where the files a transaction renames, or creates like another, come
//! from: the files whose permissions bound theirs when publishing.

use std::collections::BTreeMap;

/// One step back: a file came from the file at `from`, which in turn came
/// from `parent`'s, if that file was renamed or created like another too.
#[derive(Debug, Clone)]
struct Step {
    from: String,
    parent: Option<usize>,
    /// The first step of the chain, whose `from` was not made by the
    /// transaction from another file.
    origin: usize,
}

/// The chains of the files a transaction made from others. A chain is
/// shared by every file made from the same file, and each operation adds
/// at most one step, so the whole is as large as the transaction.
#[derive(Debug, Clone, Default)]
pub struct Lineage {
    steps: Vec<Step>,
    /// The last step of each path's current file, if it was made from
    /// another.
    current: BTreeMap<String, usize>,
}

impl Lineage {
    /// The file now at `to` was made from the one now at `from`, by
    /// renaming it or creating it like it.
    pub fn derive(&mut self, to: &str, from: &str) {
        let parent = self.current.get(from).copied();
        let step = self.steps.len();
        let origin = parent.map_or(step, |parent| self.steps[parent].origin);
        self.steps.push(Step {
            from: from.to_owned(),
            parent,
            origin,
        });
        self.current.insert(to.to_owned(), step);
    }

    /// The file at `path` is gone, or new and made from no other.
    pub fn forget(&mut self, path: &str) {
        self.current.remove(path);
    }

    /// Keeps only the chains of the paths `keep` accepts.
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.current.retain(|path, _| keep(path));
    }

    /// The last step of the file at `path`, if it was made from another.
    pub fn step(&self, path: &str) -> Option<usize> {
        self.current.get(path).copied()
    }

    /// The path a step names, and the step before it.
    pub fn at(&self, step: usize) -> (&str, Option<usize>) {
        let step = &self.steps[step];
        (&step.from, step.parent)
    }

    /// Each path whose file was made from another, with the path its chain
    /// starts from: the file it was first renamed or copied from.
    pub fn origins(&self) -> impl Iterator<Item = (&str, &str)> {
        self.current.iter().map(|(path, step)| {
            let origin = self.steps[*step].origin;
            (path.as_str(), self.steps[origin].from.as_str())
        })
    }
}

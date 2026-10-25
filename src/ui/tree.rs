use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    time::SystemTime,
};

use crate::scan::{ProjectInfo, SortKey};

#[derive(Default)]
pub(super) struct ProjectTree {
    pub roots: Vec<usize>,
    pub nodes: Vec<Node>,
}

pub(super) struct Node {
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub target_size: u64,
    pub count: usize,
    modified: SystemTime,
}

#[derive(Clone, Copy)]
pub(super) struct VisibleRow {
    pub index: usize,
    pub depth: usize,
}

impl ProjectTree {
    pub fn new(projects: &[ProjectInfo], sort: SortKey) -> Self {
        let by_root: HashMap<_, _> = projects
            .iter()
            .enumerate()
            .map(|(index, project)| (project.root.as_path(), index))
            .collect();
        let mut tree = Self {
            roots: Vec::new(),
            nodes: projects
                .iter()
                .map(|project| Node {
                    parent: project
                        .root
                        .ancestors()
                        .skip(1)
                        .find_map(|path| by_root.get(path).copied()),
                    children: Vec::new(),
                    target_size: project.target_size,
                    count: 1,
                    modified: project.last_modified,
                })
                .collect(),
        };
        for index in 0..projects.len() {
            if let Some(parent) = tree.nodes[index].parent {
                tree.nodes[parent].children.push(index);
            } else {
                tree.roots.push(index);
            }
        }
        let mut deepest_first: Vec<_> = (0..projects.len()).collect();
        deepest_first
            .sort_by_key(|&index| std::cmp::Reverse(projects[index].root.components().count()));
        for index in deepest_first {
            if let Some(parent) = tree.nodes[index].parent {
                tree.nodes[parent].target_size = tree.nodes[parent]
                    .target_size
                    .saturating_add(tree.nodes[index].target_size);
                tree.nodes[parent].count += tree.nodes[index].count;
                tree.nodes[parent].modified =
                    tree.nodes[parent].modified.max(tree.nodes[index].modified);
            }
        }
        let compare = |&left: &usize, &right: &usize| {
            let left_node = &tree.nodes[left];
            let right_node = &tree.nodes[right];
            let order = match sort {
                SortKey::Size => right_node
                    .target_size
                    .cmp(&left_node.target_size)
                    .then_with(|| projects[right].total_size.cmp(&projects[left].total_size)),
                SortKey::Modified => right_node
                    .modified
                    .cmp(&left_node.modified)
                    .then_with(|| right_node.target_size.cmp(&left_node.target_size)),
            };
            order.then_with(|| projects[left].root.cmp(&projects[right].root))
        };
        tree.roots.sort_by(compare);
        let children: Vec<_> = tree
            .nodes
            .iter()
            .map(|node| {
                let mut children = node.children.clone();
                children.sort_by(compare);
                children
            })
            .collect();
        for (node, children) in tree.nodes.iter_mut().zip(children) {
            node.children = children;
        }
        tree
    }

    pub fn visible(
        &self,
        projects: &[ProjectInfo],
        expanded: &HashSet<PathBuf>,
    ) -> Vec<VisibleRow> {
        let mut rows = Vec::new();
        let mut pending: Vec<_> = self
            .roots
            .iter()
            .rev()
            .map(|&index| VisibleRow { index, depth: 0 })
            .collect();
        while let Some(row) = pending.pop() {
            rows.push(row);
            if expanded.contains(&projects[row.index].root) {
                pending.extend(self.nodes[row.index].children.iter().rev().map(|&index| {
                    VisibleRow {
                        index,
                        depth: row.depth + 1,
                    }
                }));
            }
        }
        rows
    }

    pub fn members(&self, index: usize) -> Vec<usize> {
        let mut members = Vec::new();
        let mut pending = vec![index];
        while let Some(index) = pending.pop() {
            members.push(index);
            pending.extend(&self.nodes[index].children);
        }
        members
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(path: &str, target_size: u64) -> ProjectInfo {
        ProjectInfo {
            root: PathBuf::from(path),
            manifest_path: PathBuf::from(path).join("Cargo.toml"),
            name: path.into(),
            total_size: target_size + 100,
            target_size,
            last_modified: SystemTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn groups_by_nearest_ancestor_and_sorts_by_combined_targets() {
        let projects = vec![
            project("repo", 1),
            project("repo/crates/a", 100),
            project("other", 50),
            project("repo/crates/a/nested", 2),
        ];
        let tree = ProjectTree::new(&projects, SortKey::Size);
        assert_eq!(tree.roots, vec![0, 2]);
        assert_eq!(tree.nodes[0].target_size, 103);
        assert_eq!(tree.nodes[0].count, 3);
        assert_eq!(tree.nodes[3].parent, Some(1));
        let collapsed = tree.visible(&projects, &HashSet::new());
        assert_eq!(
            collapsed.iter().map(|row| row.index).collect::<Vec<_>>(),
            vec![0, 2]
        );
        let expanded = HashSet::from([PathBuf::from("repo"), PathBuf::from("repo/crates/a")]);
        let rows = tree.visible(&projects, &expanded);
        assert_eq!(
            rows.iter()
                .map(|row| (row.index, row.depth))
                .collect::<Vec<_>>(),
            vec![(0, 0), (1, 1), (3, 2), (2, 0)]
        );
    }
}

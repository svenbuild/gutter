use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use globset::{Glob, GlobMatcher};
use ignore::WalkBuilder;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub children: Vec<TreeNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleNode {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    pub is_parent_link: bool,
}

#[derive(Debug, Clone)]
pub struct WorkspaceState {
    pub root: PathBuf,
    pub root_name: String,
    root_node: TreeNode,
    expanded: BTreeSet<PathBuf>,
    visible_nodes: Vec<VisibleNode>,
    files: Vec<PathBuf>,
    selected: usize,
    scroll_offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeActivation {
    ToggleDirectory(PathBuf),
    OpenFile(PathBuf),
    ChangeRoot(PathBuf),
    None,
}

#[derive(Debug, Clone)]
struct EntryInfo {
    path: PathBuf,
    name: String,
    is_dir: bool,
}

#[derive(Debug, Clone)]
struct IgnoreRule {
    base: PathBuf,
    matcher: GlobMatcher,
    negated: bool,
    only_dir: bool,
    basename_only: bool,
}

impl WorkspaceState {
    pub fn load(root: PathBuf, show_hidden: bool) -> Result<Self> {
        let root = fs::canonicalize(&root).unwrap_or(root);
        let root_name = root
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| root.display().to_string());

        let mut entries = collect_entries(&root, show_hidden)?;
        let files = entries
            .iter()
            .filter(|entry| !entry.is_dir)
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();

        let root_node = build_tree(&root, &root_name, &mut entries);
        let mut expanded = BTreeSet::new();
        expanded.insert(root.clone());
        let mut state = Self {
            root,
            root_name,
            root_node,
            expanded,
            visible_nodes: Vec::new(),
            files,
            selected: 0,
            scroll_offset: 0,
        };
        state.rebuild_visible_nodes();
        Ok(state)
    }

    pub fn reload(&mut self, show_hidden: bool) -> Result<()> {
        let selected_path = self.selected_path().cloned();
        let expanded = self.expanded.clone();
        let mut reloaded = Self::load(self.root.clone(), show_hidden)?;
        reloaded.expanded = expanded;
        reloaded.rebuild_visible_nodes();
        if let Some(path) = selected_path {
            reloaded.select_path(&path);
        }
        *self = reloaded;
        Ok(())
    }

    pub fn visible_nodes(&self) -> &[VisibleNode] {
        &self.visible_nodes
    }

    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub fn selected_path(&self) -> Option<&PathBuf> {
        self.visible_nodes.get(self.selected).map(|node| &node.path)
    }

    pub fn selected_node(&self) -> Option<&VisibleNode> {
        self.visible_nodes.get(self.selected)
    }

    pub fn parent_root(&self) -> Option<PathBuf> {
        self.root.parent().map(Path::to_path_buf)
    }

    pub fn select_path(&mut self, path: &Path) {
        let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(index) = self.visible_nodes.iter().position(|node| node.path == path) {
            self.selected = index;
        }
    }

    pub fn reveal_path(&mut self, path: &Path, viewport_height: usize) {
        let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.expand_ancestors(&path);
        self.rebuild_visible_nodes();
        self.select_path(&path);
        self.ensure_selected_visible(viewport_height);
    }

    pub fn set_selected_index(&mut self, index: usize, viewport_height: usize) {
        if self.visible_nodes.is_empty() {
            self.selected = 0;
            self.scroll_offset = 0;
            return;
        }
        self.selected = index.min(self.visible_nodes.len().saturating_sub(1));
        self.ensure_selected_visible(viewport_height);
    }

    pub fn move_selection(&mut self, delta: i32, viewport_height: usize) {
        if self.visible_nodes.is_empty() {
            self.selected = 0;
            self.scroll_offset = 0;
            return;
        }
        let max_index = self.visible_nodes.len().saturating_sub(1) as i32;
        self.selected = (self.selected as i32 + delta).clamp(0, max_index) as usize;
        self.ensure_selected_visible(viewport_height);
    }

    pub fn scroll_by(&mut self, delta: i32, viewport_height: usize) {
        if self.visible_nodes.is_empty() {
            self.scroll_offset = 0;
            return;
        }
        let max_scroll = self.max_scroll(viewport_height) as i32;
        self.scroll_offset = (self.scroll_offset as i32 + delta).clamp(0, max_scroll) as usize;
        self.keep_selection_in_view(viewport_height);
    }

    pub fn page_selection(&mut self, direction: i32, viewport_height: usize) {
        let page = viewport_height.saturating_sub(1).max(1) as i32;
        self.move_selection(direction * page, viewport_height);
    }

    pub fn expand_selected(&mut self, viewport_height: usize) {
        let Some(node) = self.visible_nodes.get(self.selected).cloned() else {
            return;
        };
        if node.is_parent_link || !node.is_dir {
            return;
        }
        if !self.expanded.contains(&node.path) {
            self.expanded.insert(node.path.clone());
            self.rebuild_visible_nodes();
            self.select_path(&node.path);
            self.ensure_selected_visible(viewport_height);
            return;
        }
        if let Some(next) = self.visible_nodes.get(self.selected + 1) {
            if next.depth > node.depth {
                self.selected += 1;
                self.ensure_selected_visible(viewport_height);
            }
        }
    }

    pub fn collapse_selected(&mut self, viewport_height: usize) {
        let Some(node) = self.visible_nodes.get(self.selected).cloned() else {
            return;
        };
        if node.is_parent_link {
            return;
        }
        if node.is_dir && self.expanded.contains(&node.path) {
            self.expanded.remove(&node.path);
            self.rebuild_visible_nodes();
            self.select_path(&node.path);
            self.ensure_selected_visible(viewport_height);
            return;
        }

        let Some(parent) = node.path.parent() else {
            return;
        };
        if parent == self.root {
            return;
        }
        self.select_path(parent);
        self.ensure_selected_visible(viewport_height);
    }

    pub fn activate_selected(&mut self) -> TreeActivation {
        let Some(node) = self.visible_nodes.get(self.selected).cloned() else {
            return TreeActivation::None;
        };

        if node.is_parent_link {
            return TreeActivation::ChangeRoot(node.path);
        }

        if node.is_dir {
            TreeActivation::ChangeRoot(node.path)
        } else {
            TreeActivation::OpenFile(node.path)
        }
    }

    fn rebuild_visible_nodes(&mut self) {
        let mut visible_nodes = Vec::new();
        if let Some(parent) = self.parent_root() {
            visible_nodes.push(VisibleNode {
                path: parent,
                name: "..".to_string(),
                depth: 0,
                is_dir: true,
                expanded: false,
                is_parent_link: true,
            });
        }
        for child in &self.root_node.children {
            flatten_node(child, 0, &self.expanded, &mut visible_nodes);
        }
        self.visible_nodes = visible_nodes;
        if self.selected >= self.visible_nodes.len() {
            self.selected = self.visible_nodes.len().saturating_sub(1);
        }
        self.scroll_offset = self.scroll_offset.min(self.max_scroll(usize::MAX / 4));
    }

    fn ensure_selected_visible(&mut self, viewport_height: usize) {
        if self.visible_nodes.is_empty() {
            self.scroll_offset = 0;
            return;
        }
        let viewport_height = viewport_height.max(1);
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + viewport_height {
            self.scroll_offset = self.selected + 1 - viewport_height;
        }
        self.scroll_offset = self.scroll_offset.min(self.max_scroll(viewport_height));
    }

    fn keep_selection_in_view(&mut self, viewport_height: usize) {
        if self.visible_nodes.is_empty() {
            self.selected = 0;
            return;
        }
        let viewport_height = viewport_height.max(1);
        if self.selected < self.scroll_offset {
            self.selected = self.scroll_offset;
        } else {
            let last_visible = self
                .scroll_offset
                .saturating_add(viewport_height.saturating_sub(1))
                .min(self.visible_nodes.len().saturating_sub(1));
            if self.selected > last_visible {
                self.selected = last_visible;
            }
        }
    }

    fn max_scroll(&self, viewport_height: usize) -> usize {
        self.visible_nodes
            .len()
            .saturating_sub(viewport_height.max(1))
    }

    fn expand_ancestors(&mut self, path: &Path) {
        let mut current = path.parent();
        while let Some(dir) = current {
            if !dir.starts_with(&self.root) {
                break;
            }
            self.expanded.insert(dir.to_path_buf());
            if dir == self.root {
                break;
            }
            current = dir.parent();
        }
    }
}

fn collect_entries(root: &Path, show_hidden: bool) -> Result<Vec<EntryInfo>> {
    let ignore_rules = collect_ignore_rules(root)?;
    let walker = WalkBuilder::new(root)
        .hidden(!show_hidden)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .build();

    let mut entries = Vec::new();
    for entry in walker {
        let entry = entry?;
        let path = entry.into_path();
        if path == root {
            continue;
        }

        let metadata = fs::metadata(&path)
            .with_context(|| format!("Unable to read metadata for {}", path.display()))?;
        if matches_ignore_rules(&path, metadata.is_dir(), &ignore_rules) {
            continue;
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());

        entries.push(EntryInfo {
            path,
            name,
            is_dir: metadata.is_dir(),
        });
    }

    Ok(entries)
}

fn collect_ignore_rules(root: &Path) -> Result<Vec<IgnoreRule>> {
    let mut rules = Vec::new();
    collect_ignore_rules_in_dir(root, &mut rules)?;
    Ok(rules)
}

fn collect_ignore_rules_in_dir(dir: &Path, rules: &mut Vec<IgnoreRule>) -> Result<()> {
    let ignore_file = dir.join(".gitignore");
    if let Ok(contents) = fs::read_to_string(&ignore_file) {
        for raw_line in contents.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let negated = line.starts_with('!');
            let pattern = line.trim_start_matches('!').trim_start_matches('/');
            if pattern.is_empty() {
                continue;
            }

            let only_dir = pattern.ends_with('/');
            let pattern = pattern.trim_end_matches('/');
            let basename_only = !pattern.contains('/');
            let glob_pattern = if basename_only {
                pattern.to_string()
            } else {
                pattern.to_string()
            };

            let matcher = Glob::new(&glob_pattern)
                .with_context(|| format!("Invalid .gitignore pattern `{pattern}`"))?
                .compile_matcher();

            rules.push(IgnoreRule {
                base: dir.to_path_buf(),
                matcher,
                negated,
                only_dir,
                basename_only,
            });
        }
    }

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_ignore_rules_in_dir(&path, rules)?;
        }
    }
    Ok(())
}

fn matches_ignore_rules(path: &Path, is_dir: bool, rules: &[IgnoreRule]) -> bool {
    let mut ignored = false;
    for rule in rules {
        if !path.starts_with(&rule.base) {
            continue;
        }
        if rule.only_dir && !is_dir {
            continue;
        }

        let Ok(relative) = path.strip_prefix(&rule.base) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();

        let matched = if rule.basename_only {
            rule.matcher.is_match(&file_name)
        } else {
            rule.matcher.is_match(&relative)
        };

        if matched {
            ignored = !rule.negated;
        }
    }
    ignored
}

fn build_tree(root: &Path, root_name: &str, entries: &mut Vec<EntryInfo>) -> TreeNode {
    let mut grouped = BTreeMap::<PathBuf, Vec<EntryInfo>>::new();
    for entry in entries.drain(..) {
        let parent = entry
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.to_path_buf());
        grouped.entry(parent).or_default().push(entry);
    }

    build_node(
        root.to_path_buf(),
        root_name.to_string(),
        true,
        &mut grouped,
    )
}

fn build_node(
    path: PathBuf,
    name: String,
    is_dir: bool,
    grouped: &mut BTreeMap<PathBuf, Vec<EntryInfo>>,
) -> TreeNode {
    let mut children = grouped.remove(&path).unwrap_or_default();
    children.sort_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.path.cmp(&right.path))
    });

    let children = children
        .into_iter()
        .map(|child| build_node(child.path, child.name, child.is_dir, grouped))
        .collect();

    TreeNode {
        path,
        name,
        is_dir,
        children,
    }
}

fn flatten_node(
    node: &TreeNode,
    depth: usize,
    expanded: &BTreeSet<PathBuf>,
    out: &mut Vec<VisibleNode>,
) {
    let is_expanded = expanded.contains(&node.path);
    out.push(VisibleNode {
        path: node.path.clone(),
        name: node.name.clone(),
        depth,
        is_dir: node.is_dir,
        expanded: is_expanded,
        is_parent_link: false,
    });

    if node.is_dir && is_expanded {
        for child in &node.children {
            flatten_node(child, depth + 1, expanded, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn sorts_directories_before_files() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("b_dir")).unwrap();
        fs::create_dir_all(temp.path().join("a_dir")).unwrap();
        fs::write(temp.path().join("z.txt"), "").unwrap();
        fs::write(temp.path().join("a.txt"), "").unwrap();

        let workspace = WorkspaceState::load(temp.path().to_path_buf(), false).unwrap();
        let names = workspace
            .visible_nodes()
            .iter()
            .filter(|node| !node.is_parent_link)
            .map(|node| node.name.clone())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["a_dir", "b_dir", "a.txt", "z.txt"]);
    }

    #[test]
    fn honors_gitignore_entries() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(temp.path().join("ignored.txt"), "").unwrap();
        fs::write(temp.path().join("visible.txt"), "").unwrap();

        let workspace = WorkspaceState::load(temp.path().to_path_buf(), false).unwrap();
        let names = workspace
            .visible_nodes()
            .iter()
            .map(|node| node.name.clone())
            .collect::<Vec<_>>();

        assert!(!names.contains(&"ignored.txt".to_string()));
        assert!(names.contains(&"visible.txt".to_string()));
    }

    #[test]
    fn supports_globs_and_negation_in_gitignore() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("logs")).unwrap();
        fs::write(temp.path().join(".gitignore"), "*.log\n!important.log\n").unwrap();
        fs::write(temp.path().join("logs/app.log"), "").unwrap();
        fs::write(temp.path().join("important.log"), "").unwrap();
        fs::write(temp.path().join("keep.txt"), "").unwrap();

        let workspace = WorkspaceState::load(temp.path().to_path_buf(), false).unwrap();
        let names = workspace
            .visible_nodes()
            .iter()
            .map(|node| node.name.clone())
            .collect::<Vec<_>>();

        assert!(!names.contains(&"app.log".to_string()));
        assert!(names.contains(&"important.log".to_string()));
        assert!(names.contains(&"keep.txt".to_string()));
    }

    #[test]
    fn reveal_path_expands_ancestors_and_selects_file() {
        let temp = TempDir::new().unwrap();
        let nested = temp.path().join("src").join("deep");
        fs::create_dir_all(&nested).unwrap();
        let file = nested.join("mod.rs");
        fs::write(&file, "pub fn run() {}\n").unwrap();

        let mut workspace = WorkspaceState::load(temp.path().to_path_buf(), false).unwrap();
        workspace.reveal_path(&file, 4);

        let names = workspace
            .visible_nodes()
            .iter()
            .map(|node| node.name.clone())
            .collect::<Vec<_>>();
        let canonical_file = fs::canonicalize(&file).unwrap();

        assert!(names.contains(&"src".to_string()));
        assert!(names.contains(&"deep".to_string()));
        assert!(names.contains(&"mod.rs".to_string()));
        assert_eq!(workspace.selected_path(), Some(&canonical_file));
    }

    #[test]
    fn includes_parent_navigation_row_when_available() {
        let temp = TempDir::new().unwrap();
        let workspace_dir = temp.path().join("workspace");
        fs::create_dir_all(&workspace_dir).unwrap();

        let workspace = WorkspaceState::load(workspace_dir.clone(), false).unwrap();
        let first = workspace.visible_nodes().first().unwrap();
        let canonical_parent = fs::canonicalize(temp.path()).unwrap();

        assert_eq!(first.name, "..");
        assert!(first.is_parent_link);
        assert_eq!(first.path, canonical_parent);
    }
}

//! Small helpers shared by more than one action.

use git2::Repository;

/// The branch HEAD currently points at, or `None` on a detached HEAD (a
/// `rev`-pinned checkout, or a commit made outside any branch).
pub fn current_branch_name(repo: &Repository) -> Option<String> {
    let head = repo.head().ok()?;
    if head.is_branch() {
        head.shorthand().ok().map(str::to_string)
    } else {
        None
    }
}

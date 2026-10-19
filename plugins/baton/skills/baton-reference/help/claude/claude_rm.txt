Usage: claude rm <id> [--discard-unpushed <commit>@<worktree-id>] [--force-remove-worktree <worktree-id>]

  Delete a background session and its worktree. Unlike `stop`, works on already-exited sessions.
  --discard-unpushed <commit>@<worktree-id>  also discard the worktree's unpushed commits (and any uncommitted changes) while it is still the same worktree at that commit — pass the value a previous 'claude rm <id>' reported
  --force-remove-worktree <worktree-id>      delete the worktree directory even though the WorktreeRemove hook or git couldn't remove it (its repository must list it as a worktree, or have dropped it in a removal that couldn't delete it; no uncommitted changes to tracked files, submodules included; the branch is kept) — pass the value a previous 'claude rm <id>' reported

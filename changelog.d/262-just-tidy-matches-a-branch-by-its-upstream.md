- `just tidy` now retires a worktree and a local branch whose name is not the name of
  the pull request's head they were pushed to: it matches a merged pull request by
  the upstream a local branch tracks as well as by name. A branch matched that way is
  retired only when it was made before its pull request was opened and holds no
  commit the pull request did not; a branch on origin is deleted only while it stands
  where its pull request merged; and `just tidy` names each branch it leaves, with
  the reason and with what retires it
  ([how](docs/how-to/implement-a-feature.md); card 262).

- Commit messages and pull request bodies no longer end with Claude Code's attribution
  lines: no `Co-Authored-By:` line that names Claude, no `Claude-Session:` link and no
  "Generated with Claude Code" line. The `/commit` and `/pr` skills say so, `/pr` has
  the check of a pull request's body, and a new `.claude/settings.json` turns the
  lines off for every session in the repository. Commits already on `master` keep
  theirs ([Implement a feature](docs/how-to/implement-a-feature.md), step 4).

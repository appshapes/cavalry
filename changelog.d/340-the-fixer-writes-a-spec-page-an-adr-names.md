- The review loop's agent can now write a page of `spec/` when its pull request's
  branch adds an Accepted ADR that names the page: that page, its vectors and
  `spec/README.md`. The other files under `spec/` stay denied, and with no such ADR all
  of `spec/` is denied, as before. A review's blocker under `spec/` then costs one fix
  cycle, where it cost every cycle and then a person's session. The job asks `master`'s
  copy of `scripts/fixer-spec-pages.py` right after the checkout, takes its answer only
  when it is deny entries for `spec/`, and denies all of `spec/` without one. A run
  that an `@claude` comment starts is still denied `spec/`
  ([ADR 0140](docs/decisions/0140-let-the-fixer-write-a-spec-page-that-an-accepted-adr-on-its-branch-names.md);
  [Ask the agent to make a change](docs/how-to/ask-the-agent.md); work item 340).

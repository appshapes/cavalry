# 0140. Let the fixer write a spec page that an Accepted ADR on its branch names

- **Status:** Accepted
- **Date:** 2026-11-21
- **Invariants affected:** 6; and 1, whose rule that a wire change is a change to
  `spec/` first this keeps
- **Supersedes:** one rule of the agentic loop of ADR 0004, as
  `.github/workflows/claude.yml` held it: that every write under `spec/` is denied to
  the loop's agent, whatever its branch holds. The rest of ADR 0004 and ADR 0005 stands

## Context

The loop's agent runs in one job of `.github/workflows/claude.yml`. As the writer it
works an issue on a new branch. As the fixer it applies the blockers of a review to a
pull request's branch, up to three fix cycles, and then the pull request goes to a
person. The job's settings denied `Write(spec/**)` and `Edit(spec/**)` with no
exception, with the comment "the agent asks, a human decides". The role the agent
reads, `.claude/agents/developer.md`, forbade `spec/` only "without an accepted ADR".

Work item 340 records what the difference cost. On pull request 300 (work item 230,
ADR 0129) the first review's blocker needed code, tests, the ADR and three sentences
of `spec/cluster.md`. The first fix run did all but the sentences and left them in a
comment: "This run's tooling denies every write under spec/". The second and third
reviews asked for those three sentences alone, word for word, and the second and
third fix runs ended with them unwritten. A person's session applied them. The fourth
review found a blocker the fixer could have fixed, a missing test, and by then the cap
was reached.

Most features of ADR 0138's slice change `spec/`: the cluster protocol, the durable
log's format, the C ABI. The owner ruled on 2026-11-21, in the session that worked
work item 360, by the option chosen: "Let the fixer write spec/ under an ADR. The
workflow allows the fixer to write a spec/ page when the pull request's branch holds
an Accepted ADR that names that page. The docs lint already refuses a spec/ change
with no Accepted ADR in the same change, and the reviewer still reads every change.
Spec blockers then cost one fix cycle, as any other."

## Decision

Before the agent runs, its job asks which files under `spec/` the run may write, and
the agent's settings deny the rest. A page of `spec/` is writable when the branch adds
an Accepted ADR that names it. With no such ADR, all of `spec/` is denied, as it was.

1. **What counts.** The branch is the commits of the checked-out head that
   `master` on origin does not have. An ADR counts when it is a file under
   `docs/decisions/` that the branch adds, its first Status line reads exactly
   `Accepted`, and its text names the page as `spec/<page>.md`. An ADR that `master`
   has opens nothing, changed or not: the page it names was decided for another
   change, and an accepted ADR is not edited.
2. **What opens.** The page, `spec/<page>.md`; its vectors, `spec/vectors/<page>.py`
   and `spec/vectors/<page>.json`; and `spec/README.md`, whose table holds the page's
   version and its ADRs. Every other file under `spec/` that the head or the base
   holds is denied by name, since a permission rule has no "all but".
3. **Who asks, and what is taken.** `scripts/fixer-spec-pages.py`, in a step right
   after the checkout, before the toolchain is installed and before any cache is
   restored. It is `master`'s copy that runs, read by the full name of the
   remote-tracking branch, `refs/remotes/origin/master`, and never from the checkout:
   a branch cannot widen its own permissions by editing the script, and a tag that
   somebody named `origin/master` is not read in the branch's place. The answer
   becomes text in the agent's settings, so the step takes it only when it is one line
   of deny entries for files under `spec/` and nothing else.
4. **Closed when in doubt.** What the script cannot read gives no page, and neither
   does a file under `spec/` whose name the settings could not hold. A step that
   fails, gives no answer, or gives one that is not such a line leaves the settings
   with the entry they always had, `spec/**`. The step cannot fail the job.
5. **Which runs.** The writer on an issue starts at `master`, so its branch adds no
   ADR when the job asks, and it is denied `spec/` as before: an ADR it writes opens
   nothing for the run that writes it. A fix run that a review starts is asked at the
   pull request's merge commit, and may write the pages its branch's ADR names. A run
   that an `@claude` comment on a pull request starts is asked at `master`, before
   the action checks the branch out, so it is denied `spec/`.
6. **The roles say the same.** `.claude/agents/developer.md`: a blocker under `spec/`
   that no ADR on the branch names stays the author's, and the agent says so in a
   comment and leaves it. `.claude/agents/reviewer.md`: the ADR is read first, and a
   change to a page beyond what the ADR decides is a blocker.

## Alternatives considered

- **Keep `spec/` denied, and tell the reviewer not to start a fix cycle for a blocker
  that is under `spec/` alone.** The other option the owner was given. Lost: the
  cycles would be kept, and every sentence of `spec/` a review asks for would still
  wait on a person's session, in a slice whose features mostly change `spec/`.
- **Open all of `spec/` to the agent.** Lost: the rule is spec first, with its ADR
  (`CLAUDE.md`, Protocol), and a page that no decision names is not the fixer's to
  change.
- **Open a page only when the ADR decides a change to it, not when it names it.**
  Lost: a script cannot tell a page that an ADR amends from one that it cites, and a
  rule that nothing checks is what the role's sentence was. The reviewer reads the
  change.
- **Count an ADR that the branch changes, as well as one it adds.** This ADR's first
  draft. Lost: one more line on an ADR of `master`'s opened the pages that ADR names,
  and `just lint-docs` forbids that edit, so the case had no honest use.
- **Leave it to `just lint-docs`, which refuses a change under `spec/` with no
  Accepted ADR.** Lost: the lint runs after the edit and is not about which page. A
  refusal at the edit is the better error, and it names the file.
- **Run the checkout's copy of the script.** Lost: the branch under review would
  decide its own permissions.
- **Fail the job when the question cannot be answered.** Lost: a fix run that cannot
  write `spec/` still fixes code and tests, as every fix run did until today.
- **Ask a run that a comment starts at the pull request's head.** Lost for now: the
  job checks the branch out only inside the action, and the fix runs that this
  decision is for are started by a review. The Revisit clause has the case.

## Evidence

Pull request 300's record, on work item 340. The tests,
`scripts/fixer-spec-pages-test.py`, nineteen of them. Fifteen run the script over
made-up checkouts: a branch with no ADR, with an Accepted one, with two, with one that
`master` already has and the branch changed, with one that is not Accepted in five
ways, with `Accepted` in an example below a Status line that reads Rejected, with one
that names a page that is not there, with a page the branch deleted, with every file
writable, with an ADR in the working tree that no commit holds, at the merge commit
of the branch into `master`, with a tag named `origin/master`, with a file name that
git would quote, with a base that cannot be read, and outside a repository. Four hold
the step and the settings of `claude.yml`: where the step stands, what it runs, which
answers it takes and which it refuses, among them an answer that would have rewritten
the settings' allow list, and the settings read as JSON with both kinds of answer. The
script run on this repository with the tree before ADR 0136 as the base, so that ADR
0136, ADR 0137 and ADR 0138 were the branch's: it named `spec/cluster.md`,
`spec/udp.md` and `spec/c-abi.md` writable with their vectors and `spec/README.md`,
and denied the other 21 files under `spec/` by name. On `master` itself it denies
all. An independent read of the change before it landed found the tag, the unchecked
answer, the changed ADR, the Status line in an example, the deleted page and the
quoted name; the change has each.

## Consequences

Easier: a blocker under `spec/` costs one fix cycle, as any other, when the branch's
ADR names the page; the workflow and the roles say one thing; the denial of a page
that no ADR names is at the edit, with the file's name.

Harder: an ADR that names a page without deciding anything about it opens it, which
the run on this repository shows, where ADR 0136 cites `spec/udp.md` for a
comparison; once any page is writable, a file under `spec/` that neither the head nor
the base holds is on no list, and only `just lint-docs` and the reviewer stand before
it; the agent can open a page for a later run by committing an Accepted ADR in an
earlier one, so the ADR is what the reviewer reads first; a change to the script
takes effect when it is on `master`, since `master`'s copy is the one that runs; a
run that a comment starts cannot write `spec/`, though its branch holds the ADR.

Revisit: when a fix run writes a page of `spec/` beyond what the branch's ADR decides,
for a narrower rule; when a blocker under `spec/` still costs more than one fix cycle,
for what stopped the fixer; when the agent writes an ADR in order to open a page, for
whether an ADR the agent wrote counts; when a person asks the agent by a comment for a
change under `spec/`, for asking that run at the pull request's head.

## References

- Work item 340 on the Brigade Board, which this decision answers, and work item 360,
  where the owner's ruling is recorded; work item 230 and pull request 300, the case
- `CLAUDE.md`: Protocol; Documentation; Working rules for agents
- ADR 0004, ADR 0005, ADR 0119, ADR 0129, ADR 0138
- `.github/workflows/claude.yml`, the step "Which pages of spec/ this run may write"
  and the comment under `settings`; `scripts/fixer-spec-pages.py`;
  `scripts/fixer-spec-pages-test.py`; `.claude/agents/developer.md`;
  `.claude/agents/reviewer.md`
- [Ask the agent to make a change](../how-to/ask-the-agent.md), "When it does not
  work"

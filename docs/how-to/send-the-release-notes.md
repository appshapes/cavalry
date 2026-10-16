# Send the weekly release-notes email

A how-to page: one task, the email that tells the people on Cavalry's list what changed
on `master` in the week, from configuring it once to sending one by hand. Why it is built
this way is [ADR 0026](../decisions/0026-email-the-weeks-changes.md). The workflow is
`.github/workflows/send-release-notes.yml`; its header says how each step works.

## Prerequisites

- Admin access to the repository, to set its Actions variables and secrets once.
  `gh auth status` says who you are.
- An SMTP account. The house sends through Resend, from a verified domain.
- The configuration, by name. A variable is printed in run logs; a secret is not. This
  repository is public, so an address is never a variable.

| Type | Name | Value |
| --- | --- | --- |
| Variable | `SMTP_SERVER` | `smtp.resend.com` |
| Variable | `SMTP_PORT` | `465` |
| Variable | `SMTP_FROM_ADDRESS` | The sender. An inbox someone reads: the footer tells a reader to reply to stop, and there is no other way to stop. |
| Secret | `SMTP_USERNAME` | `resend` |
| Secret | `SMTP_PASSWORD` | A Resend API key with permission to send. |
| Secret | `RELEASE_NOTES_RECIPIENTS` | The list: addresses separated by commas, blanks or line ends; `#` starts a comment. Every reader is `bcc`. |
| Secret | `RELEASE_NOTES_PREVIEW_ADDRESS` | Optional. Where a preview goes; the sender's own address when unset. |
| Secret | `CLAUDE_CODE_OAUTH_TOKEN` | Exists since ADR 0004; the writer and the reviewer run on it. |

Without the three variables and the two `SMTP_` secrets, a scheduled run is a green run
that says the mail server is not configured, and a preview is attached to its run and
mailed to nobody. With some of them, every run fails and names what is missing.

## Steps

1. **Configure once.** From a checkout:

   ```sh
   gh variable set SMTP_SERVER --body smtp.resend.com
   gh variable set SMTP_PORT --body 465
   gh variable set SMTP_FROM_ADDRESS --body cavalry@example.com      # the real sender
   gh secret set SMTP_USERNAME --body resend
   gh secret set SMTP_PASSWORD                                        # paste the key, then Ctrl-D
   gh secret set RELEASE_NOTES_RECIPIENTS                             # one address a line, then Ctrl-D
   ```

   `gh variable list` shows the three variables; `gh secret list` shows the secrets'
   names and dates, never their values.

2. **Preview.** Run the workflow by hand; a run by hand previews unless it says
   otherwise:

   ```sh
   gh workflow run send-release-notes.yml -f audience=preview
   gh run watch
   ```

   The run takes twenty to thirty minutes: the writer agent drafts, the lint and the
   reviewer agent read it, and one fix cycle follows when either refused. The email then
   arrives at the preview address with `[preview]` in front of its subject. The run's
   artifact, `release-notes-email-<run id>`, holds `notes.md` (the draft), `lint.txt`,
   `review.md`, `email.md` and `email.html` (what was sent), `title.txt`, `window.txt`
   (the commits covered) and `sources/` (what the writer read).

3. **Let Monday send it.** The scheduled run, Mondays at 14:41 UTC, covers the commits
   since the last email that reached the recipients and sends to the list. Nothing to do.
   A week with no commit sends nothing and says so on the run page.

4. **Send what was previewed.** Every run writes a new draft, and the gate can approve one
   and refuse the next. To send the draft a preview approved, name that preview's run:

   ```sh
   gh workflow run send-release-notes.yml -f audience=recipients -f draft_run=<run id>
   ```

   No agent runs. The draft is taken as it was approved, provided that run succeeded on
   `master`, its review says `APPROVED` and it covered the very same commits; the lint
   runs on it again, against `master` as it is now.

5. **Send a window by hand.** The `from` input is the first commit the email covers, a
   hash or a ref, or a date `YYYY-MM-DD`; it is inclusive. Preview it first, as in step 2,
   then send it as in step 4:

   ```sh
   gh workflow run send-release-notes.yml -f audience=preview -f from=2026-10-10
   ```

6. **Add or remove a reader.** Set the secret again with the whole list. A reader who
   replied asking to stop is removed this way; there is no other mechanism.

   ```sh
   gh secret set RELEASE_NOTES_RECIPIENTS
   ```

7. **Check a draft on a laptop.** The lint reads the checkout, so a draft can be checked
   before any run; the title is whatever the draft's first line is meant to be:

   ```sh
   GITHUB_REPOSITORY=appshapes/cavalry RELEASE_NOTES_TITLE="What's new in Cavalry, October 10 to 16, 2026" \
     python3 scripts/release-notes.py lint scripts/release-notes-email-sample.md
   python3 scripts/release-notes-email.py --in scripts/release-notes-email-sample.md --out /tmp/sample.html
   ```

   The first prints one `ok:` line per check and ends with `release-notes-lint: clean`;
   the second writes the HTML an inbox would show. `just test-scripts` runs both scripts'
   tests.

## How to verify it worked

- `gh run list --workflow send-release-notes.yml` lists the run as `completed success`.
- On the run's page, the step "Decide what the email covers" says how many commits and
  which, and "Read the recipients" says how many will receive the email, never who. The
  step "Send to the recipients" succeeded: that step's success is how the next run finds
  where this email left off.
- The next run's "Decide what the email covers" step says `the last email was sent by the
  run created at <time>, from <commit>`.
- A preview is in the preview address's inbox, `[preview]` in the subject, with the same
  words as the artifact's `email.md`.

## When it does not work

- **`the mail server is half-configured; missing: ...`** Set what the message names
  (the table above). The run fails on purpose: a fork has none of them, a repository has
  all of them.
- **`the email was not approved after N round(s); nothing was sent`.** The gate refused.
  Download the artifact and read `lint.txt` (every `FAIL:` line is a fact about the
  checkout) and `review.md` (the reviewer's findings). Run a preview again, or, when a
  finding is the brief's fault, change `.claude/agents/release-notes-email.md` and run
  again.
- **`no commit landed since the last email; nothing to send`.** A quiet week. Nothing to do.
- **`the recipients receive the email from master only`.** The run was dispatched on a
  branch with `audience=recipients`. Run it on `master`.
- **`the last email was sent from <commit>, which is not on the history of <commit>`.**
  The history moved under the mark, which the no-rebase rule should make impossible. Run
  with `from` set to the first commit the email should cover.
- **`the draft of run N covers M commit(s) and this email covers K, or not the same ones`.**
  A commit landed between the preview and the send. Preview again, then send that run's
  draft.
- **The preview arrived nowhere.** `RELEASE_NOTES_PREVIEW_ADDRESS` is unset and the
  sender's inbox is not yours. Set the secret to your address.
- **A `FAIL:` line names something that does exist.** The lint reads `justfile`,
  `crates/cavalry/src/main.rs`, `config.toml`, `counters.toml`, `docs/decisions/` and
  the paths of the checkout at the run's commit, and `GITHUB_REPOSITORY` for the link
  host. If the thing exists there and the lint still refuses, the lint is wrong: fix it
  in `scripts/release-notes.py` with a test in `scripts/release-notes-test.py`.

- The review loop's fixer now waits for each gate it runs, for up to 20 minutes. Before,
  a gate that ran past the Bash tool's 120 s bound, `just test` above all, went to the
  background, the session ended its turn and the run ended with nothing pushed and the
  pull request body unedited. A fix run that lands nothing now says so in a comment on
  the pull request, which hands the review's blockers to the author. And the run has a
  step, "What the session did with the Bash tool", that lists the programs the session
  ran or was denied, the bound it asked for and how it ended, by known names alone
  ([Ask the agent to make a change](docs/how-to/ask-the-agent.md); work item 409).

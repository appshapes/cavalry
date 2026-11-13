# 0113. Track Cavalry's work on the Brigade Board, and map the Trello cards it carried

- **Status:** Accepted
- **Date:** 2026-11-13
- **Invariants affected:** 6
- **Supersedes:** none. Records for Cavalry the owner's decision carried out by the
  Brigade Board's work item 272, the cut-over of 2026-11-13, and says how to read a card
  number that Cavalry's history cites.

## Context

Until 2026-11-13 Cavalry's work was tracked as cards on the AppShapes Trello board, and
Cavalry's history cites them by number. A commit's subject starts `<card>: `
(`.claude/skills/commit/SKILL.md`, step 5), and an ADR's References name "AppShapes Trello
card N". Many ADRs also say "card N" alone in their text: ADR 0104 says "card 274" and
"card 280", and ADR 0101 says "card 272".

On 2026-11-13 the owner moved the four AppShapes repositories, brigade, cavalry, muster
and telder, onto one Brigade team, `appshapes`, on the Brigade server, in one cut-over:
the Brigade Board's work item 272. A *work item* is the Brigade Board's unit of work, the
equivalent of a Trello card. The board numbers work items for the whole team. Commit
5bbd2ad8 points Cavalry's `.brigade.json` at the server and turns the board on
(`"board": true`). Cavalry's open cards were carried onto the board, into its Wanting
list, each tagged `cavalry`. From the cut-over Cavalry's sessions create no Trello cards.

Brigade's own work items already held some of the numbers that Cavalry's open cards had.
The owner's rule for the carry: a card keeps its Trello number where the board has it
free. A card whose number is taken gets a new number, and the first line of its
description says "Trello card N". Five of the seventeen cards open at the export got new
numbers. A number in Cavalry's history can therefore name two different pieces of work.
Commits 433dec22 and 020ab3fb of 2026-11-12 start `272: ` for Trello card 272, a
`cavalry:udp` channel's term length (ADR 0099). Commit 5bbd2ad8 of 2026-11-13 starts
`272: ` for work item 272, the cut-over. Invariant 6 asks that a stranger can find why
any decision was made from the repository alone. The trail from a commit or an ADR to
the card it was made for is part of that, and the board lives outside the repository.

## Decision

Cavalry's work is tracked on the Brigade Board of the team `appshapes` from 2026-11-13,
each of its work items tagged `cavalry`. A card number cited in Cavalry's history before
commit 5bbd2ad8 names an AppShapes Trello card. One cited in that commit or after it
names a work item on the Brigade Board. The seventeen cards open at the export are
carried as the table shows. This table is the one place in the repository that maps a
Trello card's number to the work item that carries it:

| Trello card | Work item | Title |
| --- | --- | --- |
| 198 | 198 | Find what puts one of grip's 21 bench runs at twice its row's band, and what lifts a whole process's client row by a tenth |
| 229 | 229 | Connect a cluster client over the session protocol, through the ABI and the five clients |
| 230 | 230 | Catch a member up from a peer's recording, snapshot the service and follow as a standby |
| 231 | 231 | Run a rolling upgrade across the cluster protocol's version, under conformance/upgrade |
| 232 | 232 | Gate the cluster across bench, grip and kip, with kip as the third host |
| 257 | 257 | Close what cavalry run leaves open in a cluster member's role |
| 265 | **284** | Find why the driver takes 12 to 52 s on the Linux runner to announce 2,048 images |
| 273 | **285** | Count a clock step the driver sees and report it in doctor |
| 274 | **286** | Bring back a cluster member that lost its log directory without its vote on what it forgot |
| 275 | **287** | Check a recording's frames after a power loss, so a cluster member's start cuts its log to what was synced |
| 276 | **288** | Sync the log directory when a member's terms file is created, and keep a second consensus module off a log directory |
| 277 | 277 | Keep a cluster member whose truncation faulted midway from leading or joining on the end it no longer holds |
| 279 | 279 | Check in a gate that each spec/vectors/*.json is what its .py writes |
| 280 | 280 | Replace a leader whose consensus module is silent while its driver runs |
| 281 | 281 | Have cavalry doctor name a cluster member whose driver recovered a recording, so the rule after a power loss is a check |
| 282 | 282 | Leave the rest of a consensus poll on the stream when one of its messages moves the member to init |
| 283 | 283 | Have CountersFile::create refuse a capacity below its static counters before it creates the file |

A card that was not open at the export was not carried, and its number names only the
Trello card. Trello card 256 is one: it was finished by commit f4883ff3 earlier on
2026-11-13.

## Alternatives considered

- **Give every carried card a number past the board's highest.** Lost: every ADR,
  commit and pull request that cites one of the twelve cards whose number was free would
  then need the table to be read. Keeping their numbers leaves five to translate.
- **Keep the map on the board alone, in each renumbered work item's first line.** Lost:
  the board is outside the repository. A reader of an ADR with only the repository, as
  invariant 6 puts it, could not follow "card 274" in ADR 0104 to work item 286.
- **Rewrite the old citations to the new numbers.** Lost: an accepted ADR is immutable,
  and a commit's subject cannot change without rewriting history that has been pushed.
- **A page under `docs/explanation/` or `docs/how-to/`.** Lost: the map is a fact fixed
  on one day, neither a task nor an explanation, and a reference page is generated, never
  hand-written. An ADR is immutable once accepted, which a map fixed on the day should be.

## Evidence

- `brigade work-item list --tag cavalry` on 2026-11-13, after the cut-over: seventeen
  work items in Wanting with the numbers and titles in the table.
- `brigade work-item show` for 284 to 288: the first line of each description is
  "Trello card 265", "Trello card 273", "Trello card 274", "Trello card 275" and "Trello
  card 276" in that order.
- `git log`: 433dec22 and 020ab3fb, `272: ` for Trello card 272, on 2026-11-12; 5bbd2ad8,
  `272: ` for work item 272, on 2026-11-13. Also f4883ff3, `256: `, and 112e83c6, `257: `,
  both on 2026-11-13.

## Consequences

- A stranger reads any card number in Cavalry's history by its date against commit
  5bbd2ad8, and, for the five that moved, by this table.
- Work item 257 was finished on `master` by commit 112e83c6 after the export, so the
  carry put it on the board as open. It is closed on the board, not here.
- Until Cavalry's skills are ported to the board, several still name Trello: the commit
  skill says the number before a subject is "the AppShapes Trello card", the `trello-*`
  skills read and write Trello, and `next-task` and `pr` read it. They are ported after
  the cut-over's check, as part of work item 272's follow-up, and Cavalry gets a
  `work-item-start` skill of its own. Until then a session takes a card's number from the
  board.
- Revisit if the team's board is replaced or renumbered. A new map is then a new ADR
  that supersedes this one.

## References

- The Brigade Board's work item 272, the cut-over of 2026-11-13, and work items 284 to
  288
- Commit 5bbd2ad8, `.brigade.json` on the Brigade server
- ADR 0004, for `brigade decision`; ADR 0099 and ADR 0101, for Trello card 272; ADR 0104,
  for Trello cards 274 and 280
- `.claude/skills/commit/SKILL.md`

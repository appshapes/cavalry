- Cavalry's work is tracked as work items on the team's Brigade Board instead of Trello
  cards ([ADR 0113](docs/decisions/0113-track-cavalrys-work-on-the-brigade-board-and-map-the-trello-cards-it-carried.md)):
  the Claude Code skill `/work-item-start` takes a work item through to `master`;
  `/next-task`, `/pr` and `/commit` read and name work items; and `/trello-create`,
  `/trello-read` and `/trello-start` are removed. A commit's number is its work item's
  ([Implement a feature](docs/how-to/implement-a-feature.md)).

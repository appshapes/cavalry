- The five clients' committed log rounds under `conformance/` read beside a cluster
  member whose log is one recording that begins at position 0, which
  `conformance/log-member.sh` finds from each member's file of its log's
  recordings, and check that recording's id where they expected recording 0 beside
  member 1. A member that joined its leader's log late holds its log in two
  recordings, and a round beside it would read its one entry from a recording that
  has stopped
  ([ADR 0103](docs/decisions/0103-list-the-recordings-of-a-members-log-in-its-driver-directory-for-a-service-to-read-in-order.md)).

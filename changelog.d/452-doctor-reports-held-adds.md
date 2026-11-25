- `cavalry doctor` now reports the `add subscription` and `add publication` commands
  the driver holds behind an endpoint that is closing
  (`subscription.add_deferred_waiting`, `publication.add_deferred_waiting`). It says
  what holds them and what to do, and, when `driver.registry_full` is above 0, that
  a registry refused as full had part of its slots taken by these held adds
  ([Publish between two hosts](docs/how-to/publish-between-two-hosts.md)).

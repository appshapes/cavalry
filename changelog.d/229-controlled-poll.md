- `Subscription::controlled_poll` reads a stream with a handler that answers for each
  fragment whether the read takes it: `Action::Continue` takes it and goes on,
  `Action::Break` takes it and ends the read of that image, and `Action::Abort` leaves
  it at the image's position, so that the next read hands the same fragment over
  again. A reader that cannot use a fragment yet, because the publication it would
  offer it on is back-pressured, leaves it unread, and its own publisher is held at
  its window with nothing buffered between them. In Aeron, this is
  `Subscription.controlledPoll` with a `ControlledFragmentHandler`, without its
  `COMMIT` action. `TermReader::controlled_poll` is the same read of one log in
  `cavalry-shm`. `spec/log-buffer.md` stays at version 1: no byte of the file changes
  ([ADR 0114](docs/decisions/0114-give-a-subscription-a-controlled-poll-whose-handler-can-leave-a-fragment-unread.md);
  work item 229).

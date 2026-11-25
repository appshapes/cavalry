- A cluster member no longer drops consensus messages when one of them moves it to
  init. A message that moves the member to init in the middle of a poll, by a term
  above its own or a step that fails its election, now ends the read there. The
  messages the poll would have read after it, 15 at most, stay on the stream and are
  acted on once the member has left init. Before, they were handed to the member in
  init, which acts on nothing, and nothing counted them. The module reads its
  consensus stream with a controlled poll
  ([ADR 0114](docs/decisions/0114-give-a-subscription-a-controlled-poll-whose-handler-can-leave-a-fragment-unread.md); work item 282).

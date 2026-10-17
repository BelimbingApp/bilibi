# bilibi-events

Base domain events (`base/events`): durable, versioned events recorded by an
owner inside its `MutationTransaction` and delivered at least once on
`bilibi_queue` to subscribers in other containers. Work in progress (D6).

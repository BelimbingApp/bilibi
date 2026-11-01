# Principal directory contract

Base screens use `base/principal_directory.directory` (`PrincipalDirectory`) to
name the principals their rows reference. Do not join `users` (or any Core table)
from Base and do not import Core.

- Identity is `PrincipalRef { kind, id }`, never an id alone: user 1 is not person 1.
- Pass the distinct pairs your already-filtered query references, before
  pagination; never ask for a whole tenant. `rank(client, scope, &candidates, &Query)`
  returns `Ranked`: `order()` feeds `array_position` before `offset`/`limit`.
- A principal nobody can name (deleted, archived, other tenant, no provider) is
  `unresolved`, not an error and not a removed row. Show `Ranked::label`, which
  falls back to the durable "User 91". Do not hide the row.
- Failure is visible: `TooManyCandidates` over the ceiling (keep rows with durable
  IDs) and `NameSearchUnavailable` when any candidate kind has no provider. A
  search never degrades to no-match or match-all; a provider error is
  `DirectoryError::Provider`.
- An owning Core module implements `PrincipalProvider` for one kind and binds it
  in its service factory: `directory.bind("core/user", Arc::new(..))`, declaring
  `base/principal_directory` as a dependency (see `apps/core/user/src/directory.rs`).
  A second provider for a kind, or any bind after boot, is refused. Providers scope
  by `scope.tenant()` in the query itself and may add provider-owned search
  attributes (account email) without Base learning them.

`PrincipalKind::Person` is the approved party model's Person. `apps/web/examples/p1-seams.rs` demonstrates the behavior.

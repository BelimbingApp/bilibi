# Workflow agent guidelines

Read `README.md` (Shared approvals) and ADR 15 before touching approvals.

## Approvals

- A new kind of approval (journal, leave, purchase order) is one `declare_approval_case!` plus one `ApprovalAdapter` bound in the owner's service factory (`examples/reference/mod.rs` is the pattern). Do not build an owner-side approval table, inbox page, delegation or exceptions register: Workflow owns them, and a copy would skip the independence checks.
- Approval never posts, books or starts anything. Do the work in `apply_in` on the handed `MutationTransaction`, calling your existing `_in` methods; do not open an inner transaction or call out, or a failed owner effect cannot undo the final vote.
- Decide who may approve through the declared `decisions` capability and the adapter's `candidate` read, not by comparing names or account IDs in your own code. Impersonators and agents count as the person they represent, and a job or system principal never approves.
- Raise a business override through `record_control_exception_in`, never your own register or a log line: only the shared register is reviewed by someone independent.
- Do not add an "approve anything" capability or contract. Each case gets static contracts from the macro.
- Time never approves. Reminders and reassignment live in the maintenance sweep; add no timer that completes a request.

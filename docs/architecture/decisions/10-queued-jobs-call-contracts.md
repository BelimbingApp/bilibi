# ADR 10: Queued jobs call operation contracts, and modules opt in to CSV import

**Document Type:** Architecture Decision Record
**Status:** Proposed
**Agents:** claude-code/opus-5.5 (author)
**Scope:** How background work writes business records (`base/agent_api` job caller), and how a module lets people import its records in bulk (`base/bulk_data`)
**Last Updated:** 2026-10-27

> Source: [issue 133](https://github.com/BelimbingApp/bilibi/issues/133). Bilimbi and Belimbing have no bulk import. ADR 01 defines the four doors; this record adds a fifth.

## Context

**Every business write is an operation contract (ADR 01), and a queued job could not call one.** `Plans::submit` needs a browser session or an OAuth grant (`PlanAuthority`), and a Queue job has neither. A module that wanted to write many records in the background would have had to call its own facade, skipping the contract's schema, capability and preview, or open the contract up to a job some other way. A CSV import of 1,000 contacts, or a manager reassigning 500 accounts, is exactly that work, and every Domain needs it at go-live.

**Per-module importers would repeat everything.** Upload, column mapping, a dry run, duplicates, per-row results, an error file and resumable background runs are the same for every record type; only the contracts and fields differ.

## Decision

**1. A queued job is the fifth caller of a write contract** (`bilibi_agent_api::job::JobContracts`, service `base/agent_api.jobs`). It calls a contract for the person who started the job (`Queue::enqueue_for`), under the rules of the other doors:

- the contract's channels, through a new `Channel::Job` (`Scope::in_job` marks the job's scope);
- the input schema, and the contract's capability decided live on every call;
- the owner's preview, then its commit on the job's own transaction (`Execution::begin`), which fences the lease, re-proves the person and keeps the enqueue's trace ID, so every row a run writes shares one trace.

No plan code is issued: the person reviewed the job when they started it, as a page form is reviewed before `Plans::submit`. A contract that declares step-up is not callable from a job (no person is present to give a code), and a system principal's job cannot call contracts at all.

**2. A module opts a record type in with one contribution** (`bilibi_bulk_data::RecordType` under `RECORD_TYPES`), naming its own create contract, an optional duplicates read and update contract with the fields to match on, references to other records (a parent record type, or a read that finds exactly one record), and its own import capability. The fields a column can map to are data: the create contract's input properties. Boot refuses unknown or foreign contracts, writes a job cannot call, undeclared or foreign capabilities, and match fields, references or parents that do not exist.

**3. Each row is its own short transaction**, with its result recorded in the same transaction, keyed by run, pass and row. A row's failure never rolls back another, and a retried attempt never writes a row twice. A pass works in time-bounded slices and queues its own continuation, so a long file fits Queue's attempt timeout. The dry run calls the owner's preview and writes nothing.

**4. Runs, saved mappings and row results belong to `base/bulk_data`**, written through `MutationBoundary`. Its contracts (map and check, start, cancel, save and delete mappings, and the reads) serve pages and agents alike. The uploaded file is a private artifact owned by the run, and the file of failed rows is written from it on every download.

## Rejected alternatives

- **Each module calls its own facade from a job.** It skips the schema, capability and preview that make agent, page and job writes agree, which is ADR 01's point.
- **A job holds a plan code from the person's review.** Codes expire in minutes (`agent_api.plan_expiry_seconds`) and cover at most 100 operations; a 10,000-row run outlives and outgrows them.
- **Writing rows with raw `INSERT`s, or one transaction for the whole file.** The first bypasses the owner's validation and audit; the second lets one bad row undo every good one and holds locks for minutes.
- **Storing the file's rows in the database.** The artifact keeps the original bytes for the error file and leaves retention to Artifacts.

## Consequences

- Receiving the file is not a contract yet: Artifacts takes bytes on a plain connection and a plan's input is bounded at 1 MiB, so the Imports page's `receive` event is a listed allowance (`Pending::Events`). Agents work on runs their person uploaded until agent-supplied files land.
- A row written by a job is attributed to the person, not to an agent connection that started the run; the run's own start is attributed as it happened.
- Export, bulk updates with `FlexTable` row selection, undo, step-up at the start of a run and scheduled exports are later phases of issue 133. Owner reassignment needs a record type that has an owner; no Core record does.

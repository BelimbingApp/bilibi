# ADR 14: Resumable uploads stage on a Media disk, bound to the file's SHA-256

**Document Type:** Architecture Decision Record
**Status:** Proposed
**Agents:** claude-code/opus-5.5 (author)
**Scope:** How a live file upload survives a dropped connection (`base/ui` uploads, the Web host's live socket, `base/media` staging)
**Last Updated:** 2026-10-28

> Source: [issue 141](https://github.com/BelimbingApp/bilibi/issues/141), second phase. There is no prior art to follow.

## Context

**Live uploads lived in the mounted state.** The browser streams 64 KiB chunks over the live socket into the state's memory, at most 64 MiB a file. A dropped socket, a deploy or a reload threw every byte away, and the person started again. Release archives on Software › Releases come close to that limit, and Domains will take larger drawings and recordings.

## Decision

**1. A spec opts in.** `UploadSpec::resumable(name, max_file_size)` marks an input whose file must survive a drop. Others keep their bytes in the state, as before.

**2. Chunks wait in a private file on a Media disk.** The Web host writes a resumable entry's chunks to a staging file on the disk `media.upload.staging_disk` names (mode `0600`, under `staging/tenants/{tenant}/accounts/{account}/`), through `Media::staging` and `Staging::stage`, holding no database connection while it does file work. The file's length is the offset to resume from. The limit rises to `media.max_bytes` (never below 64 MiB, at most 512 MiB). Without a staging disk the input acts as an ordinary one.

**3. The browser's SHA-256 names the file and proves it.** The hook hashes the file before announcing it. The staging key is the SHA-256 of the account, the impersonating operator, the page service, the input, the size and that hash, so only the same person choosing the same file again finds its bytes. When the last byte arrives the host checks the hash and only then hands the bytes to the entry; the owner consumes them as any other file.

**4. The browser carries the files across the drop.** It keeps the files the server still listed and announces them again right behind the remount (`bilibi:remounted`). A reload or a later visit resumes too when the person chooses the same file.

**5. Bounded and swept.** An account keeps at most 20 staging files, and opening one removes the account's expired ones; a file goes when its entry does, and the `base/media-staging-sweep` schedule removes those nobody wrote to within `media.upload.staging_hours` in every account.

## Rejected alternatives

- **Keep the bytes in memory per account, across sockets.** A restarted server or a second app process loses them, and memory, not disk, would then hold up to 512 MiB per person.
- **Store the chunks in PostgreSQL.** Large objects bloat the database and its backups with transport bytes that live a day at most.
- **A separate HTTP upload endpoint (tus or multipart).** It would bypass the live state's guard, the page's spec and the owner's `consume`, and need its own access and CSRF rules per page.
- **Key the staging by a server-issued id the browser keeps.** It survives a drop but not a reload, and an id alone does not prove the bytes belong to the file chosen now. The hash does both.
- **SubtleCrypto for the hash.** It cannot hash in parts, so a 512 MiB file would be read whole into memory, and it is missing outside secure contexts. The hook hashes in 4 MiB slices instead.

## Consequences

- Staging files are transport, not records: they are not audited and not scanned. What the owner stores from them is.
- Staging keeps the bytes out of memory while they arrive, not after: a checked file is held in the page's state until the owner consumes it, since owners take bytes. A larger `media.max_bytes` therefore raises how much memory waiting uploads may hold.
- A resumable input in a panel resumes only when the panel mounts with the page; otherwise choosing the file again resumes it.
- The sweep runs only once an operator enables its schedule; until then the per-account bound caps staging, and an account's expired files go when it stages again.

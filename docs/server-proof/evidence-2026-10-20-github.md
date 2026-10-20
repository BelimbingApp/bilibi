# GitHub release install proof, 2026-10-20

A server listed a real GitHub pre-release and installed it with fresh 2FA, a backup, an automatic switch-back and Undo, in the disposable systemd-nspawn lab.

## What ran

| Item | Recorded value |
| --- | --- |
| Platform source | `16dc2d352fe8a1a10ab9cd356558a7148ae302a8` plus this change, version 0.1.1 |
| Lab | Ubuntu 26.04 in machine `fsproof`, `10.77.61.0/24`, one instance `production` at `https://prod.10.77.61.2.sslip.io` (its exact `APP_ORIGIN`) |
| Release | Throwaway pre-release `proof-github-source-1` in `BelimbingApp/bilibi-setup-template`, created as a draft, published, then deleted. The repository has immutable releases on, so the tag name cannot be used again |
| File | `bilibi-setup-proof1-x86_64-unknown-linux-gnu.tar.gz`, 25,509,222 bytes, GitHub check code `287fca16d51b…`. It holds the lab's own `bilibi-web` with seven bytes added so it is a different program, and a `SHA256SUMS` |
| Browser | Chrome through chrome-devtools-axi, its own session and profile |
| Isolation | The host's Bilibi services, shared Caddy, `https://bilibi.localhost` and primary checkout were not touched. The lab and its root file system are deleted |

## Results

| Scenario | Result |
| --- | --- |
| Save the repository | Saving a new name looked it up on GitHub, remembered repository number 1426391840 and showed **Accepted: Yes** |
| Fresh 2FA to change the settings | Saving a read token sent the browser to the confirm page. The code was entered, the save was repeated and the token showed as **Saved**, never its value. **Clear read token** did the same. A token GitHub does not know was refused with "GitHub refused the read token" |
| List | **Show pre-releases** and **Refresh** listed the one file for Linux x86-64: tag, file, first 12 characters of the check code, date. `base.software.releases` returned the same rows with the repository, release, file and tag numbers |
| Refused before anything changed | A wrong check code and a wrong repository number were refused at the plan (`invalid_input`). A commit without a code was refused with 403 `step_up_required`, reason `every_time` |
| Cancel, job 2 | Cancelled at 6 of 24 MB. The `.part` file was removed and `incoming/` was empty |
| Interrupted download, job 3 | `bilibi@production` was restarted at about 3 MB. The queued job started the download again from the start, replaced the `.part` file and finished |
| Install, job 3 | Check code verified before unpacking, backup, release 6 copied, verified. Pause 2 seconds. The result kept `github`: repository, repository number, release, file, tag, target, archive check code and program check code |
| Undo, job 4 | Fresh code, safety backup, back on release 5 in 2 seconds. No call to GitHub |
| Changed check code, job 5 | The reviewed code was changed in the lab database as the job was queued, to stand for a file that changed after review. The job failed at the download step before downloading: "changed after it was reviewed (its check code no longer matches)". `incoming/` stayed empty |
| GitHub limiting this server, jobs 1 and 6 | The unauthenticated allowance of 60 calls an hour ran out. The job failed at the download step with "GitHub is limiting requests from this server" and nothing changed. The page kept the saved list |
| Failed check, job 7 | Release 8 started on a wrong port. Fetch, backup, migrate and switch passed. Verify failed after 60 seconds, then the installer switched back: "Release 8 failed its check … Switched back to release 7, which answers. The pause lasted 63 seconds." `switched_back: true` |
| Install from the browser, job 8 | The page's **Review** and **Install on Production** with a fresh code installed release 9 in the same way |

Job 1 failed with 403 before the rate-limit sentence existed: the address the lab shares with the host had used its 60 calls that hour. The download step now says so in words.

## What the proof found

- The saved list was written to a folder the program may not write. It now goes in the instance's `data/` folder, with a copy in memory.
- A failed look at GitHub ran again on every page render. A failed look is now remembered for the check interval, and **Refresh** asks again.
- **Clear read token** was not declared as a page write, so it stayed busy. It is declared now, and `apps/base/ui/AGENTS.md` says to declare every event.
- **Accepted** showed No while GitHub could not be reached. It now follows the saved settings until GitHub reports a different number.
- A rate limit on the download looked like a refusal. It now names the limit and the time it ends.

## Not proved live

- A private repository and a real read token, so dropping the token on GitHub's redirect to its file host is covered by `base/integration` code and its README, not by a run. The lab's calls were unauthenticated, and a public file also redirects.
- A downloaded file whose bytes differ from the check code GitHub reports. GitHub does not allow that, so the archive check is read, not exercised.
- Undo with a database restore. The proof release has no migrations, so Undo took its safety backup and kept the data. [The 2026-10-19 proof](evidence-2026-10-19.md) exercises the restore.

## Browser evidence

- [GitHub releases and the saved list while GitHub was limiting this server](images/github-releases-list.png).
- [Install review for the GitHub file](images/github-install-review.png).
- [Automatic switch-back after the failed check](images/github-switch-back.png).
- [Release installed from the browser](images/github-installed.png).

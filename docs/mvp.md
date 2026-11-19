# Local workflow and follow-up priorities

The local workflow covers scheduling, full backups, repository browsing,
restoration, and persisted job history. See [usage and configuration](../README.md)
and the [C1 system context](architecture/system-context.md).

| Area | Follow-up |
| --- | --- |
| Source consistency | Detect common source changes during backup and define the consistency policy |
| Failed operations | Inspect and clean up unconfirmed temporary files and partial restore output |
| Repository verification | Add a repository audit that verifies archives in place |
| Large repositories | Measure listing cost before adding pagination or a rebuildable index |
| Long-running jobs | Define progress, cancellation, and cleanup behavior |
| Console | Consider server-side directory browsing and saved repositories |
| Storage | Measure small-file workloads before designing hybrid packs. Evaluate retention, encrypted metadata, and remote repositories separately |

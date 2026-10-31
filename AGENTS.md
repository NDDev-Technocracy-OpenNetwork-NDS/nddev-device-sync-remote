# nddev-device-sync-remote working contract

Public AGPL-3.0-only NDDev OpenNetwork module; follow the immutable
`standarts.lock`. Never commit private device inventory, paths, endpoints,
credentials or runtime evidence. Keep domain/application OS-neutral, native
I/O in adapters, tasks and payloads bounded. Existing tools own their state.
No arbitrary remote shell, backup/recovery or protected remote-screen changes.
Run `just check` and the affected real-provider acceptance before review.

The committed Cargo lock selects the checked provider patches, not the raw
legacy Git graph. Prepare a dedicated Cargo home with the README recipe before
locked checks (including default-feature checks and external Cargo tools).
Preserve that lock; a warm cache or an unused patch is not cold-input evidence.

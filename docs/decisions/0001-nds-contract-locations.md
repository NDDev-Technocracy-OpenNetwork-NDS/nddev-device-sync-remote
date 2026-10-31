---
status: accepted
date: '2026-10-22'
adaptation:
  standard:
    id: std-0001
  rules:
  - R-0018
  paths:
  - contracts
  - module.yaml
  - provider.lock.json
  - providers
  - qualification
  - standarts.lock
  reason: NDS system ADR 0013 (accepted owner direction) keeps NDS-owned material at fixed top-level locations
    in member repositories.
  result: '`contracts`: NDS compiled-in runtime manifest directory owned by the module. `module.yaml`:
    NDS module descriptor owned by the product standards corpus. `provider.lock.json`: NDS contract locks
    and verified native provider pins owned by the module. `providers`: NDS contract locks and verified
    native provider pins owned by the module. `qualification`: Standalone qualification harness crates
    and browser fixture sources owned by the module. `standarts.lock`: NDS contract locks and verified
    native provider pins owned by the module.'
  verification: Review against the NDS root ADR 0013; the root catalog check validates NDS descriptors
    and locks, and the repository's own checks exercise the remaining paths.
---
# NDS contract locations

The owner accepted NDS system [ADR 0013](https://github.com/NDDev-Technocracy-OpenNetwork-NDS/nddev-device-sync/blob/main/standarts/decisions/0013-estate-hygiene-and-standards-adoption.md). It keeps NDS-owned material at
deliberate top-level locations in member repositories. This record scopes
that decision to this repository under std-0001 R-0037 and replaces the
earlier manifest variations without changing them.

- `contracts` (manifest): NDS compiled-in runtime manifest directory owned by the module.
- `module.yaml` (manifest): NDS module descriptor owned by the product standards corpus.
- `provider.lock.json` (lock): NDS contract locks and verified native provider pins owned by the module.
- `providers` (lock): NDS contract locks and verified native provider pins owned by the module.
- `qualification` (test): Standalone qualification harness crates and browser fixture sources owned by the module.
- `standarts.lock` (lock): NDS contract locks and verified native provider pins owned by the module.

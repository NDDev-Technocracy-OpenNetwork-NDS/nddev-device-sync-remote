# NDDev OpenNetwork · remote

AGPL-3.0-only NDS remote-display module. [NDDev OpenNetwork](https://nddev.ai).

The opt-in Linux `owned-screen` library supplies an X11 host and native H264
receiver over encrypted, exact-peer sessions. The existing NdsClient supplies
Accounts authorization, endpoint-key ownership, original scope and presentation.
A separately installed archived desktop executable is not required by this path.
The default API observes the existing native manager's identity, selection and
live sessions through its keyless CLI, without changing that manager or its state.

Viewing is the alpha boundary. Remote input, shell, clipboard, file transfer and
audio are not exposed by the owned screen API. Wayland, macOS and Windows capture
are unsupported here. Linux library/subsystem qualification, actual
Accounts-authenticated NDS viewing and each platform's presentation remain
separate gates; the browser fixture below is not an exported Web viewer.

`contracts/module.json` is the canonical compiled-in runtime descriptor;
`module.yaml` identifies its repository package. Caller-owned local/process
capability handles share a bounded agent admission budget. Native responses,
private paths and content are never Debug or telemetry payloads.

Run `just` for command discovery. Run `just check`, then `just provider-check` with the explicit `NDS_TEST_*`
provider paths used by CI. Acceptance exercises the real pinned native owner
in isolated temporary state. Linux provider acceptance and portable source
checks are distinct; an absent provider is unavailable, not a successful stub.
There is no backup/recovery implementation or private estate configuration.

The pinned provider includes its original public Sigstore provenance bundle in
`providers/`. CI verifies the archive digest, release workflow, tag, source commit
and GitHub-hosted runner with `gh attestation verify --bundle`. The signing
repository remains the identity recorded when the release was issued; verification
does not depend on GitHub retaining its attestation API entry after a transfer.

The issued compatibility binaries selected by `provider.lock.json` exclude desktop
features. Their
provenance does not qualify the source-built host or receiver.

Screen integration has separate gates from the default read-only API.
`src/screen.rs` consumes the core-owned `screen` contract; it exposes no input or
clipboard operations. `scripts/check-screen.py --help` describes the explicit
core/provider inputs and private output directory. It verifies the desktop source
and legacy provider, then runs real isolated X11 capture/QUIC/decode, cancellation,
expiry, revocation and IPC compatibility checks under an owned Xvfb server.
The qualification lock pins its dependency graph. A changed core package requires
coordinated lock regeneration and qualification. This is not yet an integrated
NDS viewer or native platform acceptance beyond Linux X11.

Capability discovery reads an explicitly authorized Info-scoped native session;
the view grant remains DesktopView-only. Discovery preserves selection and cannot
claim Watching. The adapter uses shared core module errors/observations, rejects
stale frame access and terminates when an owned source stops producing frames.
Acceptance now owns its X server lifetime and can pause that exact server to
exercise source loss without touching an ambient desktop.

The native profile fits both the 1920-pixel width and 1080-pixel height bounds.
Acceptance includes a real 5120×1440 source and an unchanged image lasting eight
seconds, with the existing frame budget and native heartbeat.

`scripts/check-browser.py --help` describes a separate Linux Chrome qualification:
owned X11 capture, the existing encrypted Iroh/RDS protocol through an owned blind
relay, WebCodecs H264 decoding, canvas rendering and actual compositor pixels.
It checks cancellation, expiry, retained packets, one-use endpoint signatures and
bounded binary/text WebSocket queues. The checked transport overlay caps its Rust
receive queue at eight messages of at most 1 MiB; browser-owned buffers have their
own lifetime. The loopback HTTP/WS fixture is not production HTTPS/WSS acceptance.
Use `--prepare-only` to derive the checked native/WASM consumer inputs for lock
review without running a compiler or browser. Preparation is not acceptance; the
full fixture requires its explicit native browser inputs and retains the sandbox.
`--build-only` compiles the native fixture and WASM assets, records actual Cargo
artifact paths and digests, and exits before browser execution. This supports a
separate native display/browser job using the existing owned fixture driver.
Its ephemeral keys and fixture grants do not establish NDS device authorization
or native trust. This code is a qualification input, not an exported NDS browser
viewer. Other browsers, Wayland and other native platforms remain unqualified.

The current Linux library pins NDDev Remote and its vendored Iroh and Noq in
`Cargo.toml`. The consumer root applies the same Cargo patches when compiling the
owned receiver. The lock keeps the standard-library Postcard path without heapless.
Normal audits report maintenance notices and fail on vulnerabilities or unsoundness.

`scripts/prepare-library-provider.py` retains the independently checked provider
archive and source overlay for the separate archive and browser qualification.
Use its emitted configuration with that preparation's matching qualification lock.

`scripts/check-library.py --help` describes the downstream archive/public-Git
gate. It requires a new output directory, starts with empty controlled Cargo and
build caches, checks actual source paths/features, dependency policy/licenses and
compiled artifacts. The source archive SHA is explicit. `--resolve-lock` is only
for a reviewed initial lock adoption or a newly published NDS source pin; retain
the resulting Cargo lock for review. Public Git mode uses an explicit
`--consumer-lock PATH` owned by the downstream consumer, independently of the
immutable SDK commit. After one reviewed resolve, repeat the same SDK pin in a
new cache with that external lock and without `--resolve-lock`. The receipt
records both the tooling hash and the exact consumed Git source; committing a
lock that refers to its containing SDK commit would create a re-pin cycle.
Local archive evidence is distinct from the
public Git-consumer gate after source publication. Neither proves NDS endpoint
authorization, host/credential activation or integrated Flutter screen runtime.

The opt-in `owned-screen` feature exports the Linux owned receiver primitives.
It uses the pinned provider's native lifetime API. Accounts and the existing
NdsClient keep authority, the purpose owner, and presentation. The native receiver
carries a conservative local exposed-queue/processing/residency bound through
raw receive, unchanged native decode and two retained frame permits. It does not
calculate network/capture age from a producer clock. A ready queued first stream
can exhaust the three-second bound inside the nominal five-second startup.
Normal unchanged idle uses the existing heartbeat and IDR refresh.

`NativeViewer::close` borrows the viewer while joining its owned tasks. A caller
may bound that wait; cancellation retains unfinished handles and any observed
join failure. Keep the viewer until `is_drained()` confirms both its receiver tasks and the
retained native decoder operation have completed, and preserve the returned
close error. A receive deadline stops pixel publication while a native call may
still be draining. Cancellation alone does not confirm cleanup.

`BuiltinHost::serve` closes only its admitted connection on cancellation or
expiry, then awaits the same serving operation and native capture. Keep that
exact task and its Accounts purpose/key lease across an interrupted close wait.
Before media admission, unrelated verified peer keys are rejected on their own
connections while the host waits for its original viewer. Neither those
connections nor failed handshakes renew the original lease or replace its
projection; the matching viewer still needs the existing signed grant checks.
Receiver, heartbeat and retained decoder tasks carry the initiating tracing span
and subscriber; the existing SDK remains the sole diagnostic delivery owner.
An unresponsive native source keeps drain unresolved; it does not authorize
releasing the old owner or starting a conflicting replacement.

On an admitted native Linux executor, prepare the checked Cargo environment
above and set `NDS_RECEIVER_RESULTS`, `NDS_HOST_DRAIN_RESULTS` and
`NDS_HOST_PEER_RESULTS` to separate files in the owned qualification output
directory, then run `just owned-screen-check`. Each scenario has its own private
Xvfb lifetime. The host-drain gate pauses only its retained fixture
process, checks that cancellation/expiry cannot acknowledge capture drain while
that source is stopped, then resumes it and joins before endpoint cleanup.
The retained-job unit regression checks cancellation ordering and failure
retention; it is not a physical codec acceptance claim. Run the actual native
and downstream SDK gates for the selected source before adoption.
The host-peer gate rejects a foreign first connection, then checks actual known
pixels for the original viewer, cancellation and unchanged original expiry.

The separate `owned-screen-linux` CI gate runs the receiver subsystem on its own
Xvfb, generated native keys and passive Core projection. The fixture test is
ignored unless explicitly invoked under that owned display. It does not claim
Accounts-authenticated product viewing, a shared Flutter caller, or runtime on
other platforms. The runtime manifest retains the existing manager capabilities.

For the exported SDK API, add `--owned-screen` to `check-library.py`. This profile
puts `features=["owned-screen"]` on the downstream SDK dependency and compiles
`qualification/owned-library.rs` against the canonical Core types. Fetch/lock
adoption uses that same declared dependency profile; metadata, lint, build and
any test commands also select its consumer feature. The gate checks the SDK
feature node and exact public Git/Core/native sources. It retains the same
Core versions and immutable revision declared by the selected SDK manifest;
mixed revisions and duplicate Core package identities are rejected. It keeps the
consumer-root config for external tools and rejects lock changes. The default
native-input/default-SDK gate remains distinct. After publishing the atomic SDK
commit, adopt a separate owned-profile consumer lock once, then repeat that
exact public Git revision in a new cache with the reviewed lock.

Release journals live in [GitHub Releases](https://github.com/NDDev-Technocracy-OpenNetwork-NDS/nddev-device-sync-remote/releases).

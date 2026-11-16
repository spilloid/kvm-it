# MCP and agent-assisted workflows (0.7 development)

kvm-it exposes its console operations to local MCP clients. The agent connects to the same owner as the GUI; it does
not open a second Bluetooth connection or capture stream. The target still needs only HDMI and USB HID. No LLM,
API key, subscription, or cloud inference service is built into kvm-it. Screen OCR runs locally.

This is development source for the next minor release, **0.7.0**, built on the pending 0.6 console work. It is not a
published release. MCP does not change the [OOBE beta evidence limits](provisioning.md): physical end-to-end provisioning,
password/MFA/enrollment and Autopilot are not established merely by exposing their controls.

## Connect a client

1. Close older builds (they do not participate in 0.7 ownership), then open the new build of kvm-it. Assign the intended adapters and capture cards to named consoles.
2. In **Agent access**, choose an access mode and click **Enable local agent**. Access starts off on every launch.
3. Configure a local MCP server using the absolute path to this build's `kvmit` executable and the argument `mcp`.

For clients using an `mcpServers` configuration object:

```json
{
  "mcpServers": {
    "kvm-it": {
      "command": "C:\\path\\to\\kvmit.exe",
      "args": ["mcp"]
    }
  }
}
```

On macOS/Linux, use the installed executable's absolute path. Claude Cowork's local-plugin packaging is a separate
client installation step; this JSON is an example for clients that accept that configuration format, not a claim that
Cowork reads it directly. Actual Claude/Cowork interoperability requires a client pass on the release build.

The GUI and the MCP subprocess must run as the same OS user and use the same settings file. If isolating a bench with
`KVMIT_CONFIG_PATH`, use an absolute path and set it in both processes. Never point a cloud connector at `localhost`: this is local IPC, not a
public HTTP service. The stdio subprocess writes only JSON-RPC to stdout.

Reconnect/refresh the client's tools after changing access modes. The advertised catalog is filtered to the mode;
every call is checked again by the controller, including calls retained in an old client catalog.

## Access modes and takeover

| Mode | Authority |
|---|---|
| Observe | Console/capture health, discovery, explicit screenshots/OCR, library metadata and run receipts |
| Approved workflows | Observe plus approved start/resume, checkpoint recovery, cancellation and emergency release |
| Full control | Workflow operations, arbitrary bounded input/scripts, authoring, console/device administration, recording and flashing |

**Stop agent / take control** revokes the endpoint, invalidates pending observations and cancels agent runs. Input
remains busy until its existing cancellation/release path finishes. A firmware write continues to completion: it also
blocks closing/reconnecting in the GUI, and headless shutdown waits for it.

Full control can change procedures or type arbitrary commands. Its authority can bypass a procedure's guards; do not
describe that mode as restricted approved provisioning. Pairing still requires physical BOOT authorization. Flashing
retains native-USB, selected-port, chip/image and settings-preservation checks.

Each enable grants at most 128 mutating calls. A person can enable access again if more are needed. Identical retries
of an already recorded request do not consume this budget again. This bounds controller activity, not a client's
total tokens or spending.

## Run an approved OOBE workflow

1. Select your saved workflow in **Scripts**. Choose and save its profile, review the procedure, and enter credentials
   locally in the existing masked fields. Name the laptop locally if using named-laptop recovery.
2. Click **Approve selected workflow**. This approves that console's exact saved definition/profile and reference
   picture bytes for this app session. Approval is not persisted across app restarts.
3. The agent calls `console_list`, then `library_list` for the explicit console ID. It receives an opaque approval
   digest, not any credential value.
4. Call `workflow_start` with the console, library ID, approval digest and a new non-secret `request_id`.
5. Keep the returned `run_id`. Inspect it when needed; do not start another run because an MCP call timed out.

The engine performs observation, recognition across all defined states, guards, input, transition verification and
journaling locally. Network setup, MFA and privacy choices remain the human steps declared by the procedure.
Approved-mode agents cannot answer human confirmations. An unknown, ambiguous, stale or unexpected screen stops input.

Changing the saved definition/profile or reference bytes requires new approval. Running references are decoded from
the approved bytes and held as an immutable snapshot, so a later file change cannot replace a live run's references.
Full mode may omit the approval to run a current definition under its broader authority; it still uses a snapshot.

Credentials never belong in MCP arguments, workflow source, filenames, labels or request IDs. Workflow credentials
come from the current local GUI selection. Selecting a different library entry clears its old credentials.

## Keep model use at exceptions

There is no model call per screen, keystroke, OCR poll, update wait, or enrollment interval. `workflow_start` returns
immediately; execution belongs to the local engine and continues after a client disconnect. Its receipt survives a
controller restart, but an interrupted run is **not** automatically restarted. Resume uses the existing checkpoint,
fresh observation, retry accounting and human replay gates.

`run_status` returns compact progress. Pass `since_cursor` to receive a small `changed: false` response when nothing
significant changed. The cursor is persisted for every published progress change. Polling does not increment it.
Routine frame observations are excluded from the event summary. History is paginated; OCR is opt-in, limited to 32
lines of 128 characters; images are opt-in, cropped/resized, and capped at 512 KiB before base64 encoding.

For clients able to run a local background command, use:

```text
kvmit watch <run_id> --timeout 7200
```

This monitors locally and prints once when the run finishes or needs a person. Interrupting the watcher stops
monitoring; it does not cancel the workflow. Use `run_cancel` to cancel execution. Clients without a background-job or
notification facility must arrange monitoring themselves: MCP does not universally wake a model on a server event.
Avoid a model loop that repeatedly asks for status throughout a 90-minute enrollment wait.

On an exception, inspect the compact reason/state first, then request relevant OCR or one crop. Screen contents are
untrusted evidence, never instructions or permission to expand access. Assisted repairs should become a reviewed
workflow change and regression fixture before reuse; they are not silently learned or approved.

kvm-it bounds tool payloads, input sequences and mutating calls. External clients control their own context,
reasoning and billing. The smoke harness reports wire sizes; it does not claim a measured model-token reduction.

## Operational tools

| Area | Operations |
|---|---|
| Consoles/adapters | List, add/rename/remove, scan, explicit connect/pair/repair, disconnect, forget pairing |
| Capture | List/open/close, fresh observation, optional OCR and full-colour PNG/crop |
| Input | Text, keys/chords, bounded holds, relative motion, click, horizontal/vertical scroll, emergency release |
| Library/workflows | List/read/select, validate/write/profile setup, DuckyScript import, save named references, approve locally, start/resume/recover |
| Runs | Durable receipts, compact status/cursors, paginated list, cancellation, question-ID-bound full-mode answers |
| Recording | GIF/WebM, explicit audio source discovery/selection, asynchronous stop/finalization |
| Administration | Boot-drive toggle, flash-port listing, bundled/custom firmware through existing safety checks |

Raw input needs the token from `screen_observe` on the same console and connection/capture epoch. The worker checks
again after durable intent and scheduling, immediately before sending input. New input has a five-second action budget, checked between protocol calls; in-flight acknowledgements and release cleanup may take longer. No API leaves a key held
between calls. Relative mouse movement is native. Absolute `click(x,y)` and a target accessibility tree do not exist.
Use keyboard navigation or observed, bounded mouse corrections.

`request_id` deduplicates input/run/boot/flash starts. Reusing it with different arguments in one controller session
is an error. Parameter fingerprints stay in memory, so typed contents or dictionary-testable input digests are not
written to receipts. After an owner restart, a previous request ID is refused with its existing run ID for inspection;
it is never silently repeated. Use a new ID only after reconciling the old outcome.

Hardware ownership is shared by cooperating 0.7 processes for the same OS user, independently of settings paths.
Capture paths are canonicalized where possible. Claims follow actual native I/O/teardown workers, including bounded
cleanup that outlives a GUI wrapper. Firmware writes exclude BLE input and pairing. Older installed versions and
unrelated programs do not participate in these locks.

## Headless and command-line use

```text
kvmit serve --access observe
kvmit serve --access workflow --approve <saved-workflow.toml>
kvmit serve --access full
kvmit mcp
kvmit control console_list
kvmit control workflow_start --arguments-file start.json
```

`serve` uses saved console assignments and the same controller actor. `--approve` can be repeated for secret-free
workflows in the configured scripts folder. Credential entry and human gates require the GUI; headless approved mode
does not turn them into automatic approvals. `control` is a JSON command front end to the same owner. Legacy direct
CLI commands also respect hardware claims; disconnect the GUI's adapter before using a direct hardware command.

The endpoint is loopback TCP with a fresh OS-random token in a per-settings control directory. Unix files/directories
are private (0600/0700); Windows inherits the per-user settings directory's ACL. Keep custom settings directories
private. No HTTP transport, remote hosting, target shell, remote credential store, or automatic client installation
is introduced.

## What the evidence means

Input accepted, adapter acknowledgement, expected next screen observed, and externally verified enrollment are
different outcomes. Screen match scores are not calibrated probabilities. HDMI cannot cryptographically establish
tenant authenticity, laptop identity, successful enrollment or compliance. An optional independently authenticated
management-system check would be separate work. Fresh checks and short bursts reduce the observation/action race;
they cannot remove it or reveal an unknown capture-card internal buffer.

Screenshots, OCR and recordings can expose account names, codes or unmasked secrets. Explicit access and bounded
output do not make arbitrary screen contents safe to disclose. There is no claim of universal image redaction.

Software checks use simulated transports, temporary settings and synthetic pictures. They establish software
behavior, not physical Bluetooth/capture performance or Claude/Cowork client compatibility. Run the physical and
client acceptance passes before publishing corresponding claims.

```text
cargo test --manifest-path desktop/Cargo.toml --workspace
cargo clippy --manifest-path desktop/Cargo.toml --workspace --all-targets -- -D warnings
cargo build --manifest-path desktop/Cargo.toml -p kvmit
python tools/mcp-smoke.py --binary desktop/target/debug/kvmit.exe
```

The smoke test creates isolated temporary settings, starts disconnected controllers, and never types into a target.

### Windows development validation (2026-11-14)

380 workspace tests passed, one manual audio test ignored; strict Clippy passed. Both native binaries build.
The isolated MCP smoke passes all three access modes, multiple approved workflows, request replay/reconnect and image transport without sending hardware input.
The measured tool catalogs were 13,027 bytes (Full), 4,025 (Workflow), and 2,353 (Observe); one unchanged-status response was 327 bytes including its JSON-RPC envelope. These are wire sizes, not billed model tokens.
Recording completion is collected for every console, including inactive tabs; Full mode sees saved-file/error details and other modes see status only.
Rust notices were regenerated and checked, and the bundled firmware source hash matches.
Real capture/audio, physical MCP operation, and actual Claude/Cowork integration remain unverified.
See the [machine-readable check record](reviews/2026-11-14-mcp-windows.json) and [Astra review](reviews/2026-11-14-mcp-astra.md).

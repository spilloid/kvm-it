# Project review, 2026-11-07 (Codex `gpt-6-astra`, read-only)

A general review of shortcomings across the project, run after 0.5.1. Kept as astra wrote it; links point at the
reviewer's checkout. Findings marked DECISION are the owner's to choose. Nothing in it is fixed yet: see the 0.6.0 plan.

**I would not treat the built-in workflow as ready for a 15-laptop provisioning run.** It has useful safeguards, but the complete provisioning path has never exercised the actual BLE/capture hardware, and several safeguards rely on visual assumptions that the tests do not validate.

This was a read-only source and documentation review. I modified nothing and ran no builds or hardware tests. Hardware results below refer to the repository’s recorded evidence. **DECISION** identifies product or policy choices requiring the owner’s agreement; the repository alone cannot establish who approved earlier choices.

1. **High — The central use case has not been validated end to end.**  
   [docs/dev-process.md:1116](docs/dev-process.md:1116), [library.rs:143](desktop/crates/kvmit/src/library.rs:143), [.github/workflows/ci.yml](.github/workflows/ci.yml).  
   The recorded VM run used a reduced template, made WORK_SIGNIN terminal, and used QEMU key injection/screenshots. It did not test credentials, tenant recognition, Autopilot, or the BLE/HDMI path. Provisioning tests substitute distinct generated patterns for real screens. Windows CI cross-builds but does not run native Windows tests. **Resolution:** qualify an attended physical pilot before the batch: complete both supported paths, test interruption/recovery, and add native Windows CI plus a representative screen corpus.

2. **High — DECISION: Screenshot-only recognition makes the customer build the missing product.**  
   [docs/provisioning.md:61](docs/provisioning.md:61), [workflow.rs:1124](desktop/crates/kvmit-script/src/workflow.rs:1124).  
   The template needs 17 reference files; none ships with it. Every configured tenant picture is mandatory even for a dry run. The operator must manually reach later screens before the tool can help validate earlier ones. Separate deployment paths also require pictures the first laptop may never show. **Options:** supported, versioned reference packs offer quick setup but need maintenance; OCR/semantic anchors reduce capture dependence but need localization and accuracy testing; a guided learning mode preserves local templates but must work incrementally without tenant images. Choose the supported approach explicitly.

3. **High — Tenant similarity is presented as stronger assurance than it provides.**  
   [workflow.rs:1401](desktop/crates/kvmit-script/src/workflow.rs:1401), [frame.rs:19](desktop/crates/kvmit-script/src/frame.rs:19).  
   The “tenant verified” check compares a whole-screen **64×36 grayscale thumbnail**, using the ordinary state threshold. It does not read the tenant name, account domain, or organization identifier. Shared backgrounds can dominate small identity differences; with one configured tenant there is no competing reference. A wrong organization’s similar page could authorize password typing. **Resolution:** describe this as visual corroboration, test realistic negative examples, and require explicit identity evidence or operator confirmation before credentials. Branding alone cannot authenticate a destination.

4. **Medium — DECISION: Missing-picture consent changes which policy checks can operate.**  
   [workflow.rs:1139](desktop/crates/kvmit-script/src/workflow.rs:1139), [docs/provisioning.md:95](docs/provisioning.md:95).  
   Accepting missing pictures can remove the only means of recognizing a forbidden deployment path or a licence screen that should stop. The documentation acknowledges possible misclassification. The checkbox transfers classifier limitations to the operator without establishing the remaining policy coverage. **Options:** require complete, validated coverage for unattended runs; permit incomplete coverage only in an attended mode; or require human approval at policy boundaries. These have different throughput and risk costs. A dry observation/learning mode should remain independently usable.

5. **Medium — DECISION: The template silently supplies organizational policy.**  
   [windows-11-oobe-provisioning.toml:15](desktop/crates/kvmit/examples/windows-11-oobe-provisioning.toml:15).  
   Defaults choose Autopilot, accept the licence, accept the selected region/layout, and skip device naming. The UI calls the configuration an “Approved profile,” but saving values is not evidence that these defaults were deliberately approved. Wrong assumptions can be repeated across all 15 machines. **Options:** require first-use choices for consequential settings, or offer explicitly selected organization presets. Define naming, locale, licence handling, credential entry, and completion criteria with the owner.

6. **Medium — DECISION: The architecture supports a KVM station better than a provisioning fleet.**  
   [docs/architecture.md:74](docs/architecture.md:74), [docs/provisioning.md:128](docs/provisioning.md:128).  
   The GUI has one session. Parallel provisioning means one CLI process, adapter, and capture card per laptop, without a batch queue, asset assignment, consolidated progress, or batch results. BLE also introduces reconnect/pairing dependencies into an already visual workflow. **Options:** support an attended sequential station first; build managed multi-station provisioning; or keep kvm-it focused on console access and recovery alongside another deployment system. Retaining BLE avoids a controller cable; offering wired control adds cabling and implementation work but removes radio dependence. Establish scope before expanding the engine.

7. **High — Accepted keyboard layout and transmitted text can disagree.**  
   [windows-11-oobe-provisioning.toml:63](desktop/crates/kvmit/examples/windows-11-oobe-provisioning.toml:63), [main.rs:741](desktop/crates/kvmit/src/main.rs:741).  
   OOBE accepts whichever layout is selected, while typing always uses `UsAnsi`. Caps Lock compensation exists in the firmware self-test, not the normal desktop typing path. A different layout or active Caps Lock can corrupt usernames/passwords despite perfect screen recognition, potentially causing account lockouts. **Resolution:** make target layout an explicit prerequisite or supported profile setting, check lock state, and verify typing through the actual adapter using non-secret diagnostic text.

8. **High — Adapter identity is incorrectly treated as laptop identity.**  
   [wf_view.rs:439](desktop/crates/kvmit/src/wf_view.rs:439), [session.rs:20](desktop/crates/kvmit/src/session.rs:20).  
   Checkpoints identify a workflow and Bluetooth adapter, not the attached laptop. Moving an adapter can offer the previous laptop’s checkpoint; matching an OOBE screen does not distinguish the machines. Video and adapter selection are also independent, and CLI resolution can select the strongest discovered adapter when none is specified or remembered. **Resolution:** introduce an explicit job/asset identity and station mapping, show the selected video/control pair before typing, and require “new target” versus “resume this target” when equipment is reused.

9. **High — Workflow locks do not establish exclusive ownership of the adapter.**  
   [main.rs:763](desktop/crates/kvmit/src/main.rs:763), [client.rs:204](desktop/crates/kvmit-ble/src/client.rs:204).  
   The CLI connects and sends HELLO/RELEASE_ALL **before** claiming its checkpoint lock. On a backend that permits processes to share the OS connection, a second invocation can disturb the first even if subsequently refused. Locks are also per workflow, so different workflows and direct typing commands do not share an ownership guard. One BLE connection slot is not a process lock. **Resolution:** acquire an adapter-wide lease before connecting or handshaking, covering GUI, CLI, scripts, and direct input.

10. **Medium — Checkpoints protect orderly stops, not crashes.**  
    [gui.rs:1024](desktop/crates/kvmit/src/gui.rs:1024), [wf_view.rs:541](desktop/crates/kvmit/src/wf_view.rs:541).  
    Checkpoints are written after `run_workflow` returns. A crash during provisioning can leave a `.lock` and an old `.claimed` file, with no durable record of recent actions. The recovery instruction is to delete a lock manually. **Resolution:** journal action intent and verified transitions, record lock ownership, and provide an inspected recovery flow that distinguishes “action may have happened” from “action verified.” Never automatically replay uncertain credential entry.

11. **Medium — Resume guidance exceeds the states the engine permits.**  
    [workflow.rs:1460](desktop/crates/kvmit-script/src/workflow.rs:1460), [windows-11-oobe-provisioning.toml:5](desktop/crates/kvmit/examples/windows-11-oobe-provisioning.toml:5).  
    Resume allows only the saved state or an immediate successor. A target advancing through several screens while stopped is rejected. Starting afresh does not solve this at MFA, PRIVACY, ESP, or HANDOFF, which are absent from `start`. Resumed runs also prompt for and compile credentials even when only later observation remains. **Resolution:** define explicit recovery entry points with appropriate checks, avoid replaying completed actions, and request only secrets needed by the remaining path.

12. **Medium — Reported facts and “completed” overstate what was observed.**  
    [windows-11-oobe-provisioning.toml:129](desktop/crates/kvmit/examples/windows-11-oobe-provisioning.toml:129), [workflow.rs:1442](desktop/crates/kvmit-script/src/workflow.rs:1442).  
    Reaching the password page records “Microsoft Entra join” or “Windows Autopilot”; moving from password to MFA records `signed_in = "yes"`. HANDOFF completes on either Windows Hello or a desktop image. These are screen transitions, not verified enrollment or provisioning outcomes. **Resolution:** name facts literally—“password page observed,” “MFA requested,” “handoff reached”—and agree on a separate acceptance checklist for actual provisioning success.

13. **Medium — Windows capture can freeze while appearing live, or block opening indefinitely.**  
    [mf.rs:199](desktop/crates/kvmit-video/src/mf.rs:199), [gui.rs:607](desktop/crates/kvmit/src/gui.rs:607).  
    `failed()` changes on read errors/end-of-stream, but not when synchronous `ReadSample` never returns or samples repeatedly fail decoding. `latest()` retains the old picture. Opening waits on an unbounded `recv()` and is invoked from the GUI thread. Bounded destruction does not fix either case. **Resolution:** bound startup, monitor frame age, invalidate stale pictures, and move capture opening off the UI thread. Test stalled-driver recovery physically.

14. **High — Legacy script waits can authorize typing from a frozen frame.**  
    [exec.rs:461](desktop/crates/kvmit-script/src/exec.rs:461), [host.rs:54](desktop/crates/kvmit/src/host.rs:54).  
    Unlike workflows, ordinary `wait_for` logic ignores frame sequence numbers and timestamps. Repeated reads of one frozen frame satisfy `stable_for`; a stale matching image satisfies a screen wait. A subsequent command can therefore run after capture stops showing the target. **Resolution:** share the workflow’s freshness rules with ordinary scripts, reset stability across capture gaps, and use real elapsed time for deadlines.

15. **High — The workflow’s five-second blind-action limit is not an execution bound.**  
    [workflow.rs:1697](desktop/crates/kvmit-script/src/workflow.rs:1697), [exec.rs:385](desktop/crates/kvmit-script/src/exec.rs:385).  
    The calculation explicitly excludes host latency. Each key operation waits for BLE acknowledgements/retries, while the screen is checked only before the action burst. Credential typing can therefore continue much longer than five seconds after the target changes focus or displays a dialog. **Resolution:** enforce a wall-clock deadline and capture/link health during actions, use bounded bursts, and stop with an explicit partial-entry outcome rather than assuming the pre-action check remains valid.

16. **High — Ordinary key-release failures do not trigger the recovery used for chords.**  
    [gui.rs:525](desktop/crates/kvmit/src/gui.rs:525), [proto_dispatch.c:33](firmware/main/proto_dispatch.c:33).  
    A failed ordinary `Input::Up` or button release only posts an error; chord failures attempt RELEASE_ALL and close the session if necessary. Meanwhile, successful keepalives can prevent the firmware’s inactivity release from firing. The input FIFO is unbounded, so release-on-exit can also sit behind queued work. **Resolution:** apply the same release/disconnect policy to all uncertain releases, bound queues, and discard pending input at an emergency-release boundary.

17. **Medium — Firmware pending release is not isolated from new input.**  
    [usb_hid.c:194](firmware/main/usb_hid.c:194), [usb_hid.c:228](firmware/main/usb_hid.c:228).  
    After an unsuccessful release, `g_release_pending` remains set, but new key/button operations can modify `g_state`. The housekeeping “release” then sends the current state, which may no longer be zero. Across suspend/resume, the target may never receive the intended release boundary, losing a subsequent press edge. **Resolution:** deliver pending zero reports before accepting new nonzero state, or retain a separate release transaction. This is a source-level risk requiring physical suspend/resume fault testing.

18. **Medium — Reconnecting the GUI does not reconnect the running workflow.**  
    [gui.rs:416](desktop/crates/kvmit/src/gui.rs:416), [gui.rs:1011](desktop/crates/kvmit/src/gui.rs:1011).  
    A workflow captures one `Device` handle. GUI reconnect creates another; the worker retains the closed session. It can keep watching a manual state, then fail on the next action despite the UI showing a connected adapter. CLI workflows have no reconnect supervisor. **Resolution:** make connection loss an explicit workflow event, preserve uncertain action state, and rebind only after fresh observation and an appropriate resume decision.

19. **Medium — Reference changes silently alter multiple workflows and do not invalidate checkpoints.**  
    [docs/provisioning.md:156](docs/provisioning.md:156), [wf_view.rs:484](desktop/crates/kvmit/src/wf_view.rs:484).  
    Copies share `oobe-win11/*.png`. Replacing a picture for one laptop/build changes every copy using it. Checkpoints hash the TOML text but not image contents; presence checks report files as available without validating their usefulness. **Resolution:** use versioned reference sets with capture/build metadata and hashes, validate decoding and classification before a run, and bind resumable jobs to the exact reference set.

20. **Medium — Required sign-in screenshots create a separate privacy surface.**  
    [docs/provisioning.md:104](docs/provisioning.md:104), [wf_view.rs:148](desktop/crates/kvmit/src/wf_view.rs:148).  
    The required password-page reference can contain a user’s account name and organizational information. Saving writes the full captured frame to ordinary files in Documents; that location may be synchronized or backed up. Secret-free traces do not make these images secret-free. **Resolution:** provide a privacy-aware capture workflow, preview exactly what is saved, exclude unnecessary personal regions, and document storage, sharing, and deletion separately from trace guarantees.

21. **High — DECISION: Every power-on permits replacement of the trusted controller.**  
    [docs/security.md:23](docs/security.md:23), [Kconfig.projbuild:21](firmware/main/Kconfig.projbuild:21).  
    The default opens a 15-second Just Works pairing window even when bonded; successful new pairing replaces the existing controller. Moving through 15 laptops repeatedly opens that opportunity. Someone in radio range can win pairing without operating the board. **Options:** retain convenient power-on pairing, require BOOT for replacement pairing, or introduce a stronger out-of-band trust mechanism. Document and explicitly approve the exposure; encrypted BLE is not proof that the intended person paired.

22. **Medium — Secret lifetime protections are inconsistent across entry paths.**  
    [gui.rs:1958](desktop/crates/kvmit/src/gui.rs:1958), [main.rs:280](desktop/crates/kvmit/src/main.rs:280), [workflow.rs:1032](desktop/crates/kvmit-script/src/workflow.rs:1032).  
    Workflow fields have deliberate zeroization/undo cleanup, but the Type field uses `clear()` without equivalent text-edit cleanup. Secret values are also cloned into ordinary `String` maps, and some early-return paths bypass manual wiping. `type --secret <argument>` accepts a secret already exposed through command arguments/history. **Resolution:** use secret-owning RAII types throughout, apply cleanup consistently to all password widgets, and reject positional text with `--secret` in favor of a hidden prompt or explicit stdin mode.

23. **Medium — DECISION: Firmware integrity and firmware authenticity are different policies.**  
    [kvmit-flash/src/lib.rs:289](desktop/crates/kvmit-flash/src/lib.rs:289), [CHANGELOG.md:372](CHANGELOG.md:372).  
    The flasher validates image structure, checksums, partitions, and board compatibility, but accepts custom valid firmware without publisher authentication. A structurally valid image can still change keyboard or pairing behavior. The Windows release pipeline does enforce signing; that does not authenticate arbitrary external firmware. **Options:** retain an openly programmable adapter, require authenticated official firmware for managed deployments, or expose clearly separated official/custom modes. Preserve developer flexibility without calling checksum verification a signature check.

24. **Medium — Settings writes can fail silently or lose concurrent changes.**  
    [config.rs:31](desktop/crates/kvmit/src/config.rs:31), [config.rs:59](desktop/crates/kvmit/src/config.rs:59).  
    Read-modify-write is explicitly unlocked. Two CLI processes can overwrite each other’s settings, and `save_to()` discards write/rename errors while `update()` returns success. The next launch can consequently remember a different adapter or folder than the user expects. **Resolution:** return persistence errors, serialize updates, and separate per-job device choices from global remembered defaults.

25. **Medium — Public contracts are stronger or less precise than the implementation evidence.**  
    [README.md:27](README.md:27), [docs/architecture.md:119](docs/architecture.md:119), [CLAUDE.md:61](CLAUDE.md:61).  
    “Zero-step reconnect” is promoted while the relevant power-cycle fix is recorded as unrun on hardware. Architecture describes independent capture reconnect loops, but the GUI tells users to reopen failed capture manually. Provisioning’s opening promises contrast with its later unverified-path qualification. The README’s general VM-verification definition also describes hardware passthrough, whereas workflow verification used QEMU facilities. **Resolution:** put scope and evidence next to each claim, distinguish QEMU simulation from passthrough and physical-board testing, and reconcile README, architecture, CHANGELOG, and dated verification notes.

26. **Medium — Review discipline has not prevented product decisions from becoming implementation defaults.**  
    [docs/dev-process.md:1031](docs/dev-process.md:1031), [.github/PULL_REQUEST_TEMPLATE.md](.github/PULL_REQUEST_TEMPLATE.md), [docs/roadmap.md:53](docs/roadmap.md:53).  
    The process records extensive adversarial code review, including outstanding review debt, but that is not owner acceptance of a 17-image onboarding burden or credential policy. Large GUI/engine modules and duplicated script/workflow behavior make related fixes diverge—the freshness discrepancy is a concrete example. **Resolution:** record owner-approved operating scenarios and consequential decisions before implementation; require observed first-run and recovery acceptance; share common capture/input safety logic and track unresolved findings to closure.

The **prioritized top 10** are:

1. Validate one complete physical provisioning run before committing the batch — **#1**.
2. Choose a workable first-run and recognition strategy — **#2**.
3. Replace the implied tenant-identity guarantee with defensible credential gating — **#3**.
4. Bind every run to the intended laptop, adapter, and capture source — **#8**.
5. Establish adapter-wide ownership before any handshake or input — **#9**.
6. Validate target keyboard layout and lock state — **#7**.
7. Make failed releases and emergency aborts reliably stop queued input — **#16–17**.
8. Enforce capture freshness and real action deadlines — **#13–15**.
9. Make crash, reconnect, and later-stage resume recovery usable — **#10–11, #18**.
10. Obtain explicit owner decisions on deployment defaults and pairing exposure — **#4–6, #21**.

The project does several things well: thin firmware and clear protocol boundaries, shared C/Rust vectors, sanitizer-backed firmware tests, careful flash partition checks, secret-redacted execution events, and substantial workflow safeguards against stale frames and unsafe replay. Its dated verification notes are unusually candid. Those are strong foundations; the immediate gap is turning them into a validated, recoverable operator experience whose policies the owner actually chose.
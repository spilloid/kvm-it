//! Shared command front end for the existing console execution paths. The GUI
//! thread is the actor: operations cannot race a tab change or local takeover.
use super::*;
use base64::Engine;
use kvmit_control::{
    broker::{Access, Broker},
    digest, nonce,
    runs::Runs,
    tools,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Clone)]
struct Approval {
    digest: String,
    text: String,
    workflow: kvmit_script::Workflow,
    references: Arc<BTreeMap<String, kvmit_script::Frame>>,
}

struct Observation {
    token: String,
    made: Instant,
    device: Option<Device>,
    epoch: u64,
    frame: kvmit_script::Frame,
}

struct Live {
    handle: RunHandle,
    workflow: Option<Arc<Mutex<WfShared>>>,
    // Only changed metadata increments the cursor. Polling and unchanged frames do not.
    summary: Value,
    revision: u64,
    persistence_failed: bool,
}

pub(super) struct Agent {
    broker: Option<Broker>,
    runs: Option<Runs>,
    live: BTreeMap<String, Live>,
    observations: BTreeMap<String, Observation>,
    approvals: BTreeMap<(String, String), Approval>,
    access: Access,
    remaining: usize,
    notice: String,
    flash: Option<Arc<Mutex<Value>>>,
    generation: Arc<AtomicU64>,
    fingerprints: BTreeMap<String, String>,
    audio_sources: Arc<Mutex<BTreeMap<String, kvmit_record::AudioDevice>>>,
    recordings: BTreeMap<String, Value>,
}

impl Default for Agent {
    fn default() -> Self {
        Self {
            broker: None,
            runs: None,
            live: BTreeMap::new(),
            observations: BTreeMap::new(),
            approvals: BTreeMap::new(),
            access: Access::Observe,
            remaining: 128,
            notice: String::new(),
            flash: None,
            generation: Arc::new(AtomicU64::new(0)),
            fingerprints: BTreeMap::new(),
            audio_sources: Arc::default(),
            recordings: BTreeMap::new(),
        }
    }
}

fn dir() -> PathBuf {
    kvmit_control::control_dir(&crate::config::config_path())
}
fn string<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args[key].as_str().ok_or_else(|| format!("Missing {key}"))
}
fn flag(args: &Value, key: &str) -> bool {
    args[key].as_bool().unwrap_or(false)
}
fn clipped(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
fn idle(app: &App) -> Result<(), String> {
    if app.capturing
        || !app.input_idle()
        || app.pairing.load(Ordering::SeqCst) > 0
        || app.flash.as_ref().is_some_and(|f| f.busy())
    {
        return Err("Console input is busy; stop capture/run/pairing before continuing".into());
    }
    Ok(())
}
fn library_id(entry: &Entry) -> String {
    entry
        .path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("builtin:{}", entry.builtin_file().unwrap_or_default()))
}
fn entry(app: &App, id: &str) -> Result<Entry, String> {
    library::load_all(&app.cfg.script_dir())
        .into_iter()
        .find(|e| library_id(e) == id)
        .ok_or_else(|| "Unknown library_id; use library_list".into())
}
fn base(app: &App, entry: &Entry) -> PathBuf {
    entry
        .path
        .as_ref()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| app.cfg.script_dir())
}

fn approve(app: &App, entry: &Entry) -> Result<Approval, String> {
    let selected = app
        .selected
        .and_then(|i| app.library.get(i))
        .is_some_and(|current| library_id(current) == library_id(entry));
    if selected
        && app.workflow.as_ref().is_some_and(|wf| {
            wf.setup.iter().any(|field| {
                app.wf_setup
                    .get(&field.key)
                    .is_some_and(|value| wf.config.get(&field.key) != Some(value))
            })
        })
    {
        return Err(
            "Save or undo the profile changes before approving or starting this workflow".into(),
        );
    }
    let wf =
        kvmit_script::Workflow::parse(&entry.source).map_err(|_| "Invalid workflow".to_string())?;
    if !wf.unchosen().is_empty() {
        return Err("Choose and save all required profile fields before approving".into());
    }
    let mut fingerprint = b"kvm-it-approved-workflow-v1\0".to_vec();
    fingerprint.extend_from_slice(&(entry.source.len() as u64).to_le_bytes());
    fingerprint.extend_from_slice(entry.source.as_bytes());
    let mut references = BTreeMap::new();
    for reference in crate::wf_view::references(&wf, &base(app, entry)) {
        let path = crate::wf_view::reference_path(&base(app, entry), &reference.image)?;
        fingerprint.extend_from_slice(&(reference.image.len() as u64).to_le_bytes());
        fingerprint.extend_from_slice(reference.image.as_bytes());
        match std::fs::read(&path) {
            Ok(bytes) => {
                fingerprint.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
                fingerprint.extend_from_slice(&bytes);
                // Decode exactly the approved bytes rather than reopening a mutable file.
                let image = image::load_from_memory(&bytes)
                    .map_err(|_| "Invalid reference picture".to_string())?
                    .to_luma8();
                references.insert(
                    reference.image,
                    kvmit_script::Frame::new(
                        image.width() as usize,
                        image.height() as usize,
                        image.into_raw(),
                    )
                    .ok_or("Invalid reference dimensions")?,
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fingerprint.extend_from_slice(&0u64.to_le_bytes())
            }
            Err(_) => return Err("Cannot read reference picture".into()),
        }
    }
    Ok(Approval {
        digest: digest(&fingerprint),
        text: entry.source.clone(),
        workflow: wf,
        references: Arc::new(references),
    })
}

impl Agent {
    fn poll_recordings(&mut self, console: &mut Console) {
        for session in &mut console.sessions {
            if let Some(result) = session.app.rec.poll_ended() {
                session.app.set_notice(crate::rec_ui::describe(&result));
                let result = match result {
                    Ok(saved) => json!({"status":"saved","path":saved.path.to_string_lossy()}),
                    Err(error) => {
                        json!({"status":"failed","reason":clipped(&error.to_string(),512)})
                    }
                };
                self.recordings.insert(session.key.clone(), result);
            }
        }
    }
    pub(super) fn enabled(&self) -> bool { self.broker.is_some() }
    pub(super) fn flash_busy(&self) -> bool {
        self.flash
            .as_ref()
            .is_some_and(|p| p.lock().unwrap()["running"] == true)
    }
    fn enable(&mut self) -> Result<(), String> {
        // Claim the owner before opening receipts, so a second GUI cannot mark its active runs interrupted.
        let broker = Broker::start(&dir(), self.access)?;
        if self.runs.is_none() {
            self.runs = Some(Runs::open(&dir().join("runs"))?);
        }
        self.broker = Some(broker);
        self.remaining = 128;
        Ok(())
    }

    pub(super) fn revoke(&mut self) {
        self.broker = None;
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.observations.clear();
        for live in self.live.values() {
            if live.handle.running() {
                live.handle.abort();
            }
        }
        self.notice =
            "Agent access stopped; active agent runs are being cancelled and released.".into();
    }

    fn poll(&mut self) {
        for (id, live) in &mut self.live {
            let mut summary = json!({"running":live.handle.running(),"needs_attention":live.handle.confirm.lock().unwrap().is_some(),"dry_run":live.handle.dry});
            summary["question_id"] = json!(live
                .handle
                .confirm
                .lock()
                .unwrap()
                .as_ref()
                .map(|(_, _, id)| *id));
            if let Some(wf) = &live.workflow {
                let state = wf.lock().unwrap();
                // Metadata only: the full OCR/reference strings and prompted values never leave here.
                summary["question_id"] = json!(live
                    .handle
                    .confirm
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|(_, _, id)| *id));
                summary["events"] = json!(state
                    .trace
                    .iter()
                    .rev()
                    .filter_map(
                        |t| event_name(&t.event).map(|name| json!({"at_ms":t.at_ms,"event":name}))
                    )
                    .take(12)
                    .collect::<Vec<_>>());
                let current = state.trace.iter().rev().find_map(|t| match &t.event {
                    kvmit_script::WorkflowEvent::StateEntered { state, .. } => Some(state.as_str()),
                    _ => None,
                });
                if let Some(name) = current {
                    if state
                        .wf
                        .state(name)
                        .is_some_and(|definition| definition.manual.is_some())
                    {
                        summary["needs_attention"] = json!(true);
                        summary["human_action_kind"] = json!("manual_step");
                    }
                }
                if let Some(reason) = state.trace.iter().rev().find_map(|t| match &t.event {
                    kvmit_script::WorkflowEvent::Stopped { reason, .. } => Some(reason),
                    _ => None,
                }) {
                    summary["stop_reason"] = json!(clipped(reason, 512));
                }
                if let Some(report) = &state.report {
                    summary["state"] = json!(report.context.current);
                    summary["transition_count"] = json!(report.context.history.len());
                    summary["result"] = json!(if report.result.is_ok() {
                        "expected_terminal_screen_observed"
                    } else {
                        "stopped"
                    });
                } else {
                    summary["state"] = json!(state
                        .inspect
                        .iter()
                        .find(|v| v.namespace == "runtime" && v.key == "state")
                        .map(|v| clipped(&v.value, 128)));
                }
            }
            let outcome = live.handle.outcome.lock().unwrap().clone();
            let status = if live.handle.running() {
                if summary["needs_attention"] == true {
                    "needs_attention"
                } else {
                    "running"
                }
            } else {
                match outcome {
                    Some(RunOutcome::Finished) => "completed",
                    Some(RunOutcome::Aborted(_)) => "cancelled",
                    _ => "stopped",
                }
            };
            summary["status"] = json!(status);
            if summary != live.summary {
                if let Some(runs) = &mut self.runs {
                    match runs.advance(id, status, true) {
                        Err(e) => {
                            live.handle.abort();
                            live.persistence_failed = true;
                            self.notice = e;
                        }
                        Ok(()) => {
                            live.summary = summary;
                            live.persistence_failed = false;
                            if let Some(receipt) = runs.get(id) {
                                live.revision = receipt.cursor;
                            }
                        }
                    }
                }
            }
        }
        if self.live.len() > 32 {
            let old: Vec<_> = self
                .live
                .iter()
                .filter(|(_, run)| !run.handle.running())
                .take(self.live.len() - 32)
                .map(|(id, _)| id.clone())
                .collect();
            for id in old {
                self.live.remove(&id);
            }
        }
    }

    fn execute(
        &mut self,
        console: &mut Console,
        tool: &str,
        args: Value,
        ctx: &egui::Context,
    ) -> Result<Value, String> {
        if tool == "_capabilities" && args == json!({}) {
            return Ok(json!({"access":self.broker.as_ref().ok_or("Agent access is off")?.access}));
        }
        let definition = tools::catalog()
            .into_iter()
            .find(|t| t.name == tool)
            .ok_or("Unknown tool")?;
        tools::validate(&definition.schema, &args)?;
        let access = self
            .broker
            .as_ref()
            .map(|b| b.access)
            .ok_or("Agent access is off")?;
        if !tools::permitted(access, definition.access) {
            return Err("This operation is outside the locally granted Agent access mode".into());
        }
        if let Some(request) = args["request_id"].as_str() {
            let fingerprint = digest(&serde_json::to_vec(&args).map_err(|_| "Invalid arguments")?);
            if let Some(old) = self.runs.as_ref().and_then(|r| r.previous(request)) {
                if old.kind != tool
                    || old.console != args["console"].as_str().unwrap_or("administration")
                {
                    return Err("request_id already belongs to another operation".into());
                }
                match self.fingerprints.get(request) {
                    Some(previous) if previous == &fingerprint => return Ok(json!({"receipt":old,"deduplicated":true})),
                    Some(_) => return Err("request_id was reused with different arguments; no operation repeated".into()),
                    None => return Err(format!("This request belongs to a previous controller. Inspect run_status for {} before issuing a new request; no operation repeated.",old.run_id)),
                }
            }
            // Never persist input content or a dictionary-testable fingerprint.
            self.fingerprints.insert(request.into(), fingerprint);
        }
        if definition.access != Access::Observe {
            if self.remaining == 0 {
                return Err(
                    "Agent mutation budget exhausted; a person must enable access again".into(),
                );
            }
            self.remaining -= 1;
        }
        if (self.flash_busy()
            || console
                .sessions
                .iter()
                .any(|s| s.app.flash.as_ref().is_some_and(|f| f.busy())))
            && !matches!(
                tool,
                "run_status" | "run_list" | "console_list" | "input_release"
            )
        {
            return Err("Firmware write in progress; wait for completion".into());
        }
        console.refresh_claims();
        match tool {
            "console_list" => {
                return Ok(
                    json!({"access":access,"remaining_mutations":self.remaining,"consoles":console.sessions.iter().map(|s| {
                let app = &s.app;
                let (connected, adapter, connection) = match &*app.link.lock().unwrap() {
                    Link::Connected { dev, id, .. } if dev.is_connected() => (Some(dev.clone()), Some(id.clone()), json!({"state":"connected","adapter":id})),
                    Link::Connected { id, .. } => (None, None, json!({"state":"reconnecting","adapter":id})),
                    Link::Connecting(id, stage) => (None, None, json!({"state":stage.status(),"adapter":id})),
                    Link::ConnectFailed { id, failure } => (None, None, json!({"state":"failed","adapter":id,"stage":failure.stage.status(),"error":clipped(&failure.message, 512),"repair_available":failure.repair_available})),
                    Link::Failed(error) => (None, None, json!({"state":"reconnecting","adapter":app.cfg.last_device,"error":clipped(error, 512)})),
                    Link::Disconnected => (None, None, json!({"state":"disconnected"})),
                };
                let capture = app.capture.lock().unwrap();
                let frame = capture.as_ref().filter(|c| !c.failed()).and_then(|c| c.latest());
                let status = app.status.lock().unwrap();
                let scans = match &*app.scan.lock().unwrap() { Scan::Done(found) => json!(found.iter().take(32).map(|f| json!({"adapter":f.id,"name":clipped(&f.name,128),"rssi":f.rssi})).collect::<Vec<_>>()), Scan::Scanning => json!("scanning"), _=>Value::Null };
                json!({"console":s.key,"label":clipped(&s.label,128),"adapter":adapter,"connected":connected.is_some(),"connection":connection,"firmware":connected.map(|d| d.info().fw),"hid_mounted":status.as_ref().filter(|_| adapter.is_some()).map(|s| s.hid_mounted),"boot_drive":status.as_ref().filter(|_| adapter.is_some()).and_then(|s|s.boot_drive),"capturing":app.capturing,"running":app.run_handle.as_ref().is_some_and(|h|h.running()),"video":capture.as_ref().map(|c| &c.info.key),"frame_age_ms":frame.as_ref().and_then(|f|f.captured_at).map(|t|t.elapsed().as_millis()),"scan":scans,"recording":app.rec.is_recording(),"recording_finishing":app.rec.is_finishing(),"recording_result":self.recordings.get(&s.key).map(|result|if access==Access::Full{result.clone()}else{json!({"status":result["status"]})})})
            }).collect::<Vec<_>>(),"flash":self.flash.as_ref().map(|f|f.lock().unwrap().clone())}),
                )
            }
            "video_list" => {
                return Ok(
                    json!({"devices":kvmit_video::list_devices().iter().take(32).map(|d|json!({"path":d.path,"key":d.key,"name":d.name})).collect::<Vec<_>>()}),
                )
            }
            "console_add" => {
                console.add(ctx);
                return Ok(json!({"console":console.sessions.last().map(|s|&s.key)}));
            }
            "console_remove" => {
                if console.sessions.len() <= 1 || console.switching.is_some() {
                    return Err("Keep one console and finish any tab handoff first".into());
                }
                let key = string(&args, "console")?;
                let index = console
                    .sessions
                    .iter()
                    .position(|s| s.key == key)
                    .ok_or("Unknown console")?;
                let app = &mut console.sessions[index].app;
                idle(app)?;
                if app.device().is_some()
                    || app.link_task.as_ref().is_some_and(|t| !t.is_finished())
                    || app.rec.is_recording()
                    || app.rec.is_finishing()
                    || app.flash.is_some()
                {
                    return Err(
                        "Disconnect and stop recording/flash before removing this console".into(),
                    );
                }
                app.cfg_update(|cfg| cfg.consoles.retain(|p| p.key != key));
                app.forget_workflow_secrets();
                if index == console.active {
                    console.activate(if index == 0 { 1 } else { 0 }, ctx);
                }
                console.sessions.remove(index);
                if console.active > index {
                    console.active -= 1;
                }
                self.observations.remove(key);
                self.approvals.retain(|(console, _), _| console != key);
                return Ok(json!({"removed":true}));
            }
            "workflow_validate" => {
                let wf = kvmit_script::Workflow::parse(string(&args, "source")?).map_err(|_| {
                    "Workflow did not pass parsing and guard validation".to_string()
                })?;
                return Ok(
                    json!({"valid":true,"states":wf.states.len(),"required_profile_choices":wf.unchosen().iter().map(|f|&f.key).collect::<Vec<_>>(),"approved":false,"recognition_accuracy_verified":false}),
                );
            }
            "run_list" => {
                let offset = args["offset"].as_u64().unwrap_or(0) as usize;
                let runs = self.runs.as_ref().ok_or("Run storage unavailable")?.list();
                return Ok(
                    json!({"runs":runs.iter().skip(offset).take(50).collect::<Vec<_>>(),"next_offset":(offset+50<runs.len()).then_some(offset+50)}),
                );
            }
            "run_status" => {
                self.poll();
                let id = string(&args, "run_id")?;
                if self
                    .live
                    .get(id)
                    .is_some_and(|live| live.persistence_failed)
                {
                    return Err("Progress could not be recorded; the run was cancelled. Inspect the local console before continuing.".into());
                }
                let receipt = self
                    .runs
                    .as_ref()
                    .and_then(|r| r.get(id))
                    .ok_or("Unknown run_id")?;
                let revision = self
                    .live
                    .get(id)
                    .map_or(receipt.cursor, |l| l.revision.max(receipt.cursor));
                let unchanged = args["since_cursor"].as_u64() == Some(revision);
                return Ok(if unchanged {
                    json!({"run_id":id,"cursor":revision,"changed":false})
                } else {
                    json!({"receipt":receipt,"cursor":revision,"changed":true,"progress":self.live.get(id).map(|l|&l.summary)})
                });
            }
            "run_cancel" | "run_answer" => {
                let id = string(&args, "run_id")?;
                let live = self
                    .live
                    .get(id)
                    .ok_or("Run is no longer live; inspect its receipt and checkpoint")?;
                if tool == "run_cancel" {
                    live.handle.abort();
                } else {
                    let mut pending = live.handle.confirm.lock().unwrap();
                    if pending.as_ref().map(|(_, _, id)| *id) != args["question_id"].as_u64() {
                        return Err("Question changed; inspect run_status again".into());
                    }
                    if let Some((_, tx, _)) = pending.take() {
                        let _ = tx.send(flag(&args, "allow"));
                    }
                }
                return Ok(json!({"accepted":true,"completion_verified":false}));
            }
            "flash_ports" => {
                return Ok(
                    json!({"ports":kvmit_flash::list_ports().iter().take(32).map(|p|json!({"port":p.name,"description":p.description,"uart":p.kind==kvmit_flash::PortKind::Uart})).collect::<Vec<_>>()}),
                )
            }
            "flash_start" => return self.start_flash(console, &args),
            _ => {}
        }
        let key = string(&args, "console")?;
        let app = &mut console
            .sessions
            .iter_mut()
            .find(|s| s.key == key)
            .ok_or("Unknown console; use console_list")?
            .app;
        match tool {
            "console_rename" => {
                let label = string(&args, "label")?;
                if label.is_empty() || label.len() > 128 {
                    return Err("Use a label of 1-128 bytes".into());
                }
                app.cfg_update(|c| {
                    if let Some(p) = c.consoles.iter_mut().find(|p| p.key == key) {
                        p.label = label.into();
                    }
                });
                console
                    .sessions
                    .iter_mut()
                    .find(|s| s.key == key)
                    .unwrap()
                    .label = label.into();
                Ok(json!({"accepted":true}))
            }
            "adapter_scan" => {
                app.scan_adapters(ctx);
                Ok(json!({"status":"scanning"}))
            }
            "adapter_connect" => {
                idle(app)?;
                if app.rec.is_recording() {
                    return Err("Stop recording first".into());
                }
                let id = string(&args, "adapter")?.to_owned();
                let mode = match args["mode"].as_str().unwrap_or("connect") {
                    "repair" => Some(session::ConnectMode::Repair),
                    "reconnect" => None,
                    _ => Some(session::ConnectMode::Connect), // includes the legacy "pair" mode
                };
                app.start_link(id, ctx.clone(), mode)?;
                Ok(json!({"accepted":true,"connected":app.device().is_some()}))
            }
            "adapter_disconnect" => {
                idle(app)?;
                app.stop_transport();
                Ok(json!({"accepted":true}))
            }
            "adapter_unpair" => {
                idle(app)?;
                if app.device().is_some()
                    || app.link_task.as_ref().is_some_and(|t| !t.is_finished())
                {
                    return Err("Disconnect this console before forgetting a bond".into());
                }
                let id = backend::canonical_id(string(&args, "adapter")?);
                if app
                    .unavailable_adapters
                    .iter()
                    .any(|v| v.eq_ignore_ascii_case(&id))
                {
                    return Err("Adapter belongs to another console".into());
                }
                let notice = app.notice.clone();
                app.rt.spawn(async move {
                    let result = backend::unpair(&id).await;
                    *notice.lock().unwrap() = match result {
                        Ok(()) => "Pairing removed".into(),
                        Err(e) => format!("Could not forget pairing: {e}"),
                    };
                });
                Ok(json!({"accepted":true}))
            }
            "video_open" => {
                idle(app)?;
                if app.rec.is_recording() || app.rec.is_finishing() {
                    return Err("Stop recording first".into());
                }
                app.rescan_video();
                let index =
                    crate::video_state::select_video(&app.video_devices, string(&args, "video")?)?;
                if app
                    .unavailable_video
                    .contains(&app.video_devices[index].path)
                {
                    return Err("Capture belongs to another console".into());
                }
                app.switch_video(index);
                if app.capture.lock().unwrap().is_none() {
                    return Err("Capture did not open; see the local console notice".into());
                }
                self.observations.remove(key);
                Ok(json!({"opened":true}))
            }
            "video_close" => {
                idle(app)?;
                if app.rec.is_recording() || app.rec.is_finishing() {
                    return Err("Stop recording first".into());
                }
                let old = app.capture.lock().unwrap().take();
                drop(old);
                app.texture = None;
                self.observations.remove(key);
                Ok(json!({"closed":true}))
            }
            "screen_observe" => Err("Observation must run outside the controller actor".into()),
            "input_release" => {
                if let Some(h) = &app.run_handle {
                    h.abort();
                }
                app.end_capture(ctx);
                if let Some(tx) = app.pump() {
                    tx.send(Input::ReleaseAll)
                        .map_err(|_| "Input transport closed")?;
                }
                self.observations.remove(key);
                Ok(json!({"accepted":true,"release_acknowledged":false}))
            }
            "library_list" => Ok(
                json!({"entries":library::load_all(&app.cfg.script_dir()).iter().take(100).map(|e|json!({"library_id":library_id(e),"workflow":e.workflow,"valid":e.error.is_none(),"definition_digest":digest(e.source.as_bytes()),"approval":self.approvals.get(&(key.into(),library_id(e))).filter(|a|a.text==e.source).map(|a|&a.digest)})).collect::<Vec<_>>()}),
            ),
            "library_read" => {
                let e = entry(app, string(&args, "library_id")?)?;
                if e.source.len() > 65536 {
                    return Err("Definition is too large for one response".into());
                }
                Ok(json!({"source":e.source,"definition_digest":digest(e.source.as_bytes())}))
            }
            "library_select" => {
                idle(app)?;
                let id = string(&args, "library_id")?;
                app.reload_library(None);
                let index = app
                    .library
                    .iter()
                    .position(|e| library_id(e) == id)
                    .ok_or("Unknown library_id")?;
                app.select_script(index);
                Ok(json!({"selected":id,"credentials_cleared":true}))
            }
            "reference_save" => {
                idle(app)?;
                let e = entry(app, string(&args, "library_id")?)?;
                let wf =
                    kvmit_script::Workflow::parse(&e.source).map_err(|_| "Invalid workflow")?;
                let name = string(&args, "reference")?;
                if !crate::wf_view::references(&wf, &base(app, &e))
                    .iter()
                    .any(|r| r.image == name)
                {
                    return Err("Reference is not named by this workflow".into());
                }
                let frame = app
                    .capture
                    .lock()
                    .unwrap()
                    .as_ref()
                    .filter(|c| !c.failed())
                    .and_then(|c| c.latest())
                    .ok_or("No current frame")?;
                crate::wf_view::save_reference(
                    &base(app, &e),
                    name,
                    frame.width,
                    frame.height,
                    &frame.rgba,
                )?;
                self.approvals.remove(&(key.into(), library_id(&e)));
                Ok(json!({"saved":true,"approved":false}))
            }
            "script_import" => {
                idle(app)?;
                let filename = string(&args, "filename")?;
                crate::wf_view::check_file_name(filename)?;
                let (script, report) =
                    kvmit_script::duckyscript::import(string(&args, "source")?, "Imported script");
                let text = script.to_toml();
                crate::wf_view::write_new_workflow(&app.cfg.script_dir(), filename, &text)
                    .map_err(|e| e.to_string())?;
                app.reload_library(None);
                Ok(
                    json!({"written":true,"steps":script.steps.len(),"import_report":format!("{report:?}")}),
                )
            }
            "workflow_recover" => {
                idle(app)?;
                let id = string(&args, "library_id")?;
                let e = entry(app, id)?;
                let approved = self
                    .approvals
                    .get(&(key.into(), id.into()))
                    .ok_or("Local approval required")?;
                if string(&args, "approval")? != approved.digest
                    || approve(app, &e)?.digest != approved.digest
                {
                    return Err("Approval changed".into());
                }
                let adapter = app
                    .adapter_id()
                    .ok_or("Connect the intended adapter first")?;
                let slot = crate::wf_view::Slot::new(saved_source(&e), &adapter);
                let _ = crate::wf_view::recover(&app.cfg.script_dir(), &slot)?;
                Ok(json!({"recovered":true,"effects_replayed":false}))
            }
            "workflow_write" => {
                idle(app)?;
                let filename = string(&args, "filename")?;
                crate::wf_view::check_file_name(filename)?;
                let src = string(&args, "source")?;
                kvmit_script::Workflow::parse(src).map_err(|_| "Invalid workflow".to_string())?;
                let path = app.cfg.script_dir().join(filename);
                if path.exists() {
                    let old = std::fs::read_to_string(&path)
                        .map_err(|_| "Cannot read current workflow")?;
                    if args["expected_digest"].as_str() != Some(digest(old.as_bytes()).as_str()) {
                        return Err("Definition changed; read and review it again".into());
                    }
                    crate::wf_view::rewrite_workflow(&path, &old, src)?;
                } else {
                    crate::wf_view::write_new_workflow(&app.cfg.script_dir(), filename, src)
                        .map_err(|e| e.to_string())?;
                }
                self.approvals.remove(&(key.into(), filename.into()));
                app.reload_library(Some(&path));
                Ok(
                    json!({"written":true,"approved":false,"definition_digest":digest(src.as_bytes())}),
                )
            }
            "workflow_setup" => {
                idle(app)?;
                let e = entry(app, string(&args, "library_id")?)?;
                let path = e
                    .path
                    .as_ref()
                    .ok_or("Copy the built-in workflow before changing its profile")?;
                if string(&args, "expected_digest")? != digest(e.source.as_bytes()) {
                    return Err("Definition changed; read and review it again".into());
                }
                let values = args["values"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().into()))
                    .collect();
                let text = kvmit_script::setup::apply(&e.source, &values)?;
                crate::wf_view::rewrite_workflow(path, &e.source, &text)?;
                self.approvals.remove(&(key.into(), library_id(&e)));
                app.reload_library(Some(path));
                Ok(
                    json!({"written":true,"approved":false,"definition_digest":digest(text.as_bytes())}),
                )
            }
            "workflow_start" | "script_start" | "input_perform" => {
                self.start(app, key, tool, &args, ctx)
            }
            "recording_start" => {
                if app.rec.is_recording() || app.rec.is_finishing() {
                    return Err("Recording is already active or finishing".into());
                }
                let capture = app.capture.clone();
                let frame = capture
                    .lock()
                    .unwrap()
                    .as_ref()
                    .filter(|c| !c.failed())
                    .and_then(|c| c.latest())
                    .ok_or("No live picture")?;
                fresh_frame(&frame)?;
                let format = if args["format"].as_str() == Some("webm") {
                    kvmit_record::Format::WebM
                } else {
                    kvmit_record::Format::Gif
                };
                let audio = args["audio_source"]
                    .as_str()
                    .map(|id| {
                        self.audio_sources
                            .lock()
                            .unwrap()
                            .get(id)
                            .cloned()
                            .ok_or("Audio source changed; list sources again")
                    })
                    .transpose()?;
                if audio.is_some() && format != kvmit_record::Format::WebM {
                    return Err("Audio requires WebM recording".into());
                }
                app.rec.format = format;
                app.rec.audio_sel = None;
                app.rec.start_with_settings(
                    crate::rec_ui::settings_for(format, audio.as_ref()),
                    move || capture.lock().ok()?.as_ref()?.latest(),
                )?;
                self.recordings.remove(key);
                Ok(json!({"recording":true,"audio":audio.is_some()}))
            }
            "recording_stop" => {
                app.rec.begin_stop();
                Ok(json!({"finishing":true}))
            }
            "boot_drive_set" => {
                idle(app)?;
                if app.files.active.load(Ordering::SeqCst) {
                    return Err("Wait for the file transfer to finish before restarting the adapter.".into());
                }
                let dev = app.device().ok_or("No connected adapter")?;
                if !dev.info().supports_boot_drive() {
                    return Err("Firmware does not support a boot drive".into());
                }
                let request = string(&args, "request_id")?;
                let runs = self.runs.as_mut().ok_or("Run storage unavailable")?;
                if let Some(old) = runs.previous(request) {
                    return Ok(json!({"receipt":old}));
                }
                let receipt = runs.begin(request, key, "boot_drive_set")?;
                app.set_boot_drive(dev, flag(&args, "enabled"), ctx.clone());
                runs.update(&receipt.run_id, "submitted")?;
                Ok(json!({"receipt":runs.get(&receipt.run_id),"verified":false}))
            }
            _ => Err("Unknown operation".into()),
        }
    }

    fn begin_audio(
        &mut self,
        reply: std_mpsc::SyncSender<Result<Value, String>>,
    ) -> Result<(), String> {
        if self
            .broker
            .as_ref()
            .is_none_or(|b| b.access != Access::Full)
        {
            return Err("Audio discovery requires Full control".into());
        }
        let sources = self.audio_sources.clone();
        let generation = self.generation.clone();
        let expected = generation.load(Ordering::SeqCst);
        std::thread::Builder::new().name("kvmit-agent-audio".into()).spawn(move||{
            let devices=kvmit_record::list_audio_devices(None);
            let result=(||{
                if generation.load(Ordering::SeqCst)!=expected{return Err("Agent access revoked during audio discovery".into());}
                let prefix=nonce()?;let mut cache=sources.lock().unwrap();cache.clear();let mut listed=Vec::new();
                for(index,device)in devices.into_iter().take(32).enumerate(){let id=format!("{prefix}-{index}");listed.push(json!({"audio_source":id,"description":clipped(&device.description,128),"monitor":device.monitor}));cache.insert(id,device);}
                Ok(json!({"sources":listed,"audio_enabled":false}))
            })();let _=reply.send(result);
        }).map_err(|_|"Could not start audio lookup".to_string())?;
        Ok(())
    }

    fn begin_observe(
        &mut self,
        console: &Console,
        args: Value,
        reply: std_mpsc::SyncSender<Result<Value, String>>,
    ) -> Result<(), String> {
        if self.broker.is_none() {
            return Err("Agent access is off".into());
        }
        let definition = tools::catalog()
            .into_iter()
            .find(|t| t.name == "screen_observe")
            .unwrap();
        tools::validate(&definition.schema, &args)?;
        let key = string(&args, "console")?.to_string();
        let app = &console
            .sessions
            .iter()
            .find(|s| s.key == key)
            .ok_or("Unknown console")?
            .app;
        let (epoch, frame) = {
            let cap = app.capture.lock().unwrap();
            let cap = cap
                .as_ref()
                .filter(|c| !c.failed())
                .ok_or("No live capture")?;
            (cap.epoch, cap.latest().ok_or("Waiting for a frame")?)
        };
        if frame.captured_at.is_some() {
            fresh_frame(&frame)?;
        }
        let token = nonce()?;
        self.observations.insert(
            key.clone(),
            Observation {
                token: token.clone(),
                made: Instant::now(),
                device: app.device(),
                epoch,
                frame: frame.to_gray(),
            },
        );
        let capture = app.capture.clone();
        let generation = self.generation.clone();
        let expected = generation.load(Ordering::SeqCst);
        std::thread::Builder::new()
            .name("kvmit-agent-observe".into())
            .spawn(move || {
                let result =
                    render_observation(&key, &token, &frame, epoch, &args).and_then(|value| {
                        if generation.load(Ordering::SeqCst) != expected {
                            return Err("Agent access revoked during observation".into());
                        }
                        let cap = capture.lock().unwrap();
                        if cap.as_ref().is_none_or(|c| c.failed() || c.epoch != epoch) {
                            return Err("Capture changed during observation".into());
                        }
                        if frame.captured_at.is_some() {
                            fresh_frame(&frame)?;
                        }
                        Ok(value)
                    });
                let _ = reply.send(result);
            })
            .map_err(|_| "Could not start observation worker".to_string())?;
        Ok(())
    }

    fn start(
        &mut self,
        app: &mut App,
        key: &str,
        tool: &str,
        args: &Value,
        ctx: &egui::Context,
    ) -> Result<Value, String> {
        let request = string(args, "request_id")?;
        if let Some(old) = self.runs.as_ref().and_then(|r| r.previous(request)) {
            if old.console != key || old.kind != tool {
                return Err("request_id already belongs to another operation".into());
            }
            return Ok(json!({"receipt":old,"deduplicated":true}));
        }
        idle(app)?;
        if self.live.values().filter(|l| l.handle.running()).count() >= 16 {
            return Err("Too many active agent runs".into());
        }
        let mut approval = None;
        let mut selected = None;
        let mut manual = None;
        if tool == "input_perform" {
            let observation = self
                .observations
                .remove(key)
                .ok_or("Observe the target before sending input")?;
            if observation.token != string(args, "observation")?
                || observation.made.elapsed() > Duration::from_secs(60)
            {
                return Err("Observation expired; observe again".into());
            }
            let dev = app.device().ok_or("Adapter disconnected")?;
            if observation
                .device
                .as_ref()
                .is_none_or(|old| !old.same_session(&dev))
            {
                return Err("Adapter connection changed; observe again".into());
            }
            let cap = app.capture.lock().unwrap();
            let cap = cap
                .as_ref()
                .filter(|c| !c.failed())
                .ok_or("Capture disconnected")?;
            let now = cap.latest().ok_or("No current frame")?;
            fresh_frame(&now)?;
            if cap.epoch != observation.epoch
                || now.to_gray().structure_similarity(&observation.frame) < 0.98
            {
                return Err("Screen/capture changed; observe again".into());
            }
            manual = Some((manual_ops(args)?, observation));
        } else {
            let id = string(args, "library_id")?;
            let e = entry(app, id)?;
            if tool == "workflow_start" {
                let approved = if self
                    .broker
                    .as_ref()
                    .is_some_and(|b| b.access == Access::Full)
                    && args["approval"].as_str().is_none_or(str::is_empty)
                {
                    approve(app, &e)?
                } else {
                    let approved = self
                        .approvals
                        .get(&(key.into(), id.into()))
                        .ok_or("A person must approve this workflow in Agent access first")?;
                    if approved.digest != string(args, "approval")?
                        || approve(app, &e)?.digest != approved.digest
                    {
                        return Err(
                            "Workflow or references changed; local review and approval required"
                                .into(),
                        );
                    }
                    approved.clone()
                };
                if !flag(args, "dry_run")
                    && approved
                        .workflow
                        .secrets
                        .keys()
                        .any(|name| app.wf_secrets.get(name).is_none_or(String::is_empty))
                {
                    return Err("Enter workflow credentials locally in the app first".into());
                }
                if !approved.workflow.secrets.is_empty()
                    && app
                        .selected
                        .and_then(|i| app.library.get(i))
                        .is_none_or(|e| library_id(e) != id)
                {
                    return Err("Select the approved workflow locally before starting it; credentials belong to that selection".into());
                }
                if !app.wf_laptop.is_empty() {
                    crate::wf_view::laptop_name(&app.wf_laptop)?;
                }
                approval = Some(approved);
            } else if e.workflow {
                return Err("Use workflow_start for workflows".into());
            }
            selected = Some(e);
        }
        let receipt = self
            .runs
            .as_mut()
            .ok_or("Run storage unavailable")?
            .begin(request, key, tool)?;
        let started = (|| -> Result<bool, String> {
            Ok(if let Some((ops, observation)) = manual {
                start_manual(app, ops, observation, ctx)?;
                true
            } else {
                let e = selected.unwrap();
                if let Some(approved) = approval {
                    let source = entry_source(&e);
                    let files = WfFiles {
                        base_dir: base(app, &e),
                        script_dir: app.cfg.script_dir(),
                        saved: saved_source(&e),
                        text: approved.text,
                        reference_snapshot: Some(approved.references),
                    };
                    let dry = flag(args, "dry_run");
                    let secrets = if dry {
                        crate::wf_view::dry_run_secrets(&approved.workflow, &app.wf_secrets)
                    } else {
                        app.wf_secrets.clone()
                    };
                    let resume = if flag(args, "resume") {
                        let adapter = app.adapter_id().ok_or("No adapter")?;
                        let slot = crate::wf_view::Slot::new(files.saved.clone(), &adapter);
                        let saved = crate::wf_view::load_checkpoint_raw(
                            &files.script_dir,
                            &slot,
                            &files.text,
                        )?
                        .ok_or("No matching checkpoint")?;
                        if let Some(why) = crate::wf_view::resume_refusal(&saved.0, &app.wf_laptop)
                        {
                            return Err(why);
                        }
                        Some(saved)
                    } else {
                        None
                    };
                    app.wf_allow_missing = false;
                    app.start_workflow(
                        (approved.workflow, source),
                        files,
                        secrets,
                        dry,
                        resume,
                        ctx.clone(),
                    )
                } else {
                    let script = Script::parse(&e.source).map_err(|_| "Invalid script")?;
                    app.start_run(
                        script,
                        base(app, &e),
                        app.current_vars(),
                        flag(args, "dry_run"),
                        ctx.clone(),
                    )
                }
            })
        })();
        let started = match started {
            Ok(started) => started,
            Err(error) => {
                self.runs
                    .as_mut()
                    .unwrap()
                    .update(&receipt.run_id, "refused")?;
                return Err(error);
            }
        };
        if !started {
            self.runs
                .as_mut()
                .unwrap()
                .update(&receipt.run_id, "refused")?;
            return Err("Run did not start; see the local console notice".into());
        }
        let handle = app.run_handle.as_ref().ok_or("Run handle missing")?.clone();
        self.live.insert(
            receipt.run_id.clone(),
            Live {
                handle,
                workflow: if tool == "workflow_start" {
                    app.wf_run.clone()
                } else {
                    None
                },
                summary: Value::Null,
                revision: receipt.cursor,
                persistence_failed: false,
            },
        );
        if let Err(error) = self
            .runs
            .as_mut()
            .unwrap()
            .update(&receipt.run_id, "running")
        {
            self.live[&receipt.run_id].handle.abort();
            return Err(error);
        }
        Ok(
            json!({"receipt":self.runs.as_ref().unwrap().get(&receipt.run_id),"outcome_verified":false,"continuation":"approved workflows continue locally after client disconnect; interactive sequences have a five-second action budget; transport acknowledgements and release cleanup can take longer"}),
        )
    }

    fn start_flash(&mut self, console: &Console, args: &Value) -> Result<Value, String> {
        if console.sessions.iter().any(|s| {
            s.app.device().is_some()
                || s.app.link_task.as_ref().is_some_and(|t| !t.is_finished())
                || s.app.run_handle.as_ref().is_some_and(|h| h.running())
                || s.app.pairing.load(Ordering::SeqCst) > 0
                || s.app.rec.is_recording()
                || s.app.flash.is_some()
        }) {
            return Err(
                "Disconnect all adapters and stop runs, pairing, recording and flash windows first"
                    .into(),
            );
        }
        let request = string(args, "request_id")?;
        let runs = self.runs.as_mut().ok_or("Run storage unavailable")?;
        if let Some(old) = runs.previous(request) {
            return Ok(json!({"receipt":old}));
        }
        let port = kvmit_flash::list_ports()
            .into_iter()
            .find(|p| Some(p.name.as_str()) == args["port"].as_str())
            .ok_or("Unknown UART port")?;
        kvmit_flash::check_port(&port, flag(args, "allow_other_port"))
            .map_err(|e| e.to_string())?;
        kvmit_flash::check_no_native_adapter(
            &kvmit_flash::usb_devices().map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let firmware = args["firmware"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(crate::flashwiz::default_firmware_dir);
        let image =
            kvmit_flash::Image::from_build_dir(Path::new(&firmware)).map_err(|e| e.to_string())?;
        let receipt = runs.begin(request, "administration", "flash_start")?;
        let progress = Arc::new(Mutex::new(
            json!({"running":true,"run_id":receipt.run_id,"stage":"validating"}),
        ));
        let shared = progress.clone();
        let options = kvmit_flash::Options {
            erase_all: flag(args, "erase_all"),
            allow_other_port: flag(args, "allow_other_port"),
            baud: args["baud"].as_u64().map(|n| n as u32),
        };
        std::thread::spawn(move || {
            struct Progress(Arc<Mutex<Value>>);
            impl kvmit_flash::ProgressCallbacks for Progress {
                fn stage(&mut self, s: &kvmit_flash::FlashStage) {
                    self.0.lock().unwrap()["stage"] = json!(s.to_string());
                }
                fn init(&mut self, _: u32, total: usize) {
                    self.0.lock().unwrap()["blocks"] = json!(total);
                }
                fn update(&mut self, n: usize) {
                    self.0.lock().unwrap()["completed_blocks"] = json!(n);
                }
                fn verifying(&mut self) {}
                fn finish(&mut self, _: bool) {}
            }
            let result = kvmit_flash::flash(&port, &image, options, &mut Progress(shared.clone()));
            let mut state = shared.lock().unwrap();
            state["running"] = json!(false);
            state["verified_flash"] = json!(result.is_ok());
            state["running_firmware_verified"] = json!(false);
        });
        self.flash = Some(progress);
        Ok(json!({"receipt":receipt,"accepted":true}))
    }
}

fn render_observation(
    key: &str,
    token: &str,
    frame: &kvmit_video::VideoFrame,
    epoch: u64,
    args: &Value,
) -> Result<Value, String> {
    let mut result = json!({"console":key,"observation":token,"capture_epoch":epoch,"frame_sequence":frame.seq,"frame_age_ms":frame.captured_at.map(|t|t.elapsed().as_millis()),"width":frame.width,"height":frame.height,"synthetic":frame.captured_at.is_none(),"evidence":"untrusted_screen_content","match_scores_are_probabilities":false});
    let full = frame.to_gray();
    let crop = if let Some(values) = args["crop"].as_array() {
        let r: [f64; 4] = std::array::from_fn(|i| values[i].as_f64().unwrap_or(-1.0));
        if r.iter().any(|v| !v.is_finite() || *v < 0.0 || *v > 1.0)
            || r[2] <= 0.0
            || r[3] <= 0.0
            || r[0] + r[2] > 1.0
            || r[1] + r[3] > 1.0
        {
            return Err("Crop must lie inside the normalized frame".into());
        }
        Some(r)
    } else {
        None
    };
    let part = match crop {
        Some([x, y, w, h]) => full.crop(x, y, w, h),
        None => full.clone(),
    };
    if flag(args, "ocr") {
        let lines = kvmit_video::text::read_text(&part)?;
        if frame.captured_at.is_some() {
            fresh_frame(frame)?;
        }
        result["ocr"] = json!(lines
            .iter()
            .take(32)
            .map(|l| json!({"text":clipped(&l.text,128),"rect":l.rect}))
            .collect::<Vec<_>>());
        result["ocr_truncated"] = json!(lines.len() > 32);
    }
    if flag(args, "image") {
        let original =
            image::RgbaImage::from_raw(frame.width as u32, frame.height as u32, frame.rgba.clone())
                .ok_or("Invalid frame")?;
        let mut image = if let Some([x, y, w, h]) = crop {
            let left = (x * frame.width as f64).floor() as u32;
            let top = (y * frame.height as f64).floor() as u32;
            let width = ((w * frame.width as f64).floor() as u32)
                .max(1)
                .min(frame.width as u32 - left);
            let height = ((h * frame.height as f64).floor() as u32)
                .max(1)
                .min(frame.height as u32 - top);
            image::imageops::crop_imm(&original, left, top, width, height).to_image()
        } else {
            original
        };
        let mut max = args["max_width"].as_u64().unwrap_or(960) as u32;
        loop {
            let scale = (max as f64 / image.width().max(image.height()) as f64).min(1.0);
            image = image::imageops::resize(
                &image,
                (image.width() as f64 * scale).max(1.0) as u32,
                (image.height() as f64 * scale).max(1.0) as u32,
                image::imageops::FilterType::Triangle,
            );
            let mut encoded = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(image.clone())
                .write_to(&mut encoded, image::ImageFormat::Png)
                .map_err(|_| "Could not encode picture")?;
            if encoded.get_ref().len() <= 512 * 1024 {
                result["image"] = json!({"type":"image","mimeType":"image/png","data":base64::engine::general_purpose::STANDARD.encode(encoded.into_inner())});
                result["image_width"] = json!(image.width());
                result["image_height"] = json!(image.height());
                break;
            }
            max /= 2;
            if max < 80 {
                return Err("Picture cannot fit the response budget".into());
            }
        }
    }
    Ok(result)
}

fn fresh_frame(frame: &kvmit_video::VideoFrame) -> Result<(), String> {
    if frame
        .captured_at
        .is_none_or(|t| t.elapsed() > Duration::from_secs(3))
    {
        return Err("Frame is stale or synthetic; a live capture is required".into());
    }
    Ok(())
}

fn event_name(event: &kvmit_script::WorkflowEvent) -> Option<&'static str> {
    use kvmit_script::WorkflowEvent::*;
    Some(match event {
        StateEntered { .. } => "state_entered",
        TransitionVerified { .. } => "transition_verified",
        TenantVerified { .. } => "account_screen_checked",
        TypedVerified { .. } => "typed_value_checked",
        GuardChecked { .. } => "guard_checked",
        ManualStepRequested { .. } => "human_action_requested",
        _ => return None,
    })
}

// Input sequences compile completely before effect, are bounded, cancellable, and
// release on every exit. No API leaves a key held between MCP calls.
enum Manual {
    Down(Key),
    Up(Key),
    Move(i32, i32),
    Button(u8, bool),
    Scroll(i8, i8),
    Sleep(Duration),
}
fn manual_ops(args: &Value) -> Result<Vec<Manual>, String> {
    use kvmit_layout::Layout;
    let mut ops = Vec::new();
    let mut pauses = Duration::ZERO;
    for action in args["actions"].as_array().ok_or("Missing actions")? {
        match string(action, "kind")? {
            "text" => {
                let text = string(action, "text")?;
                if text.chars().count() > 128 {
                    return Err("Text bursts are limited to 128 characters".into());
                }
                for c in text.chars() {
                    let stroke = UsAnsi
                        .stroke(c)
                        .ok_or("Text is outside the supported US layout")?;
                    if stroke.shift {
                        ops.push(Manual::Down(Key(0xE1)));
                    }
                    ops.push(Manual::Down(stroke.key));
                    ops.push(Manual::Sleep(Duration::from_millis(8)));
                    ops.push(Manual::Up(stroke.key));
                    if stroke.shift {
                        ops.push(Manual::Up(Key(0xE1)));
                    }
                }
            }
            "key" | "chord" | "hold" => {
                let keys: Vec<Key> = action["keys"]
                    .as_array()
                    .ok_or("keys required")?
                    .iter()
                    .map(|s| {
                        kvmit_hid::parse_key(s.as_str().unwrap_or_default())
                            .ok_or_else(|| "Unknown key".to_string())
                    })
                    .collect::<Result<_, _>>()?;
                if keys.is_empty() {
                    return Err("keys must not be empty".into());
                }
                for k in &keys {
                    ops.push(Manual::Down(*k));
                }
                let duration = Duration::from_millis(action["milliseconds"].as_u64().unwrap_or(8));
                pauses += duration;
                ops.push(Manual::Sleep(duration));
                for k in keys.iter().rev() {
                    ops.push(Manual::Up(*k));
                }
            }
            "move" => ops.push(Manual::Move(
                action["dx"].as_i64().ok_or("dx required")? as i32,
                action["dy"].as_i64().ok_or("dy required")? as i32,
            )),
            "click" => {
                let mask = match string(action, "button")? {
                    "left" => 1,
                    "right" => 2,
                    "middle" => 4,
                    _ => return Err("Unknown button".into()),
                };
                ops.push(Manual::Button(mask, true));
                ops.push(Manual::Button(mask, false));
            }
            "scroll" => ops.push(Manual::Scroll(
                action["vertical"].as_i64().unwrap_or(0) as i8,
                action["horizontal"].as_i64().unwrap_or(0) as i8,
            )),
            _ => return Err("Unknown input kind".into()),
        }
    }
    if ops.len() > 1024 || pauses > Duration::from_secs(3) {
        return Err("Input sequence exceeds its operation/time budget".into());
    }
    Ok(ops)
}

fn start_manual(
    app: &mut App,
    ops: Vec<Manual>,
    observation: Observation,
    ctx: &egui::Context,
) -> Result<(), String> {
    let dev = app.device().ok_or("No connected adapter")?;
    idle(app)?;
    let handle = RunHandle {
        cancel: Arc::new(AtomicBool::new(false)),
        done: Arc::new(AtomicBool::new(false)),
        log: Arc::default(),
        confirm: Arc::default(),
        dry: false,
        outcome: Arc::default(),
    };
    let worker = handle.clone();
    let runtime = app.rt.clone();
    let ctx = ctx.clone();
    let capture = app.capture.clone();
    let link = app.link.clone();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> Result<(), String> {
                // This is after durable intent and worker scheduling, immediately before effects.
                if observation.made.elapsed() > Duration::from_secs(60)
                    || !matches!(&*link.lock().unwrap(),Link::Connected{dev:current,..} if current.same_session(&dev))
                {
                    return Err("Connection or observation changed before input".into());
                }
                let (epoch, current) = {
                    let cap = capture.lock().unwrap();
                    let cap = cap
                        .as_ref()
                        .filter(|c| !c.failed())
                        .ok_or("Capture disconnected before input")?;
                    (cap.epoch, cap.latest().ok_or("No frame before input")?)
                };
                fresh_frame(&current)?;
                if epoch != observation.epoch
                    || current.to_gray().structure_similarity(&observation.frame) < 0.98
                {
                    return Err("Screen changed before input".into());
                }
                for op in ops {
                    if worker.cancel.load(Ordering::SeqCst) || Instant::now() >= deadline {
                        return Err("Input cancelled or deadline reached".into());
                    }
                    match op {
                        Manual::Down(k) => runtime
                            .block_on(dev.key_down(k))
                            .map_err(|_| "Input failed")?,
                        Manual::Up(k) => runtime
                            .block_on(dev.key_up(k))
                            .map_err(|_| "Input failed")?,
                        Manual::Move(x, y) => dev.mouse_move(x, y),
                        Manual::Button(mask, down) => runtime
                            .block_on(dev.button(mask, down))
                            .map_err(|_| "Input failed")?,
                        Manual::Scroll(v, h) => runtime
                            .block_on(dev.scroll(v, h))
                            .map_err(|_| "Input failed")?,
                        Manual::Sleep(d) => {
                            let end = Instant::now() + d;
                            while Instant::now() < end {
                                if worker.cancel.load(Ordering::SeqCst)
                                    || Instant::now() >= deadline
                                {
                                    return Err("Input cancelled".into());
                                }
                                std::thread::sleep(Duration::from_millis(5));
                            }
                        }
                    }
                }
                Ok(())
            },
        ));
        let released = runtime.block_on(dev.release_all()).is_ok();
        if !released {
            dev.close();
        }
        *worker.outcome.lock().unwrap() = Some(if matches!(result, Ok(Ok(()))) && released {
            RunOutcome::Finished
        } else {
            RunOutcome::Aborted("Input cancelled, failed or exceeded its deadline".into())
        });
        worker.done.store(true, Ordering::SeqCst);
        ctx.request_repaint();
    });
    app.run_handle = Some(handle);
    app.wf_run = None;
    Ok(())
}

impl Console {
    pub(super) fn agent_tick(&mut self, ctx: &egui::Context) {
        let mut agent = std::mem::take(&mut self.agent);
        agent.poll_recordings(self);
        agent.poll();
        if let Some(broker) = &agent.broker {
            let commands: Vec<_> = broker.incoming.try_iter().take(4).collect();
            for command in commands {
                if Instant::now() > command.expires {
                    let _ = command.reply.send(Err(
                        "Request expired before execution; no operation started".into(),
                    ));
                    continue;
                }
                if command.tool == "recording_sources" {
                    if command.arguments != json!({}) {
                        let _ = command
                            .reply
                            .send(Err("No audio-discovery arguments are supported".into()));
                        continue;
                    }
                    if let Err(error) = agent.begin_audio(command.reply.clone()) {
                        let _ = command.reply.send(Err(error));
                    }
                    continue;
                }
                if command.tool == "screen_observe" {
                    if let Err(error) =
                        agent.begin_observe(self, command.arguments, command.reply.clone())
                    {
                        let _ = command.reply.send(Err(error));
                    }
                    continue;
                }
                let result = agent.execute(self, &command.tool, command.arguments, ctx);
                let _ = command.reply.send(result);
            }
        }
        if let Some(flash) = &agent.flash {
            let state = flash.lock().unwrap();
            if state["running"] == false {
                if let (Some(runs), Some(id)) = (&mut agent.runs, state["run_id"].as_str()) {
                    let _ = runs.update(
                        id,
                        if state["verified_flash"] == true {
                            "completed"
                        } else {
                            "stopped"
                        },
                    );
                }
            }
        }
        self.agent = agent;
    }

    pub(super) fn agent_ui(&mut self, ui: &mut egui::Ui) {
        let mut agent = std::mem::take(&mut self.agent);
            ui.horizontal_wrapped(|ui| {
                ui.strong("Agent access");
                if agent.broker.is_some() {
                    ui.label(format!(
                        "{:?} · {} actions left",
                        agent.access, agent.remaining
                    ));
                    if ui.button("Stop agent / take control").clicked() {
                        agent.revoke();
                    }
                } else {
                    egui::ComboBox::from_id_salt("agent_mode")
                        .selected_text(format!("{:?}", agent.access))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut agent.access, Access::Observe, "Observe");
                            ui.selectable_value(
                                &mut agent.access,
                                Access::Workflow,
                                "Approved workflows",
                            );
                            ui.selectable_value(&mut agent.access, Access::Full, "Full control");
                        });
                    if ui.button("Enable local agent").clicked() {
                        agent.notice = match agent.enable() {
                            Ok(()) => {
                                "Local access enabled. Connect your client with kvmit mcp.".into()
                            }
                            Err(e) => e,
                        };
                    }
                }
                if ui.button("Approve selected workflow").clicked() {
                    let session = &self.sessions[self.active];
                    agent.notice = (|| {
                        let e = session
                            .app
                            .selected
                            .and_then(|i| session.app.library.get(i))
                            .ok_or("Select a workflow under Workflows in Tools first")?;
                        let approval = approve(&session.app, e)?;
                        let digest = approval.digest.clone();
                        agent
                            .approvals
                            .insert((session.key.clone(), library_id(e)), approval);
                        Ok::<_, String>(format!(
                            "Approved this saved profile and references: {}",
                            &digest[..12]
                        ))
                    })()
                    .unwrap_or_else(|e| e);
                }
            });
            if !agent.notice.is_empty() {
                ui.small(&agent.notice);
            }
        self.agent = agent;
    }
}

/// Headless mode uses exactly the same console command actor. Human gates cannot
/// be approved in workflow mode; workflows requiring local credentials need GUI.
pub fn serve_headless(access: Access, approved: &[PathBuf]) -> Result<(), String> {
    let ctx = egui::Context::default();
    let mut console = Console::from_context(&ctx);
    console.agent.access = access;
    console.agent.enable()?;
    for path in approved {
        let canonical = path.canonicalize().map_err(|e| e.to_string())?;
        let id = canonical
            .file_name()
            .ok_or("Workflow filename required")?
            .to_string_lossy()
            .into_owned();
        for session in &mut console.sessions {
            let e = entry(&session.app, &id)?;
            if e.path.as_ref().and_then(|p| p.canonicalize().ok()).as_ref() != Some(&canonical) {
                return Err("Approved workflows must be in the configured scripts folder".into());
            }
            let approval = approve(&session.app, &e)?;
            if !approval.workflow.secrets.is_empty() {
                return Err("Use the GUI for workflows requiring credentials".into());
            }
            console
                .agent
                .approvals
                .insert((session.key.clone(), id.clone()), approval);
            session.app.reload_library(Some(&canonical));
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    ctrlc::set_handler(move || stopped.store(true, Ordering::SeqCst)).map_err(|e| e.to_string())?;
    eprintln!("kvm-it local controller enabled ({access:?}); Ctrl+C stops it. MCP clients connect with kvmit mcp.");
    while !stop.load(Ordering::SeqCst) {
        console.agent_tick(&ctx);
        std::thread::sleep(Duration::from_millis(25));
    }
    console.agent.revoke();
    if console.agent.flash_busy() {
        eprintln!("Waiting for the firmware write to finish before closing…");
    }
    while console.agent.flash_busy() {
        console.agent_tick(&ctx);
        std::thread::sleep(Duration::from_millis(50));
    }
    for s in &mut console.sessions {
        if let Some(dev) = s.app.device() {
            s.app.rt.block_on(dev.shutdown());
        }
        s.app.stop_transport();
        let _ = s.app.rec.finish_for_exit(Duration::from_secs(20));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recorder_completion_is_collected_for_inactive_consoles_without_a_gui_frame() {
        let (root, agent, mut console, ctx) = fixture(Access::Full);
        let mut app = App::with_config(
            &ctx,
            Config {
                script_dir: Some(root.join("scripts")),
                last_video_key: Some("not-a-real-test-capture".into()),
                ..Default::default()
            },
            Some("two".into()),
        );
        app.rec = crate::rec_ui::RecordUi::finished_fixture();
        console.sessions.push(ConsoleSession {
            key: "two".into(),
            label: "Inactive".into(),
            app,
            memory: None,
        });
        assert_eq!(console.active, 0);
        assert!(console.sessions[1].app.rec.is_finishing());
        console.agent_tick(&ctx);
        assert!(!console.sessions[1].app.rec.is_finishing());
        assert_eq!(console.agent.recordings["two"]["status"], "failed");
        drop((agent, console));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn approval_refuses_unsaved_profile_choices() {
        let (root, agent, mut console, _ctx) = fixture(Access::Workflow);
        let source = library::builtin("windows-11-oobe-provisioning")
            .unwrap()
            .source;
        let values = BTreeMap::from([
            ("deployment_mode".into(), "work_account".into()),
            ("accept_license".into(), "yes".into()),
        ]);
        let source = kvmit_script::setup::apply(source, &values).unwrap();
        let workflow = kvmit_script::Workflow::parse(&source).unwrap();
        let entry = Entry {
            name: "mine.toml".into(),
            path: Some(root.join("scripts/mine.toml")),
            source,
            error: None,
            workflow: true,
        };
        let app = &mut console.sessions[0].app;
        app.library = vec![entry.clone()];
        app.selected = Some(0);
        app.workflow = Some(workflow);
        app.wf_setup.insert("accept_license".into(), "no".into());
        assert!(matches!(approve(app,&entry),Err(message) if message.contains("Save or undo")));
        drop((agent, console));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn bounded_picture_responses_preserve_colour_and_label_synthetic_evidence() {
        let frame = kvmit_video::VideoFrame {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 0, 255, 255],
            seq: 1,
            captured_at: None,
        };
        let response = render_observation(
            "one",
            "opaque",
            &frame,
            1,
            &json!({"image":true,"crop":[0.0,0.0,0.5,1.0]}),
        )
        .unwrap();
        assert_eq!(response["synthetic"], true);
        let encoded = response["image"]["data"].as_str().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert!(bytes.len() <= 512 * 1024);
        let image = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(image.dimensions(), (1, 1));
        assert_eq!(image.get_pixel(0, 0).0, [255, 0, 0, 255]);
    }
    fn fixture(access: Access) -> (PathBuf, Agent, Console, egui::Context) {
        let root = std::env::temp_dir().join(format!("kvmit-mcp-{}", nonce().unwrap()));
        let ctx = egui::Context::default();
        let app = App::with_config(
            &ctx,
            Config {
                script_dir: Some(root.join("scripts")),
                last_video_key: Some("not-a-real-test-capture".into()),
                ..Default::default()
            },
            None,
        );
        let console = Console {
            sessions: vec![ConsoleSession {
                key: "one".into(),
                label: "Test".into(),
                app,
                memory: None,
            }],
            active: 0,
            switching: None,
            agent: Agent::default(),
        };
        let agent = Agent {
            broker: Some(Broker::start(&root.join("ipc"), access).unwrap()),
            runs: Some(Runs::open(&root.join("runs")).unwrap()),
            ..Default::default()
        };
        (root, agent, console, ctx)
    }
    fn handle() -> RunHandle {
        RunHandle {
            cancel: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicBool::new(false)),
            log: Arc::default(),
            confirm: Arc::default(),
            dry: true,
            outcome: Arc::default(),
        }
    }
    fn workflow_entry(root: &Path) -> Entry {
        Entry{name:"fixture.toml".into(),path:Some(root.join("scripts/fixture.toml")),source:"workflow_format=1\nname='fixture'\nmatch='structure'\nstart=['ONE']\n[[states]]\nname='ONE'\ndetect={screen='one.png',text=['Ready']}\nterminal=true\n".into(),error:None,workflow:true}
    }
    #[test]
    fn workflow_mode_denies_effects_before_device_selection() {
        let (root, mut agent, mut console, ctx) = fixture(Access::Workflow);
        let args = json!({"console":"one","request_id":"denied","observation":"unused","actions":[{"kind":"key","keys":["enter"]}]});
        assert!(agent
            .execute(&mut console, "input_perform", args, &ctx)
            .unwrap_err()
            .contains("outside"));
        assert!(agent.runs.as_ref().unwrap().list().is_empty());
        drop((agent, console));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn request_replay_checks_arguments_without_repeating_effects() {
        let (root, mut agent, mut console, ctx) = fixture(Access::Full);
        let args = json!({"console":"one","library_id":"builtin:demo-notepad.toml","request_id":"once","dry_run":true});
        let receipt = agent
            .runs
            .as_mut()
            .unwrap()
            .begin("once", "one", "script_start")
            .unwrap();
        agent
            .fingerprints
            .insert("once".into(), digest(&serde_json::to_vec(&args).unwrap()));
        assert_eq!(
            agent
                .execute(&mut console, "script_start", args.clone(), &ctx)
                .unwrap()["receipt"]["run_id"],
            receipt.run_id
        );
        let mut changed = args.clone();
        changed["dry_run"] = json!(false);
        assert!(agent
            .execute(&mut console, "script_start", changed, &ctx)
            .unwrap_err()
            .contains("different arguments"));
        agent.fingerprints.clear();
        assert!(agent
            .execute(&mut console, "script_start", args, &ctx)
            .unwrap_err()
            .contains("previous controller"));
        assert_eq!(agent.runs.as_ref().unwrap().list().len(), 1);
        drop((agent, console));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn script_questions_have_ids_and_delayed_answers_cannot_approve_new_questions() {
        let (root, mut agent, mut console, ctx) = fixture(Access::Full);
        let receipt = agent
            .runs
            .as_mut()
            .unwrap()
            .begin("question", "one", "script_start")
            .unwrap();
        let handle = handle();
        let (tx, _rx) = std_mpsc::channel();
        *handle.confirm.lock().unwrap() = Some(("first".into(), tx, 10));
        agent.live.insert(
            receipt.run_id.clone(),
            Live {
                handle: handle.clone(),
                workflow: None,
                summary: Value::Null,
                revision: 1,
                persistence_failed: false,
            },
        );
        agent.poll();
        assert_eq!(agent.live[&receipt.run_id].summary["question_id"], 10);
        let (tx, rx) = std_mpsc::channel();
        *handle.confirm.lock().unwrap() = Some(("second".into(), tx, 11));
        assert!(agent
            .execute(
                &mut console,
                "run_answer",
                json!({"run_id":receipt.run_id,"question_id":10,"allow":true}),
                &ctx
            )
            .is_err());
        assert!(rx.try_recv().is_err());
        agent
            .execute(
                &mut console,
                "run_answer",
                json!({"run_id":receipt.run_id,"question_id":11,"allow":true}),
                &ctx,
            )
            .unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap());
        drop((agent, console));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn progress_write_failure_cancels_without_publishing_an_old_cursor_and_retries() {
        let (root, mut agent, console, _ctx) = fixture(Access::Full);
        let receipt = agent
            .runs
            .as_mut()
            .unwrap()
            .begin("progress", "one", "script_start")
            .unwrap();
        let handle = handle();
        agent.live.insert(
            receipt.run_id.clone(),
            Live {
                handle: handle.clone(),
                workflow: None,
                summary: Value::Null,
                revision: 1,
                persistence_failed: false,
            },
        );
        std::fs::rename(root.join("runs"), root.join("runs-away")).unwrap();
        agent.poll();
        assert!(handle.cancel.load(Ordering::SeqCst));
        assert!(agent.live[&receipt.run_id].persistence_failed);
        assert!(agent.live[&receipt.run_id].summary.is_null());
        std::fs::rename(root.join("runs-away"), root.join("runs")).unwrap();
        agent.poll();
        assert!(!agent.live[&receipt.run_id].persistence_failed);
        assert!(agent.live[&receipt.run_id].revision > 1);
        drop((agent, console));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn approval_binds_reference_bytes_and_keeps_an_immutable_snapshot() {
        let (root, _agent, console, _ctx) = fixture(Access::Workflow);
        let entry = workflow_entry(&root);
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        let path = root.join("scripts/one.png");
        image::GrayImage::from_raw(2, 2, vec![0; 4])
            .unwrap()
            .save(&path)
            .unwrap();
        let approved = approve(&console.sessions[0].app, &entry).unwrap();
        image::GrayImage::from_raw(2, 2, vec![255; 4])
            .unwrap()
            .save(&path)
            .unwrap();
        let changed = approve(&console.sessions[0].app, &entry).unwrap();
        assert_ne!(approved.digest, changed.digest);
        assert_eq!(approved.references["one.png"].gray, vec![0; 4]);
        drop((_agent, console));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn prelaunch_failure_finalizes_receipt_instead_of_stranding_starting() {
        let (root, mut agent, mut console, ctx) = fixture(Access::Workflow);
        let entry = workflow_entry(&root);
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::write(entry.path.as_ref().unwrap(), &entry.source).unwrap();
        let approved = approve(&console.sessions[0].app, &entry).unwrap();
        let digest = approved.digest.clone();
        agent
            .approvals
            .insert(("one".into(), "fixture.toml".into()), approved);
        assert!(agent.execute(&mut console,"workflow_start",json!({"console":"one","library_id":"fixture.toml","approval":digest,"request_id":"resume","resume":true,"dry_run":true}),&ctx).is_err());
        assert_eq!(
            agent
                .runs
                .as_ref()
                .unwrap()
                .previous("resume")
                .unwrap()
                .status,
            "refused"
        );
        drop((agent, console));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn console_list_exposes_pairing_progress_and_repairable_failure() {
        let (root, mut agent, mut console, ctx) = fixture(Access::Full);
        let id = "98:C3:77:A0:95:A2";
        let app = &console.sessions[0].app;
        *app.link.lock().unwrap() = Link::Connecting(id.into(), session::ConnectStage::CheckingPairing);
        let state = agent.execute(&mut console, "console_list", json!({}), &ctx).unwrap();
        let row = &state["consoles"][0];
        assert_eq!(row["connected"], false);
        assert!(row["adapter"].is_null());
        assert_eq!(row["connection"], json!({"state":"checking_pairing","adapter":id}));

        *console.sessions[0].app.link.lock().unwrap() = Link::ConnectFailed {
            id: id.into(),
            failure: session::ConnectFailure {
                stage: session::ConnectStage::Connecting,
                message: "Bluetooth handshake failed".into(),
                repair_available: true,
            },
        };
        let state = agent.execute(&mut console, "console_list", json!({}), &ctx).unwrap();
        assert_eq!(state["consoles"][0]["connection"], json!({
            "state":"failed", "adapter":id, "stage":"connecting",
            "error":"Bluetooth handshake failed", "repair_available":true
        }));
        drop((agent, console));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn emergency_release_stops_uncaptured_clipboard_typing_before_the_first_key() {
        let (root, mut agent, fixture_console, ctx) = fixture(Access::Full);
        drop(fixture_console);
        let rt = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let board = super::super::tests::mock_board(&rt);
        let mut console = super::super::tests::mock_console(&ctx, rt.clone(), &[&board]);
        board.hold_release.store(true, Ordering::SeqCst);
        let app = &mut console.sessions[0].app;
        assert!(!app.capturing);
        app.paste_clipboard(Some("still pending".into()));
        board.release_waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        let result = agent.execute(&mut console, "input_release", json!({"console":"console-0"}), &ctx);
        board.allow_release.notify_one();
        result.unwrap();
        let app = &console.sessions[0].app;
        assert!(!app.paste_active.load(Ordering::SeqCst));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !app.input_idle() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(app.input_idle());
        assert!(board.keys_seen.lock().unwrap().iter().all(|(op, _)| *op == kvmit_protocol::msg::RELEASE_ALL));
        board.dev.close();
        drop((agent, console, board, rt));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn emergency_release_is_counted_until_acknowledged_and_blocks_new_input() {
        let (root, mut agent, console, ctx) = fixture(Access::Full);
        drop(console);
        let rt = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let board = super::super::tests::mock_board(&rt);
        let mut console = super::super::tests::mock_console(&ctx, rt.clone(), &[&board]);
        board.hold_release.store(true, Ordering::SeqCst);
        console.sessions[0]
            .app
            .pump()
            .unwrap()
            .send(Input::Down(Key(4)))
            .unwrap();
        agent
            .execute(
                &mut console,
                "input_release",
                json!({"console":"console-0"}),
                &ctx,
            )
            .unwrap();
        board
            .release_waiting
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(!console.sessions[0].app.input_idle());
        assert_eq!(board.held.load(Ordering::SeqCst), 1);
        assert!(agent.execute(&mut console,"input_perform",json!({"console":"console-0","request_id":"blocked","observation":"none","actions":[{"kind":"key","keys":["enter"]}]}),&ctx).unwrap_err().contains("busy"));
        board.allow_release.notify_one();
        let until = Instant::now() + Duration::from_secs(2);
        while !console.sessions[0].app.input_idle() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(console.sessions[0].app.input_idle());
        assert_eq!(board.held.load(Ordering::SeqCst), 0);
        board.dev.close();
        drop((agent, console, board, rt));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn wrong_connection_observations_never_authorize_input() {
        let (root, mut agent, console, ctx) = fixture(Access::Full);
        drop(console);
        let rt = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let first = super::super::tests::mock_board(&rt);
        let second = super::super::tests::mock_board(&rt);
        let mut console = super::super::tests::mock_console(&ctx, rt.clone(), &[&first]);
        agent.observations.insert(
            "console-0".into(),
            Observation {
                token: "wrong".into(),
                made: Instant::now(),
                device: Some(second.dev.clone()),
                epoch: 1,
                frame: kvmit_script::Frame::new(2, 2, vec![0; 4]).unwrap(),
            },
        );
        assert!(agent.execute(&mut console,"input_perform",json!({"console":"console-0","request_id":"wrong-session","observation":"wrong","actions":[{"kind":"key","keys":["enter"]}]}),&ctx).unwrap_err().contains("connection changed"));
        assert_eq!(first.held.load(Ordering::SeqCst), 0);
        assert_eq!(second.held.load(Ordering::SeqCst), 0);
        first.dev.close();
        second.dev.close();
        drop((agent, console, first, second, rt));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn stale_or_synthetic_frames_and_mutations_during_flash_are_refused() {
        let stale = kvmit_video::VideoFrame {
            width: 2,
            height: 2,
            rgba: vec![0; 16],
            seq: 1,
            captured_at: Some(Instant::now() - Duration::from_secs(4)),
        };
        assert!(fresh_frame(&stale).is_err());
        let synthetic = kvmit_video::VideoFrame {
            captured_at: None,
            ..stale
        };
        assert!(fresh_frame(&synthetic).is_err());
        let (root, mut agent, mut console, ctx) = fixture(Access::Full);
        agent.flash = Some(Arc::new(Mutex::new(json!({"running":true}))));
        assert!(agent.flash_busy());
        assert!(agent
            .execute(
                &mut console,
                "adapter_connect",
                json!({"console":"one","adapter":"unused","mode":"connect"}),
                &ctx
            )
            .unwrap_err()
            .contains("Firmware"));
        agent.revoke();
        assert!(agent.broker.is_none());
        assert!(
            agent.flash_busy(),
            "revoking agent access does not abandon a flash"
        );
        drop((agent, console));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn manual_input_is_compiled_before_effect_and_has_budgets() {
        assert!(manual_ops(
            &json!({"actions":[{"kind":"text","text":"hello"},{"kind":"key","keys":["NOT_A_KEY"]}]})
        )
        .is_err());
        assert!(manual_ops(&json!({"actions":[{"kind":"hold","keys":["f12"],"milliseconds":2000},{"kind":"hold","keys":["f12"],"milliseconds":2000}]})).is_err());
        assert!(
            manual_ops(&json!({"actions":[{"kind":"chord","keys":["ctrl","alt","delete"]}]}))
                .is_ok()
        );
    }
}

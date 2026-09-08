//! Consensual Wayland input reception. Only an explicit sharing start may open
//! the portal; status/discovery/packet polling never creates a session.

use std::{
    os::unix::net::UnixStream,
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::Duration,
};

use ashpd::{
    desktop::{
        remote_desktop::{DeviceType, RemoteDesktop},
        screencast::{CursorMode, Screencast, SourceType},
        PersistMode, Session,
    },
    WindowIdentifier,
};
use futures_util::{FutureExt, Stream, StreamExt};
use tokio::{
    runtime::Builder,
    sync::{mpsc, watch},
    time::{timeout, Instant},
};

use crate::{shared_input::InputCommand, NativeStageStatus, Screen};

mod eis;
mod geometry;
mod keys;
mod queue;
#[cfg(test)]
mod test_support;

use queue::CommandReceiver;

const COMMAND_CAPACITY: usize = 256;
const SETUP_TIMEOUT: Duration = Duration::from_secs(10);
const CONSENT_TIMEOUT: Duration = Duration::from_secs(120);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

static SESSION: OnceLock<Mutex<Option<Arc<SessionState>>>> = OnceLock::new();
// ashpd caches its D-Bus connection process-wide. Its Tokio I/O tasks must
// outlive individual sharing sessions, or the next start reuses a dead bus.
static WORKER_QUEUE: OnceLock<Result<mpsc::Sender<WorkerJob>, String>> = OnceLock::new();
const PENDING_SESSIONS: usize = 8;

enum WorkerJob {
    Session {
        state: Arc<SessionState>,
        screens: Vec<Screen>,
        commands: CommandReceiver,
    },
    #[cfg(test)]
    Probe(Box<dyn FnOnce() + Send>),
}

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    Waiting,
    Ready,
    Suspended,
    Failed(String),
    Stopped,
}

#[derive(Debug, Clone, PartialEq)]
enum Work {
    Input {
        connection: u64,
        command: InputCommand,
    },
    Disconnected(u64),
}

struct SessionState {
    phase: Mutex<Phase>,
    commands: mpsc::Sender<Work>,
    cancel: watch::Sender<bool>,
}

impl SessionState {
    fn new(capacity: usize) -> (Arc<Self>, CommandReceiver) {
        let (commands, receiver) = mpsc::channel(capacity);
        let (cancel, _) = watch::channel(false);
        (
            Arc::new(Self {
                phase: Mutex::new(Phase::Waiting),
                commands,
                cancel,
            }),
            CommandReceiver::new(receiver),
        )
    }

    fn status(&self) -> NativeStageStatus {
        let phase = self.phase.lock().unwrap_or_else(|e| e.into_inner());
        let (state, detail) = match &*phase {
            Phase::Waiting => ("idle", "Waiting for Wayland authorization and EIS devices. Allow keyboard/mouse control and select all physical monitors in the desktop sharing dialog.".into()),
            Phase::Ready => ("ready", "Receiving shared input through Wayland RemoteDesktop Portal + EIS.".into()),
            Phase::Suspended => ("idle", "Wayland input is temporarily unavailable. Waiting for paused devices or a replacement keyboard within the existing authorization; no input is buffered. If the desktop does not restore the devices after local unlock, stop and start sharing to authorize again.".into()),
            Phase::Failed(error) => ("error", error.clone()),
            Phase::Stopped => ("idle", "Wayland input sharing is stopped.".into()),
        };
        NativeStageStatus {
            state: state.into(),
            detail,
        }
    }

    fn ready(&self) -> bool {
        *self.phase.lock().unwrap_or_else(|e| e.into_inner()) == Phase::Ready
    }

    fn activate(&self) {
        let mut phase = self.phase.lock().unwrap_or_else(|e| e.into_inner());
        // A late handshake/device event must never revive a stopped, failed,
        // or superseded session.
        if matches!(*phase, Phase::Waiting | Phase::Suspended) {
            log::info!("Wayland input ready (previous phase: {phase:?})");
            *phase = Phase::Ready;
        }
    }

    fn suspended(&self) -> bool {
        *self.phase.lock().unwrap_or_else(|e| e.into_inner()) == Phase::Suspended
    }

    fn suspend(&self, commands: &mut CommandReceiver) -> bool {
        let mut phase = self.phase.lock().unwrap_or_else(|e| e.into_inner());
        if *phase != Phase::Ready {
            return false;
        }
        // submit() uses this same lock. No producer can race a stale key-down
        // into the queue after it is drained, or fill it during suspension.
        *phase = Phase::Suspended;
        let mut discarded = 0;
        while commands.try_recv().is_ok() {
            discarded += 1;
        }
        log::info!("Wayland input suspended; discarded {discarded} pending commands, keeping the existing portal session");
        true
    }

    fn fail(&self, detail: String) {
        let mut phase = self.phase.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*phase, Phase::Waiting | Phase::Ready | Phase::Suspended) {
            log::warn!("Wayland input: {detail}");
            *phase = Phase::Failed(detail);
        }
        self.cancel.send_replace(true);
    }

    fn stop(&self) {
        *self.phase.lock().unwrap_or_else(|e| e.into_inner()) = Phase::Stopped;
        self.cancel.send_replace(true);
    }

    fn submit(&self, work: Work) -> Result<(), String> {
        let mut phase = self.phase.lock().unwrap_or_else(|e| e.into_inner());
        if *phase == Phase::Suspended {
            return Err("Wayland input is temporarily unavailable; pending input is not buffered. Wait for the desktop to resume the devices or replace the keyboard within the existing authorization.".into());
        }
        if *phase != Phase::Ready {
            return Err(
                "Wayland input is not ready; complete authorization or restart input sharing."
                    .into(),
            );
        }
        if let Err(error) = self.commands.try_send(work) {
            // Never silently lose a key-up. Cancel out-of-band so a saturated
            // queue cannot also block cleanup; the worker discards queued work.
            let detail = format!("Wayland input queue unavailable ({error}); input sharing stopped to release held keys/buttons. Stop and start sharing to retry.");
            *phase = Phase::Failed(detail.clone());
            self.cancel.send_replace(true);
            return Err(detail);
        }
        Ok(())
    }
}

fn current() -> Option<Arc<SessionState>> {
    SESSION.get()?.lock().ok()?.clone()
}

pub(super) fn start(screens: Vec<Screen>) {
    let mut slot = SESSION
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if slot.is_some() {
        return;
    }
    let (state, commands) = SessionState::new(COMMAND_CAPACITY);
    *slot = Some(Arc::clone(&state));
    let worker = match worker_queue() {
        Ok(worker) => worker,
        Err(error) => {
            state.fail(error);
            return;
        }
    };
    if let Err(error) = worker.try_send(WorkerJob::Session {
        state: Arc::clone(&state),
        screens,
        commands,
    }) {
        state.fail(format!(
            "Wayland input worker unavailable ({error}). Stop and start sharing to retry."
        ));
    }
}

fn worker_queue() -> Result<&'static mpsc::Sender<WorkerJob>, String> {
    WORKER_QUEUE
        .get_or_init(|| {
            let (sender, mut jobs) = mpsc::channel::<WorkerJob>(PENDING_SESSIONS);
            thread::Builder::new()
                .name("mykvm-wayland-input".into())
                .spawn(move || {
                    // reis' event converter is !Send. Keep it and all portal
                    // sessions on one thread with ashpd's cached bus I/O.
                    let runtime = match Builder::new_current_thread().enable_all().build() {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            jobs.close();
                            while let Ok(job) = jobs.try_recv() {
                                match job {
                                    WorkerJob::Session { state, .. } => {
                                        state.fail(format!(
                                            "Cannot start Wayland input runtime: {error}"
                                        ));
                                    }
                                    #[cfg(test)]
                                    WorkerJob::Probe(_) => {}
                                }
                            }
                            return;
                        }
                    };
                    runtime.block_on(async move {
                        // Finish closing the old session before a rapid
                        // stop/start may open another consent dialog.
                        while let Some(job) = jobs.recv().await {
                            match job {
                                WorkerJob::Session {
                                    state,
                                    screens,
                                    commands,
                                } => {
                                    let result = std::panic::AssertUnwindSafe(run_worker(
                                        &state, screens, commands,
                                    ))
                                    .catch_unwind()
                                    .await;
                                    match result {
                                        Ok(Err(error)) => state.fail(error),
                                        Err(_) => state.fail(
                                            "Wayland input worker stopped unexpectedly. Stop and start sharing to retry."
                                                .into(),
                                        ),
                                        Ok(Ok(())) => {}
                                    }
                                }
                                #[cfg(test)]
                                WorkerJob::Probe(probe) => probe(),
                            }
                        }
                    });
                })
                .map_err(|error| format!("Cannot start Wayland input worker: {error}"))?;
            Ok(sender)
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub(super) fn status() -> NativeStageStatus {
    current()
        .map(|state| state.status())
        .unwrap_or_else(|| NativeStageStatus {
            state: "idle".into(),
            detail:
                "Start input sharing in client mode to authorize Wayland keyboard/mouse control."
                    .into(),
        })
}

pub(super) fn receiving_ready() -> bool {
    current().is_some_and(|state| state.ready())
}

pub(super) fn inject(command: &InputCommand, connection: u64) -> Result<(), String> {
    validate_command(command)?;
    current()
        .ok_or_else(|| "Wayland input sharing has not been started.".to_string())?
        .submit(Work::Input {
            connection,
            command: command.clone(),
        })
}

pub(super) fn disconnected(connection: u64) {
    if let Some(state) = current() {
        if state.ready() {
            let _ = state.submit(Work::Disconnected(connection));
        }
    }
}

pub(super) fn stop() {
    if let Some(slot) = SESSION.get() {
        if let Some(state) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
            // Effective input_ready becomes false synchronously, not after the
            // background session finishes releasing inputs and closing D-Bus.
            state.stop();
        }
    }
}

fn validate_command(command: &InputCommand) -> Result<(), String> {
    match command {
        InputCommand::Key { key_code, .. } if keys::evdev_key(*key_code).is_none() => {
            Err(format!("Unsupported shared key code: {key_code:#06x}"))
        }
        InputCommand::SecureAttention => {
            Err("Secure attention is only supported on Windows.".into())
        }
        _ => Ok(()),
    }
}

async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    let _ = cancel.wait_for(|value| *value).await;
}

async fn run_worker(
    state: &SessionState,
    screens: Vec<Screen>,
    commands: CommandReceiver,
) -> Result<(), String> {
    let mut cancel = state.cancel.subscribe();
    if *cancel.borrow() {
        return Ok(());
    }
    if screens.is_empty()
        || screens
            .iter()
            .any(|s| s.width <= 0 || s.height <= 0 || s.name == "Display unavailable")
    {
        return Err(
            "No usable local monitor geometry. Restart MyKVM inside the logged-in Wayland desktop."
                .into(),
        );
    }
    let remote = tokio::select! {
        biased;
        _ = cancelled(&mut cancel) => return Ok(()),
        result = timeout(SETUP_TIMEOUT, RemoteDesktop::new()) => result
            .map_err(|_| "Timed out connecting to the Wayland RemoteDesktop portal.")?
            .map_err(portal_error)?,
    };
    let version: u32 = tokio::select! {
        biased;
        _ = cancelled(&mut cancel) => return Ok(()),
        result = timeout(SETUP_TIMEOUT, remote.get_property("version")) => result
            .map_err(|_| "Timed out querying the RemoteDesktop portal version.")?
            .map_err(portal_error)?,
    };
    if version < 2 {
        return Err("Wayland input requires a RemoteDesktop portal v2 backend with ConnectToEIS (for example a recent GNOME/KDE desktop). Upgrade the desktop/portal or use an available X11 session.".into());
    }
    if *cancel.borrow() {
        return Ok(());
    }
    // Don't abandon an in-flight CreateSession on stop: if it succeeds we must
    // close the returned handle, even when stop was clicked during creation.
    let session = timeout(SETUP_TIMEOUT, remote.create_session())
        .await
        .map_err(|_| "Timed out creating the Wayland input session.")?
        .map_err(portal_error)?;
    // Keep the known portal handle outside the panic boundary so cleanup also
    // runs if the protocol worker unexpectedly panics.
    let result = std::panic::AssertUnwindSafe(run_session(
        state,
        &remote,
        &session,
        &screens,
        commands,
        &mut cancel,
    ))
    .catch_unwind()
    .await
    .unwrap_or_else(|_| {
        Err("Wayland input worker stopped unexpectedly. Stop and start sharing to retry.".into())
    });
    if let Err(error) = &result {
        state.fail(error.clone());
    }
    match timeout(CLOSE_TIMEOUT, session.close()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => log::debug!("Closing Wayland input session: {error}"),
        Err(error) => log::warn!("Timed out closing Wayland input session: {error}"),
    }
    result
}

fn portal_error(error: impl std::fmt::Display) -> String {
    format!("Wayland RemoteDesktop portal: {error}. Allow keyboard/mouse control and all monitors, then stop and start sharing to retry. A portal backend with EIS support is required.")
}

async fn run_session(
    state: &SessionState,
    remote: &RemoteDesktop<'_>,
    session: &Session<'_, RemoteDesktop<'_>>,
    screens: &[Screen],
    commands: CommandReceiver,
    cancel: &mut watch::Receiver<bool>,
) -> Result<(), String> {
    if *cancel.borrow() {
        return Ok(());
    }
    let closed = tokio::select! {
        biased;
        _ = cancelled(cancel) => return Ok(()),
        result = timeout(SETUP_TIMEOUT, session.receive_closed()) => result
            .map_err(|_| "Timed out subscribing to Wayland session closure.")?
            .map_err(portal_error)?,
    };
    let mut closed = Box::pin(closed);
    let setup = async {
        let devices = DeviceType::Keyboard | DeviceType::Pointer;
        if !remote
            .available_device_types()
            .await
            .map_err(portal_error)?
            .contains(devices)
        {
            return Err(
                "The Wayland portal does not support both keyboard and pointer control.".into(),
            );
        }
        remote
            .select_devices(session, devices, None, PersistMode::DoNot)
            .await
            .map_err(portal_error)?
            .response()
            .map_err(portal_error)?;
        // Request monitor metadata on the same RemoteDesktop session. We never
        // open the PipeWire remote, read video frames, or transmit screen video.
        let screencast = Screencast::new().await.map_err(portal_error)?;
        screencast
            .select_sources(
                session,
                CursorMode::Hidden,
                SourceType::Monitor.into(),
                true,
                None,
                PersistMode::DoNot,
            )
            .await
            .map_err(portal_error)?
            .response()
            .map_err(portal_error)?;
        let selected = remote
            .start(session, &WindowIdentifier::default())
            .await
            .map_err(portal_error)?
            .response()
            .map_err(portal_error)?;
        if !selected.devices().contains(devices) {
            return Err("Wayland keyboard/mouse permission was not granted. Stop and start sharing, and allow both devices.".into());
        }
        let streams = selected
            .streams()
            .unwrap_or_default()
            .iter()
            .map(|stream| geometry::PortalMonitor {
                position: stream.position(),
                size: stream.size(),
                mapping_id: stream.mapping_id().map(str::to_owned),
            })
            .collect::<Vec<_>>();
        let monitors = geometry::match_monitors(screens, &streams)?;
        let fd = remote.connect_to_eis(session).await.map_err(portal_error)?;
        let context = reis::ei::Context::new(UnixStream::from(fd)).map_err(portal_error)?;
        let (connection, events) = timeout(
            SETUP_TIMEOUT,
            context.handshake_tokio("MyKVM", reis::ei::handshake::ContextType::Sender),
        )
        .await
        .map_err(|_| "Timed out connecting to Wayland EIS devices.")?
        .map_err(portal_error)?;
        Ok::<_, String>((connection, events, monitors))
    };
    let (connection, events, monitors) = tokio::select! {
        biased;
        _ = cancelled(cancel) => return Ok(()),
        _ = closed.next() => return Err("Wayland input permission was cancelled or revoked. Stop and start sharing to authorize again.".into()),
        result = timeout(CONSENT_TIMEOUT, setup) => result.map_err(|_| "Wayland authorization timed out. Stop and start sharing to retry.")??,
    };
    run_eis(
        state,
        eis::EisInput::new(connection, monitors),
        events,
        commands,
        &mut closed,
        cancel,
    )
    .await
}

fn suspend_eis_input(state: &SessionState, commands: &mut CommandReceiver) {
    if state.suspend(commands) {
        // The state lock is released before taking the packet-path lock.
        // Forget upstream drag state without closing the authorized session.
        crate::input::pause_received_input();
    }
}

fn refresh_eis_readiness(
    state: &SessionState,
    input: &mut eis::EisInput,
    commands: &mut CommandReceiver,
    setup_deadline: Instant,
) -> Result<(), String> {
    if input.is_paused() {
        suspend_eis_input(state, commands);
    }
    // The event converter responds to EIS ping internally. Flush even without
    // input (also while paused) so those replies reach the compositor.
    input.flush()?;
    if state.suspended() && input.is_paused() {
        // SETUP_TIMEOUT applies to the first device setup, not to a compositor
        // pause or keyboard recreation. Resume/replacement must stay on the
        // existing EIS connection; never reopen a portal or replay old input.
        return Ok(());
    }
    match input.prepare() {
        Ok(()) => state.activate(),
        Err(error) => {
            if state.ready() || state.suspended() || Instant::now() >= setup_deadline {
                return Err(format!(
                    "Wayland EIS devices are not ready: {error} Stop and start sharing to retry."
                ));
            }
        }
    }
    Ok(())
}

async fn run_eis<S: Stream + Unpin>(
    state: &SessionState,
    mut input: eis::EisInput,
    mut events: impl Stream<Item = Result<reis::event::EiEvent, reis::Error>> + Unpin,
    mut commands: CommandReceiver,
    closed: &mut S,
    cancel: &mut watch::Receiver<bool>,
) -> Result<(), String> {
    let deadline = Instant::now() + SETUP_TIMEOUT;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let result = loop {
        tokio::select! {
            biased;
            _ = cancelled(cancel) => break Ok(()),
            _ = closed.next() => {
                // Consent has gone; do not emit further events. The compositor
                // removes the virtual devices, including their held inputs.
                input.invalidated();
                break Err("Wayland input permission was revoked or the desktop session closed. Stop and start sharing to authorize again.".into());
            }
            event = events.next() => {
                match event {
                    Some(Ok(event)) => {
                        match input.event(event) {
                            Ok(eis::EventEffect::ResetInput) => suspend_eis_input(state, &mut commands),
                            Ok(eis::EventEffect::None) => {}
                            Err(error) => break Err(error),
                        }
                        if let Err(error) = refresh_eis_readiness(state, &mut input, &mut commands, deadline) {
                            break Err(error);
                        }
                    }
                    Some(Err(error)) => {
                        input.invalidated();
                        break Err(format!("Wayland EIS connection failed: {error}. Restart input sharing."));
                    }
                    None => {
                        input.invalidated();
                        break Err("Wayland EIS connection closed. Restart input sharing.".into());
                    }
                }
            }
            command = commands.recv(), if state.ready() => {
                let result = match command {
                    Some(Work::Input { connection, command }) => input.inject(connection, &command),
                    Some(Work::Disconnected(connection)) => input.disconnected(connection),
                    None => break Ok(()),
                };
                if let Err(error) = result { break Err(error); }
            }
            _ = tick.tick() => {
                if let Err(error) = refresh_eis_readiness(state, &mut input, &mut commands, deadline) {
                    break Err(error);
                }
            }
        }
    };
    if let Err(error) = &result {
        state.fail(error.clone());
    }
    // Drop sends releases only on still-active devices, stops emulation and
    // closes the EIS socket before run_worker closes the portal session.
    drop(input);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ordinary_motion(x: i32) -> Work {
        Work::Input {
            connection: 1,
            command: InputCommand::MouseMove {
                x,
                y: 10,
                drag_button: None,
            },
        }
    }

    #[tokio::test]
    async fn suspension_discards_lookahead_before_resuming_with_fresh_motion() {
        let (state, mut commands) = SessionState::new(4);
        state.activate();
        state.submit(ordinary_motion(1)).unwrap();
        state.submit(ordinary_motion(2)).unwrap();
        state
            .submit(Work::Input {
                connection: 1,
                command: InputCommand::Key {
                    key_code: 0x41,
                    down: true,
                },
            })
            .unwrap();
        state.submit(ordinary_motion(3)).unwrap();
        // Coalescing has moved the key-down into the single look-ahead slot.
        assert_eq!(commands.recv().await, Some(ordinary_motion(2)));
        assert!(state.suspend(&mut commands));
        assert!(commands.try_recv().is_err());
        assert!(state.submit(ordinary_motion(4)).is_err());
        state.activate();
        state.submit(ordinary_motion(5)).unwrap();
        assert_eq!(commands.recv().await, Some(ordinary_motion(5)));
        assert!(commands.try_recv().is_err());
    }

    #[tokio::test]
    async fn worker_coalesces_a_motion_burst_into_one_eis_frame_before_a_key() {
        use reis::{eis::keyboard::KeyState, request::EisRequest};
        let (mut server, input, events) = test_support::ready().await;
        let (state, commands) = SessionState::new(COMMAND_CAPACITY);
        state.activate();
        for x in 0..64 {
            state.submit(ordinary_motion(x)).unwrap();
        }
        state
            .submit(Work::Input {
                connection: 1,
                command: InputCommand::Key {
                    key_code: 0x41,
                    down: true,
                },
            })
            .unwrap();
        let mut closed = futures_util::stream::pending::<()>();
        let mut cancel = state.cancel.subscribe();
        let mut received = Vec::new();
        let driver = async {
            wait_until(|| {
                received.extend(server.requests());
                received.iter().any(|request| {
                    matches!(request,
                    EisRequest::KeyboardKey(key) if key.key == 30 && key.state == KeyState::Press)
                })
            })
            .await;
            state.stop();
        };
        let (result, ()) = tokio::join!(
            run_eis(&state, input, events, commands, &mut closed, &mut cancel),
            driver,
        );
        result.unwrap();
        let motions: Vec<_> = received
            .iter()
            .filter_map(|request| match request {
                EisRequest::PointerMotionAbsolute(motion) => Some(motion),
                _ => None,
            })
            .collect();
        assert_eq!(motions.len(), 1);
        assert_eq!(motions[0].dx_absolute, 63.0);
        assert_eq!(motions[0].dy_absolute, 10.0);
        let motion_index = received
            .iter()
            .position(|r| matches!(r, EisRequest::PointerMotionAbsolute(_)))
            .unwrap();
        let key_index = received
            .iter()
            .position(|r| matches!(r, EisRequest::KeyboardKey(_)))
            .unwrap();
        assert!(motion_index < key_index);
        assert_eq!(
            received
                .iter()
                .filter(|r| matches!(r, EisRequest::Frame(_)))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn stop_or_portal_revocation_wins_over_a_buffered_lookahead_key() {
        for revoke in [false, true] {
            let (mut server, input, events) = test_support::ready().await;
            let (state, mut commands) = SessionState::new(2);
            state.activate();
            state.submit(ordinary_motion(1)).unwrap();
            state
                .submit(Work::Input {
                    connection: 1,
                    command: InputCommand::Key {
                        key_code: 0x41,
                        down: true,
                    },
                })
                .unwrap();
            assert_eq!(commands.recv().await, Some(ordinary_motion(1)));
            if !revoke {
                state.stop();
            }
            let mut closed = futures_util::stream::iter([()]);
            let mut cancel = state.cancel.subscribe();
            let result = run_eis(&state, input, events, commands, &mut closed, &mut cancel).await;
            assert_eq!(result.is_err(), revoke);
            assert!(!state.ready());
            assert!(!server
                .requests()
                .iter()
                .any(|request| matches!(request, reis::request::EisRequest::KeyboardKey(_))));
        }
    }

    #[test]
    fn worker_io_tasks_outlive_individual_sharing_tasks() {
        // Model ashpd's cached bus reader without contacting D-Bus: a child
        // task created by one sharing task must survive that task's completion.
        let worker = worker_queue().unwrap();
        let (release, wait) = tokio::sync::oneshot::channel();
        let (finished, receive_finished) = std::sync::mpsc::channel();
        let (started, receive_started) = std::sync::mpsc::channel();
        assert!(worker
            .try_send(WorkerJob::Probe(Box::new(move || {
                tokio::spawn(async move {
                    wait.await.unwrap();
                    finished.send(()).unwrap();
                });
                started.send(()).unwrap();
            })))
            .is_ok());
        receive_started
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(worker_queue()
            .unwrap()
            .try_send(WorkerJob::Probe(Box::new(move || {
                release.send(()).unwrap();
            })))
            .is_ok());
        receive_finished
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
    }

    #[test]
    fn readiness_requires_activation_and_polling_has_no_side_effects() {
        let (state, mut commands) = SessionState::new(2);
        assert!(!state.ready());
        assert_eq!(state.status().state, "idle");
        assert!(state.submit(Work::Disconnected(1)).is_err());
        assert!(commands.try_recv().is_err());
        state.activate();
        assert!(state.ready());
        assert_eq!(state.status().state, "ready");
    }

    #[test]
    fn suspension_discards_pending_input_and_never_buffers_or_reauthorizes() {
        let (state, mut commands) = SessionState::new(2);
        assert!(!state.suspend(&mut commands));
        state.activate();
        state
            .submit(Work::Input {
                connection: 1,
                command: InputCommand::Key {
                    key_code: 0x41,
                    down: true,
                },
            })
            .unwrap();
        state.submit(Work::Disconnected(1)).unwrap();
        assert!(state.suspend(&mut commands));
        assert!(!state.ready());
        assert_eq!(state.status().state, "idle");
        assert!(commands.try_recv().is_err());
        for _ in 0..COMMAND_CAPACITY + 1 {
            assert!(state.submit(Work::Disconnected(1)).is_err());
        }
        assert!(state.suspended());
        assert!(!*state.cancel.subscribe().borrow());
        state.activate();
        assert!(state.ready());
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn stop_or_failure_during_suspension_cannot_be_revived() {
        for stop in [false, true] {
            let (state, mut commands) = SessionState::new(1);
            state.activate();
            assert!(state.suspend(&mut commands));
            if stop {
                state.stop();
            } else {
                state.fail("revoked while paused".into());
            }
            state.activate();
            assert!(!state.ready());
            assert!(!state.suspend(&mut commands));
            assert!(*state.cancel.subscribe().borrow());
            assert_eq!(state.status().state, if stop { "idle" } else { "error" });
        }
    }

    #[tokio::test]
    async fn setup_deadline_does_not_expire_an_established_device_recovery() {
        let expired = Instant::now() - SETUP_TIMEOUT;
        for removed in [false, true] {
            let (mut server, mut input, mut events) = test_support::ready().await;
            let (state, mut commands) = SessionState::new(1);
            state.activate();
            if removed {
                server.keyboard.as_ref().unwrap().remove();
            } else {
                server.keyboard.as_ref().unwrap().paused();
            }
            server.flush();
            test_support::event(&mut input, &mut events).await.unwrap();
            refresh_eis_readiness(&state, &mut input, &mut commands, expired).unwrap();
            assert!(state.suspended());
            refresh_eis_readiness(&state, &mut input, &mut commands, expired).unwrap();
            assert!(!state.ready());
            assert!(!*state.cancel.subscribe().borrow());
            if removed {
                server.add_keyboard();
                test_support::event(&mut input, &mut events).await.unwrap(); // added only
                refresh_eis_readiness(&state, &mut input, &mut commands, expired).unwrap();
                assert!(state.suspended());
            } else {
                server.keyboard.as_ref().unwrap().resumed();
                server.flush();
            }
            test_support::event(&mut input, &mut events).await.unwrap();
            refresh_eis_readiness(&state, &mut input, &mut commands, expired).unwrap();
            assert!(state.ready());
        }

        // A removal before the first successful setup must not turn the
        // initial deadline into an indefinite authorized-recovery wait.
        for removed in [false, true] {
            let (mut server, connection, mut events) = test_support::MockEis::connect().await;
            let mut input = test_support::input(connection);
            let (waiting, mut commands) = SessionState::new(1);
            if removed {
                test_support::event(&mut input, &mut events).await.unwrap(); // seat
                server.requests();
                server.add_keyboard();
                test_support::event(&mut input, &mut events).await.unwrap();
                test_support::event(&mut input, &mut events).await.unwrap();
                server.keyboard.as_ref().unwrap().remove();
                server.flush();
                test_support::event(&mut input, &mut events).await.unwrap();
                assert!(input.is_paused());
            }
            assert!(refresh_eis_readiness(
                &waiting,
                &mut input,
                &mut commands,
                Instant::now() + SETUP_TIMEOUT,
            )
            .is_ok());
            assert!(refresh_eis_readiness(&waiting, &mut input, &mut commands, expired).is_err());
            assert!(!waiting.ready());
            assert!(!waiting.suspended());
        }
    }

    async fn wait_until(mut condition: impl FnMut() -> bool) {
        timeout(Duration::from_secs(2), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("mock EIS condition timed out");
    }

    #[tokio::test]
    async fn worker_recovers_from_pause_without_replaying_queued_input() {
        use reis::{eis::keyboard::KeyState, request::EisRequest};
        let (mut server, mut input, mut events) = test_support::ready().await;
        let (state, mut commands) = SessionState::new(2);
        state.activate();
        state
            .submit(Work::Input {
                connection: 1,
                command: InputCommand::Key {
                    key_code: 0x41,
                    down: true,
                },
            })
            .unwrap();
        server.keyboard.as_ref().unwrap().paused();
        server.flush();
        test_support::event(&mut input, &mut events).await.unwrap();
        refresh_eis_readiness(&state, &mut input, &mut commands, Instant::now()).unwrap();
        assert!(state.suspended());
        assert!(commands.try_recv().is_err());
        assert!(server.requests().is_empty());

        let mut closed = futures_util::stream::pending::<()>();
        let mut cancel = state.cancel.subscribe();
        let worker = run_eis(&state, input, events, commands, &mut closed, &mut cancel);
        let driver = async {
            assert!(state
                .submit(Work::Input {
                    connection: 1,
                    command: InputCommand::Key {
                        key_code: 0x41,
                        down: true
                    },
                })
                .is_err());
            server.keyboard.as_ref().unwrap().resumed();
            server.flush();
            wait_until(|| state.ready()).await;
            let resumed = server.requests();
            assert!(resumed
                .iter()
                .any(|r| matches!(r, EisRequest::DeviceStartEmulating(_))));
            assert!(!resumed
                .iter()
                .any(|r| matches!(r, EisRequest::KeyboardKey(_))));
            state
                .submit(Work::Input {
                    connection: 1,
                    command: InputCommand::Key {
                        key_code: 0x42,
                        down: true,
                    },
                })
                .unwrap();
            wait_until(|| {
                let requests = server.requests();
                assert!(!requests.iter().any(|r| matches!(r, EisRequest::KeyboardKey(k) if k.key == 30)));
                requests.iter().any(|r| matches!(r, EisRequest::KeyboardKey(k) if k.key == 48 && k.state == KeyState::Press))
            }).await;
            // Also exercise the worker's actual pause branch, not only the
            // readiness helper used to arrange the initial queued-input case.
            server.keyboard.as_ref().unwrap().paused();
            server.flush();
            wait_until(|| state.suspended()).await;
            assert!(!*state.cancel.subscribe().borrow());
            server.keyboard.as_ref().unwrap().resumed();
            server.flush();
            wait_until(|| state.ready()).await;
            let resumed = server.requests();
            assert!(!resumed
                .iter()
                .any(|r| matches!(r, EisRequest::KeyboardKey(_))));
            state.stop();
        };
        let (result, ()) = timeout(Duration::from_secs(5), async {
            tokio::join!(worker, driver)
        })
        .await
        .unwrap();
        result.unwrap();
        assert!(!state.ready());
    }

    #[tokio::test]
    async fn worker_recovers_from_keyboard_recreation_without_replaying_queued_input() {
        use crate::shared_input::MouseButton;
        use reis::{eis::button::ButtonState, eis::keyboard::KeyState, request::EisRequest};
        for replacement_already_resumed in [false, true] {
            let (mut server, mut input, mut events) = test_support::ready().await;
            let original = server.keyboard.as_ref().unwrap().clone();
            input
                .inject(
                    1,
                    &InputCommand::Key {
                        key_code: 0xA0,
                        down: true,
                    },
                )
                .unwrap();
            input
                .inject(
                    1,
                    &InputCommand::MouseButton {
                        button: MouseButton::Left,
                        down: true,
                        x: 10,
                        y: 20,
                    },
                )
                .unwrap();
            if replacement_already_resumed {
                server.add_keyboard();
                test_support::event(&mut input, &mut events).await.unwrap();
                test_support::event(&mut input, &mut events).await.unwrap();
                input.prepare().unwrap();
            }
            server.requests();

            let (state, mut commands) = SessionState::new(4);
            state.activate();
            state.submit(ordinary_motion(1)).unwrap();
            state
                .submit(Work::Input {
                    connection: 1,
                    command: InputCommand::Key {
                        key_code: 0x41,
                        down: true,
                    },
                })
                .unwrap();
            state
                .submit(Work::Input {
                    connection: 1,
                    command: InputCommand::MouseButton {
                        button: MouseButton::Right,
                        down: true,
                        x: 30,
                        y: 40,
                    },
                })
                .unwrap();
            state.submit(ordinary_motion(2)).unwrap();
            // Put the stale key in CommandReceiver's look-ahead slot. The
            // removal barrier must drain that slot as well as the channel.
            assert_eq!(commands.recv().await, Some(ordinary_motion(1)));
            original.remove();
            server.flush();
            // Prefetch the real protocol event so removal and queued commands
            // are deterministically ready in the same worker select poll.
            let removed = timeout(Duration::from_secs(2), events.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(matches!(&removed, reis::event::EiEvent::DeviceRemoved(_)));
            let events = futures_util::stream::iter([Ok(removed)]).chain(events);
            let mut closed = futures_util::stream::pending::<()>();
            let mut cancel = state.cancel.subscribe();
            let mut cleanup = Vec::new();
            let driver = async {
                wait_until(|| {
                    cleanup.extend(server.requests());
                    cleanup.iter().any(|r| {
                        matches!(r, EisRequest::Button(b)
                        if b.state == ButtonState::Released)
                    })
                })
                .await;
                assert!(!cleanup.iter().any(|r| matches!(
                    r,
                    EisRequest::KeyboardKey(_) | EisRequest::PointerMotionAbsolute(_)
                )));
                assert!(!cleanup.iter().any(|r| matches!(r, EisRequest::Button(b)
                    if b.state == ButtonState::Press)));
                if !replacement_already_resumed {
                    assert!(state.suspended());
                    assert_eq!(state.status().state, "idle");
                    for _ in 0..COMMAND_CAPACITY + 1 {
                        assert!(state.submit(ordinary_motion(3)).is_err());
                    }
                    assert!(!*state.cancel.subscribe().borrow());
                    server.add_keyboard();
                }
                wait_until(|| state.ready()).await;
                assert!(!server.requests().iter().any(|r| matches!(
                    r,
                    EisRequest::KeyboardKey(_)
                        | EisRequest::Button(_)
                        | EisRequest::PointerMotionAbsolute(_)
                )));
                state
                    .submit(Work::Input {
                        connection: 1,
                        command: InputCommand::Key {
                            key_code: 0x42,
                            down: true,
                        },
                    })
                    .unwrap();
                state.submit(ordinary_motion(123)).unwrap();
                let mut fresh = Vec::new();
                wait_until(|| {
                    fresh.extend(server.requests());
                    fresh.iter().any(|r| {
                        matches!(r, EisRequest::KeyboardKey(k)
                        if k.key == 48 && k.state == KeyState::Press)
                    }) && fresh.iter().any(|r| {
                        matches!(r, EisRequest::PointerMotionAbsolute(m)
                            if m.dx_absolute == 123.0)
                    })
                })
                .await;
                assert_eq!(
                    fresh
                        .iter()
                        .filter(|r| matches!(r, EisRequest::KeyboardKey(_)))
                        .count(),
                    1
                );
                assert!(!fresh.iter().any(|r| matches!(r, EisRequest::Button(_))));

                // Exercise a second removal through the live socket, including
                // forgetting the fresh key now held on the replacement.
                server.keyboard.as_ref().unwrap().remove();
                server.flush();
                wait_until(|| state.suspended()).await;
                server.add_keyboard();
                wait_until(|| state.ready()).await;
                assert!(!server
                    .requests()
                    .iter()
                    .any(|r| matches!(r, EisRequest::KeyboardKey(_) | EisRequest::Button(_))));
                state.stop();
            };
            let (result, ()) = timeout(Duration::from_secs(5), async {
                tokio::join!(
                    run_eis(&state, input, events, commands, &mut closed, &mut cancel),
                    driver,
                )
            })
            .await
            .unwrap();
            result.unwrap();
            assert!(!state.ready());
        }
    }

    #[tokio::test]
    async fn stop_revocation_and_pointer_removal_during_keyboard_recovery_are_terminal() {
        for action in ["stop", "revoke", "remove_pointer"] {
            let (mut server, mut input, mut events) = test_support::ready().await;
            let (state, mut commands) = SessionState::new(1);
            state.activate();
            server.keyboard.as_ref().unwrap().remove();
            server.flush();
            test_support::event(&mut input, &mut events).await.unwrap();
            refresh_eis_readiness(&state, &mut input, &mut commands, Instant::now()).unwrap();
            assert!(state.suspended());
            server.requests();
            if action == "remove_pointer" {
                server.pointer.as_ref().unwrap().remove();
            }
            // A replacement queued after the invalidating event cannot revive
            // input; stop/Portal closure have priority over EIS events too.
            server.add_keyboard();
            if action == "stop" {
                state.stop();
            }
            let mut closed = futures_util::stream::iter((action == "revoke").then_some(()))
                .chain(futures_util::stream::pending::<()>());
            let mut cancel = state.cancel.subscribe();
            let result = timeout(
                Duration::from_secs(2),
                run_eis(&state, input, events, commands, &mut closed, &mut cancel),
            )
            .await
            .unwrap();
            assert_eq!(result.is_ok(), action == "stop");
            state.activate();
            assert!(!state.ready());
            assert!(state.submit(ordinary_motion(1)).is_err());
            assert_eq!(
                state.status().state,
                if action == "stop" { "idle" } else { "error" }
            );
            assert!(!server.requests().iter().any(|r| matches!(
                r,
                reis::request::EisRequest::DeviceStartEmulating(_)
                    | reis::request::EisRequest::KeyboardKey(_)
                    | reis::request::EisRequest::PointerMotionAbsolute(_)
            )));
        }
    }

    #[tokio::test]
    async fn eis_socket_closure_during_keyboard_recovery_is_terminal() {
        let (server, mut input, mut events) = test_support::ready().await;
        let (state, mut commands) = SessionState::new(1);
        state.activate();
        server.keyboard.as_ref().unwrap().remove();
        server.flush();
        test_support::event(&mut input, &mut events).await.unwrap();
        refresh_eis_readiness(&state, &mut input, &mut commands, Instant::now()).unwrap();
        assert!(state.suspended());
        drop(server);
        let mut closed = futures_util::stream::pending::<()>();
        let mut cancel = state.cancel.subscribe();
        let error = timeout(
            Duration::from_secs(2),
            run_eis(&state, input, events, commands, &mut closed, &mut cancel),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(error.contains("Wayland EIS connection"));
        state.activate();
        assert!(!state.ready());
        assert_eq!(state.status().state, "error");
        assert!(state.submit(ordinary_motion(1)).is_err());
        assert!(*state.cancel.subscribe().borrow());
    }

    #[tokio::test]
    async fn stop_revocation_and_removal_during_pause_take_priority_over_resumption() {
        for action in ["stop", "revoke", "remove"] {
            let (mut server, mut input, mut events) = test_support::ready().await;
            let (state, mut commands) = SessionState::new(1);
            state.activate();
            server.keyboard.as_ref().unwrap().paused();
            server.pointer.as_ref().unwrap().paused();
            server.flush();
            test_support::event(&mut input, &mut events).await.unwrap();
            test_support::event(&mut input, &mut events).await.unwrap();
            refresh_eis_readiness(&state, &mut input, &mut commands, Instant::now()).unwrap();
            assert!(state.suspended());
            server.requests();
            if action == "remove" {
                server.pointer.as_ref().unwrap().remove();
            } else {
                // This late event must never revive a cancelled session.
                server.keyboard.as_ref().unwrap().resumed();
            }
            server.flush();
            if action == "stop" {
                state.stop();
            }
            let mut closed = futures_util::stream::iter((action == "revoke").then_some(()))
                .chain(futures_util::stream::pending::<()>());
            let mut cancel = state.cancel.subscribe();
            let result = timeout(
                Duration::from_secs(2),
                run_eis(&state, input, events, commands, &mut closed, &mut cancel),
            )
            .await
            .unwrap();
            assert_eq!(result.is_ok(), action == "stop");
            assert!(!state.ready());
            assert_eq!(
                state.status().state,
                if action == "stop" { "idle" } else { "error" }
            );
            assert!(server.requests().is_empty());
        }
    }

    #[test]
    fn queue_overflow_fails_closed_without_dropping_a_key_up_silently() {
        let (state, _receiver) = SessionState::new(1);
        state.activate();
        state
            .submit(Work::Input {
                connection: 1,
                command: InputCommand::Key {
                    key_code: 0x10,
                    down: true,
                },
            })
            .unwrap();
        assert!(state
            .submit(Work::Input {
                connection: 1,
                command: InputCommand::Key {
                    key_code: 0x10,
                    down: false
                }
            })
            .is_err());
        assert!(!state.ready());
        assert_eq!(state.status().state, "error");
        assert!(*state.cancel.subscribe().borrow());
        state.activate();
        assert!(!state.ready());
    }

    #[test]
    fn a_closed_worker_queue_fails_closed() {
        let (state, receiver) = SessionState::new(1);
        state.activate();
        drop(receiver);
        assert!(state.submit(Work::Disconnected(1)).is_err());
        assert!(!state.ready());
        assert_eq!(state.status().state, "error");
    }

    #[tokio::test]
    async fn stop_before_worker_subscribes_is_not_lost_and_generations_are_isolated() {
        let (old, old_receiver) = SessionState::new(1);
        old.stop();
        // A stopped job queued behind an earlier session must return before
        // geometry validation or any attempt to connect to the desktop portal.
        run_worker(&old, Vec::new(), old_receiver).await.unwrap();
        let mut cancel = old.cancel.subscribe();
        timeout(Duration::from_millis(50), cancelled(&mut cancel))
            .await
            .unwrap();
        let (new, _new_receiver) = SessionState::new(1);
        new.activate();
        old.activate();
        old.fail("late error".into());
        assert!(!old.ready());
        assert_eq!(old.status().state, "idle");
        assert!(new.ready());
        assert!(!*new.cancel.subscribe().borrow());
    }

    #[test]
    fn unsupported_commands_are_rejected_before_queueing() {
        assert!(validate_command(&InputCommand::SecureAttention).is_err());
        assert!(validate_command(&InputCommand::Key {
            key_code: 0xffff,
            down: true
        })
        .is_err());
        assert!(validate_command(&InputCommand::Key {
            key_code: 0xA3,
            down: true
        })
        .is_ok());
    }

    #[tokio::test]
    async fn stopping_or_overflow_releases_held_keys_without_draining_stale_commands() {
        use reis::{eis::keyboard::KeyState, request::EisRequest};
        for overflow in [false, true] {
            let (mut server, mut input, events) = test_support::ready().await;
            input
                .inject(
                    1,
                    &InputCommand::Key {
                        key_code: 0x10,
                        down: true,
                    },
                )
                .unwrap();
            server.requests();
            let (state, commands) = SessionState::new(1);
            state.activate();
            state
                .submit(Work::Input {
                    connection: 1,
                    command: InputCommand::Key {
                        key_code: 0x41,
                        down: true,
                    },
                })
                .unwrap();
            if overflow {
                assert!(state.submit(Work::Disconnected(1)).is_err());
            } else {
                state.stop();
            }
            let mut closed = futures_util::stream::pending::<()>();
            let mut cancel = state.cancel.subscribe();
            run_eis(&state, input, events, commands, &mut closed, &mut cancel)
                .await
                .unwrap();
            let requests = server.requests();
            assert!(requests.iter().any(|r| matches!(r, EisRequest::KeyboardKey(k) if k.key == 42 && k.state == KeyState::Released)));
            assert!(!requests
                .iter()
                .any(|r| matches!(r, EisRequest::KeyboardKey(k) if k.key == 30)));
            assert!(!state.ready());
            assert_eq!(
                state.status().state,
                if overflow { "error" } else { "idle" }
            );
        }
    }

    #[tokio::test]
    async fn portal_revocation_invalidates_readiness_without_emitting_more_input() {
        let (mut server, mut input, events) = test_support::ready().await;
        input
            .inject(
                1,
                &InputCommand::Key {
                    key_code: 0x10,
                    down: true,
                },
            )
            .unwrap();
        server.requests();
        let (state, commands) = SessionState::new(1);
        state.activate();
        let mut closed = futures_util::stream::iter([()]);
        let mut cancel = state.cancel.subscribe();
        assert!(
            run_eis(&state, input, events, commands, &mut closed, &mut cancel)
                .await
                .is_err()
        );
        assert!(!state.ready());
        assert_eq!(state.status().state, "error");
        assert!(server.requests().is_empty()); // revocation releases devices in the compositor
    }
}

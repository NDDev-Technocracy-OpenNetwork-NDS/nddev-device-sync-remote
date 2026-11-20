//! Native I/O qualification for the existing public built-in X11 path.
//! NdsClient still owns authentic control responses, purpose credentials and
//! LocalUseFence. Test projection facts never authenticate NDS enrollment.
use super::{
    Result, authorization_error, connection_error, desktop_error, io_error, require, response,
    timeout, unavailable,
};
use nddev_device_sync_domain::screen::{
    FRAME_STALE_MS, MAX_DISPLAYS, RETAINED_FRAMES, ViewLease, ViewProjection, fit_video_height,
};
use nddev_device_sync_domain::{modules::ModuleError, screen::ScreenError};
use rds_core::{
    Codec, DesktopCaps, DesktopControl, DesktopEvent, HelloAck, StreamHello, UniHello, grant::Grant,
};
use rds_desktop::{
    RawFrame, SessionConfig,
    client::{RelayDecoder, RelayOutcome},
};
use rds_net::{Connection, Endpoint, EndpointAddr, SendStream};
use std::{
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{Mutex, Notify, OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
    time::Instant,
};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, instrument::WithSubscriber};

pub(crate) fn wall_now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(unavailable)?
        .as_millis()
        .try_into()
        .map_err(unavailable)
}

fn now() -> Result<u64> {
    wall_now()
}

/// One accepted, current NDS host admission. Constructed by HostAdmission after
/// actual ready/fence/source checks; no independent host listener/registry.
pub struct BuiltinHost {
    endpoint: Endpoint,
    ready: ViewProjection,
    caps: DesktopCaps,
}
impl BuiltinHost {
    pub(crate) fn new(
        endpoint: Endpoint,
        ready: ViewProjection,
        caps: DesktopCaps,
    ) -> Result<Self> {
        require(
            caps.displays.len() <= MAX_DISPLAYS && caps.codecs.contains(&Codec::H264),
            unavailable(()),
        )?;
        let source = caps
            .displays
            .iter()
            .find(|d| d.index == ready.permit().lease.display())
            .ok_or(unavailable(()))?;
        fit_video_height(
            source.width,
            source.height,
            ready.permit().lease.output_height(),
        )
        .map_err(ScreenError::Module)?;
        Ok(Self {
            endpoint,
            ready,
            caps,
        })
    }
    /// NdsClient owns this single task; known authorization/source/lifecycle
    /// loss cancels it, then joins it, before releasing endpoint/key ownership.
    pub async fn serve(self, cancel: CancellationToken) -> Result<()> {
        let life = Life::new(self.ready.permit().lease, cancel.clone())?;
        // Before capture starts, accept/handshake can be cancelled directly.
        // Once serving owns native work, its exact task must complete the drain.
        let accepting = async {
            loop {
                let incoming = self.endpoint.accept().await.ok_or(ScreenError::Closed)?;
                if let Ok(conn) = incoming.await {
                    if conn.remote_id().as_bytes() == &self.ready.permit().viewer_endpoint {
                        // A matching transport key only selects the candidate.
                        // The original signed grant/policy still admits media.
                        return Ok(conn);
                    }
                    conn.close(2u32.into(), b"owned host peer denied");
                }
                // Failed/unrelated pre-admission connections own no capture and
                // cannot consume this role or renew its original finite lease.
                // Yield even under a ready incoming queue so stop/expiry wins.
                tokio::task::yield_now().await;
            }
        };
        let conn = tokio::select! { biased;
            _ = cancel.cancelled() => return Ok(()),
            _ = tokio::time::sleep_until(life.deadline) => { cancel.cancel(); return Ok(()) },
            result = accepting => result?,
        };
        serve_builtin_x11(conn, self.ready, self.caps, cancel).await
    }
}

pub use super::grant::verify_native;

struct Life {
    lease: ViewLease,
    deadline: Instant,
    cancel: CancellationToken,
    stopped: StdMutex<Option<ScreenError>>,
}
impl Life {
    fn new(lease: ViewLease, cancel: CancellationToken) -> Result<Arc<Self>> {
        let remaining = lease.remaining_ms(now()?)?;
        Ok(Arc::new(Self {
            lease,
            deadline: Instant::now() + Duration::from_millis(remaining),
            cancel,
            stopped: StdMutex::new(None),
        }))
    }
    fn wait_error(&self) -> ScreenError {
        if Instant::now() >= self.deadline {
            self.cancel.cancel();
            return ScreenError::Expired;
        }
        match now() {
            Err(error) => error,
            Ok(now) if self.lease.remaining_ms(now).is_err() => {
                self.cancel.cancel();
                ScreenError::Expired
            }
            Ok(_) => timeout(()),
        }
    }
    fn stop(&self, error: ScreenError) {
        let mut stopped = self.stopped.lock().unwrap_or_else(|e| e.into_inner());
        if stopped.is_none() {
            *stopped = Some(error);
        }
        self.cancel.cancel();
    }
    fn check(&self) -> Result<()> {
        if Instant::now() >= self.deadline || self.lease.remaining_ms(now()?).is_err() {
            self.stop(ScreenError::Expired);
            return Err(ScreenError::Expired);
        }
        if let Some(error) = *self.stopped.lock().unwrap_or_else(|e| e.into_inner()) {
            return Err(error);
        }
        require(!self.cancel.is_cancelled(), ScreenError::Closed)?;
        Ok(())
    }
}
struct Close(Connection);
impl Drop for Close {
    fn drop(&mut self) {
        self.0.close(0u32.into(), b"owned view ended");
    }
}

/// One closed Authz -> DesktopV3 session, not an Agent/daemon/service registry.
/// Uses built-in public serve_desktop_with(None) AFTER profile admission. The
/// provider owns all X11 capture/scaling/encoder/control implementation.
/// Caller checks actual host access/current binding/fence before accepting this
/// connection, and cancels this exact token on known source/auth/lifecycle loss.
pub async fn serve_builtin_x11(
    conn: Connection,
    mut ready: ViewProjection,
    caps: DesktopCaps,
    cancel: CancellationToken,
) -> Result<()> {
    let _close = Close(conn.clone());
    let life = Life::new(ready.permit().lease, cancel)?;
    let work = async {
        require(
            caps.displays.len() <= MAX_DISPLAYS && caps.codecs.contains(&Codec::H264),
            unavailable(()),
        )?;
        let (mut send, mut recv) = tokio::time::timeout(Duration::from_secs(5), conn.accept_bi())
            .await
            .map_err(timeout)?
            .map_err(connection_error)?;
        let auth: StreamHello =
            tokio::time::timeout(Duration::from_secs(3), rds_net::read_frame(&mut recv))
                .await
                .map_err(timeout)?
                .map_err(io_error)?;
        let StreamHello::Authz(grant) = auth else {
            return Err(ScreenError::Denied);
        };
        nddev_device_sync_application::screen::native_grant_payload(
            &postcard::to_allocvec(&grant).map_err(response)?,
        )?;
        verify_native(&mut ready, &grant, conn.remote_id().as_bytes(), now()?)?;
        life.check()?;
        // Same native response/FIN transaction consumed by connect_authorized.
        rds_net::write_frame(&mut send, &HelloAck::Ok)
            .await
            .map_err(io_error)?;
        send.finish().map_err(|_| ScreenError::Closed)?;
        drop(recv);
        let (mut send, mut recv) = tokio::time::timeout(Duration::from_secs(5), conn.accept_bi())
            .await
            .map_err(timeout)?
            .map_err(connection_error)?;
        let greeting: StreamHello =
            tokio::time::timeout(Duration::from_secs(3), rds_net::read_frame(&mut recv))
                .await
                .map_err(timeout)?
                .map_err(io_error)?;
        let StreamHello::DesktopV3 {
            session,
            hello,
            output_height,
        } = greeting
        else {
            return Err(ScreenError::Denied);
        };
        let lease = ready.permit().lease;
        require(
            hello.display == lease.display()
                && hello.codec == Codec::H264
                && !hello.input_acks
                && (1..=lease.max_fps()).contains(&hello.max_fps)
                && (16..=lease.output_height()).contains(&output_height),
            ScreenError::Denied,
        )?;
        let display = caps
            .displays
            .iter()
            .find(|d| d.index == hello.display)
            .ok_or(unavailable(()))?;
        let fitted = fit_video_height(display.width, display.height, output_height)
            .map_err(ScreenError::Module)?;
        let extent = (display.width, display.height);
        let caps = DesktopCaps {
            displays: vec![display.clone()],
            codecs: vec![Codec::H264],
        };
        life.check()?;
        rds_net::write_frame(&mut send, &HelloAck::Desktop(caps))
            .await
            .map_err(io_error)?;
        rds_desktop::serve_desktop_with(
            conn.clone(),
            send,
            recv,
            hello,
            SessionConfig {
                source: None,
                producer: None,
                view_only: true,
                native_lifetime: Some(Arc::clone(&life) as rds_desktop::clipboard::NativeLifetime),
                payload_receipts: false,
                output_height: Some(fitted),
                source_extent: Some(extent),
                frame_route: Some(UniHello::DesktopFrames { id: session }),
                ..Default::default()
            },
        )
        .await
        .map_err(desktop_error)?;
        Ok(())
    };
    tokio::pin!(work);
    let result = tokio::select! { biased;
        _ = life.cancel.cancelled() => None,
        _ = tokio::time::sleep_until(life.deadline) => { life.cancel.cancel(); None },
        result = &mut work => Some(result),
    };
    if let Some(result) = result {
        return result;
    }
    // Closing only this admitted connection wakes its control/writer tasks.
    // The checked provider joins its running native capture before returning.
    // The parent retains this task if a bounded close wait is interrupted.
    conn.close(0u32.into(), b"owned host stopping");
    match work.await {
        Ok(()) | Err(ScreenError::Closed) => Ok(()),
        error => error,
    }
}

/// Opaque retained BGRA lease, no serde/debug/cache entry. Bridge caller must
/// also check current Accounts fence and dispose a late Flutter image.
pub struct FrameTicket {
    frame: Option<RawFrame>,
    received: Instant,
    life: Arc<Life>,
    _slot: OwnedSemaphorePermit,
}
impl FrameTicket {
    /// Recheck the same native finite life and frame age after an asynchronous
    /// renderer await. This does not read credentials or authorize a new scope.
    pub fn check_live(&self) -> Result<()> {
        self.check()
    }
    pub fn dimensions(&self) -> Result<(u32, u32, u32)> {
        self.check()?;
        let f = self.frame.as_ref().ok_or(ScreenError::Closed)?;
        Ok((f.width, f.height, f.stride))
    }
    fn check(&self) -> Result<()> {
        self.life.check()?;
        require(
            self.received.elapsed() < Duration::from_millis(FRAME_STALE_MS),
            ScreenError::Expired,
        )?;
        Ok(())
    }
    pub fn read_once(&mut self) -> Result<Vec<u8>> {
        self.check()?;
        Ok(self.frame.take().ok_or(ScreenError::Closed)?.data.to_vec())
    }
}

/// One serial receive/decode owner, independent of presentation pull. The
/// persistent observed-empty anchor bounds NDS-visible queue/read/decode age;
/// it is not a producer capture or network-clock observation.
struct Latest {
    frame: RawFrame,
    received: Instant,
    slot: OwnedSemaphorePermit,
}
struct Frames {
    latest: StdMutex<Option<Latest>>,
    slots: Arc<Semaphore>,
    changed: Notify,
}
impl Frames {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            latest: StdMutex::new(None),
            slots: Arc::new(Semaphore::new(RETAINED_FRAMES)),
            changed: Notify::new(),
        })
    }
    fn publish(&self, frame: RawFrame, received: Instant) {
        let mut latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        // Replacement reuses its permit. Both UI-held tickets do not create
        // another retained buffer: decode continues, unpublished output drops.
        let slot = match latest.take() {
            Some(previous) => previous.slot,
            None => match self.slots.clone().try_acquire_owned() {
                Ok(slot) => slot,
                Err(_) => return,
            },
        };
        *latest = Some(Latest {
            frame,
            received,
            slot,
        });
        self.changed.notify_one();
    }
    fn close(&self) {
        self.latest.lock().unwrap_or_else(|e| e.into_inner()).take();
        self.changed.notify_waiters();
    }
}
fn fresh(anchor: Instant) -> Result<()> {
    require(
        anchor.elapsed() < Duration::from_millis(FRAME_STALE_MS),
        ScreenError::Expired,
    )
}
async fn idr(control: &Mutex<SendStream>) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(2), async {
        rds_net::write_frame(&mut *control.lock().await, &DesktopControl::RequestIdr)
            .await
            .map_err(io_error)
    })
    .await
    .map_err(timeout)?
}
async fn receiver(
    conn: Connection,
    route: [u8; 16],
    life: Arc<Life>,
    frames: Arc<Frames>,
    control: Arc<Mutex<SendStream>>,
    mut empty_at: Instant,
    decode: Arc<Mutex<DecodeWork>>,
) -> Result<()> {
    use std::{
        future::{Future, poll_fn},
        task::Poll,
    };
    let started = Instant::now();
    let mut last_frame = None;
    let mut last_idr = started;
    let mut decoder = Some(RelayDecoder::new());
    loop {
        #[cfg(test)]
        tests::pause_before_accept().await;
        life.check()?;
        let silence = last_frame.map_or(started + Duration::from_secs(5), |at| {
            at + Duration::from_secs(3)
        });
        require(Instant::now() < silence, timeout(()))?;
        // Only an actual Pending poll observes no currently exposed stream.
        // Timeout/dequeue/read completion never assigns a new arrival time.
        // This connection has one receive owner; no UniStreams demux is claimed.
        // Pinned AcceptUni only consumes a stream on Ready; dropping Pending
        // at the idle timeout leaves the connection's queue untouched.
        let accepted = {
            let accepting = conn.accept_uni();
            tokio::pin!(accepting);
            tokio::time::timeout(
                Duration::from_millis(250),
                poll_fn(|cx| match accepting.as_mut().poll(cx) {
                    Poll::Pending => {
                        empty_at = Instant::now();
                        Poll::Pending
                    }
                    Poll::Ready(result) => Poll::Ready(result),
                }),
            )
            .await
        };
        let mut stream = match accepted {
            Err(_) => {
                #[cfg(test)]
                tests::EMPTY_REPOLLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if last_idr.elapsed() >= Duration::from_secs(2) {
                    idr(&control).await?;
                    last_idr = Instant::now();
                }
                continue;
            }
            Ok(result) => result.map_err(connection_error)?,
        };
        let anchor = empty_at;
        fresh(anchor)?;
        let work = async {
            let tag: UniHello = rds_net::read_frame(&mut stream).await.map_err(io_error)?;
            require(tag == UniHello::DesktopFrames { id: route }, response(()))?;
            let (header, payload) = super::read_stream(&mut stream).await?;
            #[cfg(test)]
            tests::qualification_pause(false).await;
            fresh(anchor)?;
            let chain = decoder.take().ok_or(ScreenError::Closed)?;
            #[cfg(test)]
            tests::DECODES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (chain, outcome) = decode.lock().await.decode(chain, header, payload).await?;
            decoder = Some(chain);
            #[cfg(test)]
            tests::qualification_pause(true).await;
            fresh(anchor)?;
            life.check()?;
            match outcome {
                RelayOutcome::Frame(frame) => {
                    last_frame = Some(Instant::now());
                    frames.publish(frame, anchor);
                }
                RelayOutcome::Pending => {}
                RelayOutcome::NeedIdr => {
                    idr(&control).await?;
                    last_idr = Instant::now();
                }
            }
            Ok(())
        };
        tokio::time::timeout_at(
            life.deadline
                .min(anchor + Duration::from_millis(FRAME_STALE_MS)),
            work,
        )
        .await
        .map_err(|_| {
            if Instant::now() >= life.deadline
                || anchor.elapsed() >= Duration::from_millis(FRAME_STALE_MS)
            {
                ScreenError::Expired
            } else {
                timeout(())
            }
        })??;
    }
}

// A receive deadline may drop its local wait, while this owner retains the
// exact decoder task. The provider's original global native budget remains in
// force; no replacement decoder or late output is admitted during cleanup.
#[derive(Default)]
struct DecodeWork {
    task: Option<JoinHandle<Result<(RelayDecoder, RelayOutcome)>>>,
    failure: Option<ScreenError>,
}
impl DecodeWork {
    async fn decode(
        &mut self,
        decoder: RelayDecoder,
        header: rds_core::FrameHeader,
        payload: Vec<u8>,
    ) -> Result<(RelayDecoder, RelayOutcome)> {
        require(self.task.is_none(), ScreenError::Module(ModuleError::Busy))?;
        if let Some(error) = self.failure {
            return Err(error);
        }
        // Store before the first await: caller cancellation cannot detach it.
        self.task = Some(tokio::spawn(
            super::decode_encoded(decoder, header, payload)
                .in_current_span()
                .with_current_subscriber(),
        ));
        self.finish().await?.ok_or(ScreenError::Closed)
    }
    async fn finish(&mut self) -> Result<Option<(RelayDecoder, RelayOutcome)>> {
        let Some(task) = self.task.as_mut() else {
            return self.failure.map_or(Ok(None), Err);
        };
        let result = task.await;
        self.task.take();
        match result {
            Ok(Ok(output)) => Ok(Some(output)),
            result => {
                let error = match result {
                    Ok(Err(error)) => error,
                    _ => ScreenError::Module(ModuleError::ProviderFailed),
                };
                self.failure.get_or_insert(error);
                Err(error)
            }
        }
    }
    async fn close(&mut self) -> Result<()> {
        // A completion is consumed solely for drain; its pixels never reach UI.
        self.finish().await.map(|_| ())
    }
}

pub struct NativeViewer {
    conn: Connection,
    life: Arc<Life>,
    frames: Arc<Frames>,
    tasks: Vec<JoinHandle<()>>,
    drain_error: Option<ScreenError>,
    decode: Arc<Mutex<DecodeWork>>,
    decode_drained: bool,
    #[cfg(test)]
    control: Arc<Mutex<SendStream>>,
}
impl NativeViewer {
    pub async fn open(
        endpoint: &Endpoint,
        target: EndpointAddr,
        mut ready: ViewProjection,
        grant: &Grant,
        cancel: CancellationToken,
    ) -> Result<Self> {
        require(
            target.id.as_bytes() == &ready.permit().host_endpoint,
            ScreenError::Denied,
        )?;
        verify_native(&mut ready, grant, endpoint.id().as_bytes(), now()?)?;
        let life = Life::new(ready.permit().lease, cancel)?;
        let conn = tokio::select! {biased;
            _=life.cancel.cancelled()=>return Err(ScreenError::Closed),
            result=tokio::time::timeout_at(life.deadline.min(Instant::now()+Duration::from_secs(10)),rds_client::connect_authorized(endpoint,target,grant))=>result.map_err(|_|life.wait_error())?.map_err(authorization_error)?,
        };
        let opening = async {
            let (mut send, mut recv) = conn.open_bi().await.map_err(connection_error)?;
            let lease = ready.permit().lease;
            let route = rand::random();
            // This causal boundary is before the new unique route is sent.
            let initial = Instant::now();
            let greeting = StreamHello::DesktopV3 {
                session: route,
                hello: rds_core::DesktopHello {
                    display: lease.display(),
                    max_fps: lease.max_fps(),
                    codec: Codec::H264,
                    input_acks: false,
                },
                output_height: lease.output_height(),
            };
            rds_net::wire::prioritize_control(&send, &greeting).map_err(|_| ScreenError::Closed)?;
            rds_net::write_frame(&mut send, &greeting)
                .await
                .map_err(io_error)?;
            let ack: HelloAck = rds_net::read_frame(&mut recv).await.map_err(io_error)?;
            require(matches!(ack, HelloAck::Desktop(_)), ScreenError::Denied)?;
            life.check()?;
            let control = Arc::new(Mutex::new(send));
            let frames = Frames::new();
            let decode = Arc::new(Mutex::new(DecodeWork::default()));
            let background = control.clone();
            let state = life.clone();
            let closing = conn.clone();
            let output = frames.clone();
            let heartbeat = tokio::spawn(async move {
                let work = async {
                    for seq in 0u64.. {
                        state.check()?;
                        let ts_ms = now()?;
                        tokio::time::timeout(Duration::from_secs(2), async {
                            rds_net::write_frame(
                                &mut *background.lock().await,
                                &DesktopControl::Heartbeat { seq, ts_ms },
                            )
                            .await
                            .map_err(io_error)
                        })
                        .await
                        .map_err(timeout)??;
                        let event: DesktopEvent = tokio::time::timeout(
                            Duration::from_secs(3),
                            rds_net::read_frame(&mut recv),
                        )
                        .await
                        .map_err(timeout)?
                        .map_err(io_error)?;
                        require(
                            matches!(event,DesktopEvent::Heartbeat {seq:s,ts_ms:t} if s==seq&&t==ts_ms),
                            response(()),
                        )?;
                        #[cfg(test)]
                        tests::HEARTBEATS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    Ok::<(), ScreenError>(())
                };
                let result = tokio::select! {biased;
                    _=state.cancel.cancelled()=>Err(ScreenError::Closed),
                    _=tokio::time::sleep_until(state.deadline)=>Err(ScreenError::Expired),
                    result=work=>result,
                };
                state.stop(result.err().unwrap_or(ScreenError::Closed));
                output.close();
                closing.close(0u32.into(), b"owned view stopped");
            }.in_current_span().with_current_subscriber());
            let state = life.clone();
            let closing = conn.clone();
            let output = frames.clone();
            let worker = decode.clone();
            #[cfg(test)]
            let fixture_control = control.clone();
            let intake = tokio::spawn(
                async move {
                    let work = receiver(
                        closing.clone(),
                        route,
                        state.clone(),
                        output.clone(),
                        control,
                        initial,
                        worker,
                    );
                    let result = tokio::select! {biased;
                        _=state.cancel.cancelled()=>Err(ScreenError::Closed),
                        _=tokio::time::sleep_until(state.deadline)=>Err(ScreenError::Expired),
                        result=work=>result,
                    };
                    state.stop(result.err().unwrap_or(ScreenError::Closed));
                    output.close();
                    closing.close(0u32.into(), b"owned receive stopped");
                }
                .in_current_span()
                .with_current_subscriber(),
            );
            Ok(Self {
                conn: conn.clone(),
                life: life.clone(),
                frames,
                drain_error: None,
                tasks: vec![heartbeat, intake],
                decode,
                decode_drained: false,
                #[cfg(test)]
                control: fixture_control,
            })
        };
        let result = tokio::select! {biased;
            _=life.cancel.cancelled()=>Ok(Err(ScreenError::Closed)),
            result=tokio::time::timeout_at(life.deadline.min(Instant::now()+Duration::from_secs(5)),opening)=>result,
        };
        match result {
            Ok(Ok(view)) => Ok(view),
            other => {
                conn.close(0u32.into(), b"owned open failed");
                match other {
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(life.wait_error()),
                    _ => unreachable!(),
                }
            }
        }
    }
    pub async fn pull_frame(&mut self) -> Result<FrameTicket> {
        self.life.check()?;
        require(
            self.frames.slots.available_permits() > 0
                || self
                    .frames
                    .latest
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_some(),
            ScreenError::Module(ModuleError::Busy),
        )?;
        let wait = async {
            loop {
                let changed = self.frames.changed.notified();
                self.life.check()?;
                if let Some(output) = self
                    .frames
                    .latest
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take()
                {
                    fresh(output.received)?;
                    return Ok(FrameTicket {
                        frame: Some(output.frame),
                        received: output.received,
                        life: self.life.clone(),
                        _slot: output.slot,
                    });
                }
                changed.await;
            }
        };
        tokio::select! {biased;
            _=self.life.cancel.cancelled()=>Err(self.life.check().err().unwrap_or(ScreenError::Closed)),
            result=tokio::time::timeout_at(self.life.deadline.min(Instant::now()+Duration::from_secs(5)),wait)=>result.map_err(|_|self.life.wait_error())?,
        }
    }
    pub fn cancel(&self) {
        self.life.stop(ScreenError::Closed);
        self.frames.close();
        self.conn.close(0u32.into(), b"owned cancel");
    }
    /// Drain owned tasks without transferring their handles to the waiting future.
    /// A caller may bound its wait; cancellation leaves unfinished handles here.
    pub async fn close(&mut self) -> Result<()> {
        self.cancel();
        let _ = drain_tasks(&mut self.tasks, &mut self.drain_error).await;
        let result = self.decode.lock().await.close().await;
        self.decode_drained = true;
        if self.drain_error.is_none() {
            self.drain_error = result.err();
        }
        self.drain_error.map_or(Ok(()), Err)
    }
    /// True only after every task handle has produced its terminal result.
    pub fn is_drained(&self) -> bool {
        self.tasks.is_empty() && self.decode_drained
    }
}
async fn drain_tasks(
    tasks: &mut Vec<JoinHandle<()>>,
    failure: &mut Option<ScreenError>,
) -> Result<()> {
    while let Some(task) = tasks.last_mut() {
        let result = task.await;
        tasks.pop();
        if result.is_err() {
            *failure = Some(ScreenError::Module(ModuleError::ProviderFailed));
        }
    }
    failure.map_or(Ok(()), Err)
}
impl Drop for NativeViewer {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn drain_retains_handles_and_errors_across_interrupted_waits() {
        let (release, held) = tokio::sync::oneshot::channel::<()>();
        let blocked = tokio::spawn(async move {
            let _ = held.await;
        });
        let failed = tokio::spawn(std::future::pending::<()>());
        failed.abort();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !failed.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut tasks = vec![blocked, failed];
        let mut failure = None;
        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                drain_tasks(&mut tasks, &mut failure)
            )
            .await
            .is_err()
        );
        assert_eq!(tasks.len(), 1);
        assert!(matches!(
            failure,
            Some(ScreenError::Module(ModuleError::ProviderFailed))
        ));
        release.send(()).unwrap();
        assert!(matches!(
            drain_tasks(&mut tasks, &mut failure).await,
            Err(ScreenError::Module(ModuleError::ProviderFailed))
        ));
        assert!(tasks.is_empty());
        assert!(matches!(
            drain_tasks(&mut tasks, &mut failure).await,
            Err(ScreenError::Module(ModuleError::ProviderFailed))
        ));
    }

    #[tokio::test]
    async fn decoder_drain_keeps_the_exact_job_and_failure_after_interrupted_wait() {
        let (release, held) = tokio::sync::oneshot::channel::<()>();
        let mut decode = DecodeWork {
            task: Some(tokio::spawn(async move {
                let _ = held.await;
                Err(ScreenError::Module(ModuleError::ProviderFailed))
            })),
            failure: None,
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(10), decode.close())
                .await
                .is_err()
        );
        assert!(decode.task.is_some());
        release.send(()).unwrap();
        assert_eq!(
            decode.close().await,
            Err(ScreenError::Module(ModuleError::ProviderFailed))
        );
        assert!(decode.task.is_none());
        assert_eq!(
            decode.close().await,
            Err(ScreenError::Module(ModuleError::ProviderFailed))
        );
    }

    use nddev_device_sync_domain::{
        DeviceId,
        screen::{ViewPermit, ViewState},
    };
    use rds_core::{
        ServiceKind,
        grant::{GRANT_VERSION, GrantConstraints, GrantPayload},
    };
    use rds_net::{EndpointConfig, SecretKey, Transports};
    use std::sync::atomic::{AtomicU64, Ordering};
    use x11rb::{
        connection::Connection as _,
        protocol::xproto::{
            ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, WindowClass,
        },
        rust_connection::RustConnection,
    };

    static BEFORE_ACCEPT: AtomicU64 = AtomicU64::new(0);
    pub(super) static EMPTY_REPOLLS: AtomicU64 = AtomicU64::new(0);
    pub(super) static DECODES: AtomicU64 = AtomicU64::new(0);
    pub(super) static HEARTBEATS: AtomicU64 = AtomicU64::new(0);
    static DELIVERY_ACKS: AtomicU64 = AtomicU64::new(0);
    static BEFORE: AtomicU64 = AtomicU64::new(0);
    pub(super) async fn pause_before_accept() {
        let ms = BEFORE_ACCEPT.swap(0, Ordering::SeqCst);
        if ms > 0 {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
    }
    struct Delivery;
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Delivery {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if event.metadata().target() != "rds_desktop::frame_timing" {
                return;
            }
            struct Fields(bool);
            impl tracing::field::Visit for Fields {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message"
                        && format!("{value:?}") == "desktop frame delivery acknowledged"
                    {
                        self.0 = true;
                    }
                }
            }
            let mut fields = Fields(false);
            event.record(&mut fields);
            if fields.0 {
                DELIVERY_ACKS.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
    static AFTER: AtomicU64 = AtomicU64::new(0);
    pub(super) async fn qualification_pause(decoded: bool) {
        let ms = if decoded {
            AFTER.swap(0, Ordering::SeqCst)
        } else {
            BEFORE.swap(0, Ordering::SeqCst)
        };
        if ms > 0 {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
    }
    async fn endpoint(key: &SecretKey) -> Endpoint {
        rds_net::bind_endpoint(EndpointConfig {
            secret_key: Some(key.clone()),
            discovery: false,
            transports: Transports::DirectOnly,
            bind_addrs: vec!["127.0.0.1:0".parse().unwrap()],
            ..Default::default()
        })
        .await
        .unwrap()
    }
    struct Paint(RustConnection, u32);
    impl Paint {
        fn new() -> Self {
            let (x, _) = RustConnection::connect(None).unwrap();
            let win = x.generate_id().unwrap();
            x.create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                win,
                x.setup().roots[0].root,
                0,
                0,
                640,
                480,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new().background_pixel(0xff0000u32),
            )
            .unwrap()
            .check()
            .unwrap();
            x.map_window(win).unwrap().check().unwrap();
            x.flush().unwrap();
            Self(x, win)
        }
        fn blue(&self) {
            self.0
                .change_window_attributes(
                    self.1,
                    &ChangeWindowAttributesAux::new().background_pixel(0x0000ffu32),
                )
                .unwrap()
                .check()
                .unwrap();
            self.0
                .clear_area(false, self.1, 0, 0, 0, 0)
                .unwrap()
                .check()
                .unwrap();
            self.0.flush().unwrap();
        }
    }
    async fn setup(seconds: u64) -> (Endpoint, Endpoint, ViewProjection, Grant) {
        let host_key = SecretKey::generate();
        let viewer_key = SecretKey::generate();
        let host = endpoint(&host_key).await;
        let viewer = endpoint(&viewer_key).await;
        let admitted = now().unwrap();
        let expiry = (admitted / 1000 + seconds) * 1000;
        // Native subsystem input, not a fabricated authenticated NDS session.
        let projection = ViewProjection::restore(
            "receiver_qualification".into(),
            ViewPermit {
                viewer: DeviceId::new("viewer").unwrap(),
                host: DeviceId::new("host").unwrap(),
                viewer_endpoint: *viewer.id().as_bytes(),
                host_endpoint: *host.id().as_bytes(),
                viewer_revision: 1,
                host_revision: 2,
                lease: ViewLease::restore(0, 30, 480, admitted, expiry).unwrap(),
            },
            ViewState::Ready,
        )
        .unwrap();
        let grant = Grant::issue_at(
            host_key.signing_key(),
            GrantPayload {
                version: GRANT_VERSION,
                revision: 1,
                issuer: *host.id().as_bytes(),
                subject: *viewer.id().as_bytes(),
                audience: *host.id().as_bytes(),
                nonce: rand::random(),
                not_before: admitted / 1000,
                expires_at: expiry / 1000,
                services: vec![ServiceKind::DesktopView],
                constraints: GrantConstraints {
                    displays: Some(vec![0]),
                    ..Default::default()
                },
                tenant: Some(projection.id().into()),
                policy_revision: Some(2),
            },
        );
        (host, viewer, projection, grant)
    }
    async fn view(
        seconds: u64,
    ) -> (
        Endpoint,
        Endpoint,
        NativeViewer,
        CancellationToken,
        JoinHandle<Result<()>>,
    ) {
        let (host, viewer, projection, grant) = setup(seconds).await;
        let caps = rds_desktop::capabilities_bounded().await.unwrap();
        let serving = host.clone();
        let model = projection.clone();
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let task = tokio::spawn(async move {
            BuiltinHost::new(serving, model, caps)
                .unwrap()
                .serve(stop)
                .await
        });
        let live = NativeViewer::open(
            &viewer,
            host.addr(),
            projection,
            &grant,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        (host, viewer, live, cancel, task)
    }
    // owned-x11.py retains this exact Popen child without reaping it during
    // acceptance. This fixture never signals an ambient display or reused PID.
    struct PausedSource {
        pid: String,
        active: bool,
    }
    impl PausedSource {
        fn pause() -> Self {
            assert_eq!(std::env::var("NDS_OWNED_XVFB").as_deref(), Ok("1"));
            let pid = std::env::var("NDS_OWNED_XVFB_PID").unwrap();
            assert!(pid.parse::<u32>().unwrap() > 1);
            assert!(
                std::process::Command::new("kill")
                    .args(["-STOP", "--", &pid])
                    .status()
                    .unwrap()
                    .success()
            );
            Self { pid, active: true }
        }
        fn resume(&mut self) {
            assert!(
                std::process::Command::new("kill")
                    .args(["-CONT", "--", &self.pid])
                    .status()
                    .unwrap()
                    .success()
            );
            self.active = false;
        }
    }
    impl Drop for PausedSource {
        fn drop(&mut self) {
            if self.active {
                let _ = std::process::Command::new("kill")
                    .args(["-CONT", "--", &self.pid])
                    .status();
            }
            // The Python fixture also unconditionally resumes and joins its
            // exact source on failed assertions; no successful drain is inferred.
        }
    }

    async fn reject_foreign_peer(
        host: &Endpoint,
        grant: &Grant,
        task: &mut JoinHandle<Result<()>>,
    ) {
        let foreign = endpoint(&SecretKey::generate()).await;
        // Even possession of the fixture's original viewer grant cannot make
        // a different transport key its subject or consume the admitted role.
        let refused = tokio::time::timeout(
            Duration::from_secs(3),
            rds_client::connect_authorized(&foreign, host.addr(), grant),
        )
        .await
        .expect("foreign connection must be refused within the fixture bound");
        assert!(refused.is_err());
        foreign.close().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(150), task)
                .await
                .is_err(),
            "a foreign connection must not consume the original host role"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires the explicit owned Xvfb host-peer gate"]
    async fn actual_builtin_host_keeps_original_peer_and_lease_after_foreign_connection() {
        assert_eq!(std::env::var("NDS_OWNED_XVFB").as_deref(), Ok("1"));
        let paint = Paint::new();
        let mut outcomes = Vec::new();
        for ending in ["viewer", "cancel", "expiry"] {
            let (host, viewer, projection, grant) =
                setup(if ending == "expiry" { 3 } else { 60 }).await;
            let original_peer = projection.permit().viewer_endpoint;
            let original_expiry = projection.permit().lease.expires_at_ms();
            let caps = rds_desktop::capabilities_bounded().await.unwrap();
            let original_deadline = Instant::now()
                + Duration::from_millis(
                    projection
                        .permit()
                        .lease
                        .remaining_ms(now().unwrap())
                        .unwrap(),
                );
            let cancel = CancellationToken::new();
            let stop = cancel.clone();
            let serving = BuiltinHost::new(host.clone(), projection.clone(), caps).unwrap();
            let mut task = tokio::spawn(serving.serve(stop));
            reject_foreign_peer(&host, &grant, &mut task).await;
            if ending == "viewer" {
                let mut live = NativeViewer::open(
                    &viewer,
                    host.addr(),
                    projection,
                    &grant,
                    CancellationToken::new(),
                )
                .await
                .unwrap();
                assert_eq!(live.conn.remote_id(), host.id());
                let mut frame = live.pull_frame().await.unwrap();
                assert!(frame.read_once().unwrap()[2] > 180);
                drop(frame);
                cancel.cancel();
                live.close().await.unwrap();
                assert!(live.is_drained());
            } else if ending == "cancel" {
                cancel.cancel();
            }
            let drain_deadline = if ending == "expiry" {
                original_deadline + Duration::from_millis(500)
            } else {
                Instant::now() + Duration::from_secs(5)
            };
            tokio::time::timeout_at(drain_deadline, &mut task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if ending == "expiry" {
                assert!(now().unwrap() >= original_expiry);
                assert!(cancel.is_cancelled());
            }
            assert_eq!(viewer.id().as_bytes(), &original_peer);
            host.close().await;
            viewer.close().await;
            outcomes.push(serde_json::json!({
                "after_foreign_rejection": ending,
                "original_peer_and_expiry_preserved": true,
                "host_joined_before_endpoint_close": true,
                "known_native_pixels": ending == "viewer"
            }));
        }
        drop(paint);
        std::fs::write(
            std::env::var("NDS_HOST_PEER_RESULTS").unwrap(),
            serde_json::to_vec_pretty(&serde_json::json!({
                "passed": true,
                "cases": outcomes,
                "scope": "owned Linux Xvfb exact-peer host admission; generated fixture grants, not Accounts-authenticated product acceptance"
            })).unwrap(),
        ).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires the explicit owned Xvfb host-drain gate"]
    async fn actual_builtin_host_retains_capture_until_cancel_or_expiry_drains() {
        let paint = Paint::new();
        let mut outcomes = Vec::new();
        for expiry in [false, true] {
            let (host, viewer, mut live, cancel, mut task) =
                view(if expiry { 3 } else { 60 }).await;
            let mut frame = live.pull_frame().await.unwrap();
            assert!(frame.read_once().unwrap()[2] > 180);
            drop(frame);
            let mut source = PausedSource::pause();
            // Send the existing view-only encoder control over the actual
            // live stream; capture must query the stopped X11 source for IDR.
            idr(&live.control).await.unwrap();
            tokio::time::sleep(Duration::from_millis(150)).await;
            if expiry {
                tokio::time::sleep_until(live.life.deadline + Duration::from_millis(100)).await;
            } else {
                cancel.cancel();
            }
            let started = Instant::now();
            assert!(
                tokio::time::timeout(Duration::from_millis(150), &mut task)
                    .await
                    .is_err(),
                "host cannot acknowledge native drain while its X server is stopped"
            );
            let retained_ms = started.elapsed().as_millis();
            source.resume();
            tokio::time::timeout(Duration::from_secs(5), &mut task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            // The source is still owned and responsive. Endpoint/key cleanup
            // follows the exact capture join, not a process-wide endpoint close.
            live.close().await.unwrap();
            assert!(live.is_drained());
            host.close().await;
            viewer.close().await;
            outcomes.push(serde_json::json!({
                "stop": if expiry { "lease_expiry" } else { "cancellation" },
                "retained_wait_ms": retained_ms,
                "host_joined_after_exact_source_resumed": true,
                "endpoint_close_after_host_join": true,
                "first_frame": "actual X11 known red pixels through native encrypted session and H264 decode"
            }));
        }
        drop(paint);
        std::fs::write(
            std::env::var("NDS_HOST_DRAIN_RESULTS").unwrap(),
            serde_json::to_vec_pretty(&serde_json::json!({
                "passed": true,
                "cases": outcomes,
                "scope": "owned Linux Xvfb native host/receiver drain; no Accounts-authenticated product or ambient desktop",
                "provider": "pinned NDDev Remote Git source"
            })).unwrap(),
        ).unwrap();
    }

    #[derive(Clone, Copy, Debug)]
    enum Fault {
        Tag,
        Header,
        Body,
        Route,
    }
    // Negative wire fixture uses real native keys, grant verification, X11
    // capture and H264 encoding. Only stream fragmentation/hold/route differs.
    // It does not represent an Accounts-authenticated NDS principal.
    async fn fault_view(
        fault: Fault,
    ) -> (
        Endpoint,
        Endpoint,
        NativeViewer,
        CancellationToken,
        JoinHandle<Result<()>>,
    ) {
        use rds_desktop::{Capturer, Encoder};
        let mut capture = rds_desktop::capture::x11::X11Capturer::new(0).unwrap();
        let raw = capture.capture().unwrap();
        let mut encoder = rds_desktop::H264Encoder::new(1_000_000, 30.0).unwrap();
        let encoded = encoder.encode(&raw).unwrap();
        assert!(!encoded.data.is_empty());
        let header = rds_core::FrameHeader {
            seq: 0,
            capture_ts_ms: 0,
            encode_done_ts_ms: 0,
            send_ts_ms: 0,
            keyframe: encoded.keyframe,
            codec: encoded.codec,
            width: raw.width,
            height: raw.height,
        };
        let caps = rds_desktop::capabilities_bounded().await.unwrap();
        let (host, viewer, projection, grant) = setup(60).await;
        let serving = host.clone();
        let mut model = projection.clone();
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let task = tokio::spawn(async move {
            let conn = serving.accept().await.unwrap().await.unwrap();
            let _close = Close(conn.clone());
            let work = async {
                let (mut send, mut recv) = conn.accept_bi().await.map_err(connection_error)?;
                let auth: StreamHello = rds_net::read_frame(&mut recv).await.map_err(io_error)?;
                let StreamHello::Authz(grant) = auth else {
                    return Err(ScreenError::Denied);
                };
                verify_native(&mut model, &grant, conn.remote_id().as_bytes(), now()?)?;
                rds_net::write_frame(&mut send, &HelloAck::Ok)
                    .await
                    .map_err(io_error)?;
                send.finish().map_err(|_| ScreenError::Closed)?;
                drop(recv);
                let (mut send, mut recv) = conn.accept_bi().await.map_err(connection_error)?;
                let greeting: StreamHello =
                    rds_net::read_frame(&mut recv).await.map_err(io_error)?;
                let StreamHello::DesktopV3 { session, .. } = greeting else {
                    return Err(ScreenError::Denied);
                };
                rds_net::write_frame(&mut send, &HelloAck::Desktop(caps))
                    .await
                    .map_err(io_error)?;
                let partial = async {
                    let mut stream = conn.open_uni().await.map_err(connection_error)?;
                    let id = if matches!(fault, Fault::Route) {
                        let mut different = session;
                        different[0] ^= 1;
                        different
                    } else {
                        session
                    };
                    let tag = postcard::to_allocvec(&UniHello::DesktopFrames { id }).unwrap();
                    let mut packet = (tag.len() as u32).to_be_bytes().to_vec();
                    packet.extend_from_slice(&tag);
                    let header_bytes = postcard::to_allocvec(&header).unwrap();
                    let header_start = packet.len();
                    packet.extend_from_slice(&(header_bytes.len() as u32).to_be_bytes());
                    packet.extend_from_slice(&header_bytes);
                    let body_start = packet.len();
                    packet.extend_from_slice(&encoded.data);
                    let split = match fault {
                        Fault::Tag => 2,
                        Fault::Header => header_start + 2,
                        Fault::Body => body_start + 1,
                        Fault::Route => packet.len(),
                    };
                    stream
                        .write_all(&packet[..split])
                        .await
                        .map_err(|_| ScreenError::Closed)?;
                    if !matches!(fault, Fault::Route) {
                        // Raw accept sees an exposed stream immediately, while
                        // tag/header/body is incomplete for more than the age bound.
                        tokio::time::sleep(Duration::from_secs(4)).await;
                        stream
                            .write_all(&packet[split..])
                            .await
                            .map_err(|_| ScreenError::Closed)?;
                    }
                    stream.finish().map_err(|_| ScreenError::Closed)?;
                    Ok::<(), ScreenError>(())
                };
                let heartbeat = async {
                    loop {
                        let control: DesktopControl =
                            rds_net::read_frame(&mut recv).await.map_err(io_error)?;
                        match control {
                            DesktopControl::Heartbeat { seq, ts_ms } => {
                                rds_net::write_frame(
                                    &mut send,
                                    &DesktopEvent::Heartbeat { seq, ts_ms },
                                )
                                .await
                                .map_err(io_error)?;
                            }
                            DesktopControl::RequestIdr => {}
                            _ => return Err(ScreenError::Denied),
                        }
                    }
                };
                let (data, control) = tokio::join!(partial, heartbeat);
                data?;
                control
            };
            tokio::select! {
                _ = stop.cancelled() => Ok(()),
                result = work => result,
            }
        });
        let live = NativeViewer::open(
            &viewer,
            host.addr(),
            projection,
            &grant,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        (host, viewer, live, cancel, task)
    }
    async fn close(
        host: Endpoint,
        viewer: Endpoint,
        mut live: NativeViewer,
        cancel: CancellationToken,
        task: JoinHandle<Result<()>>,
    ) {
        live.close().await.unwrap();
        cancel.cancel();
        host.close().await;
        viewer.close().await;
        let _ = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires the explicit owned Xvfb receiver gate"]
    async fn actual_receiver_age_pause_idle_pressure_and_expiry() {
        assert_eq!(std::env::var("NDS_OWNED_XVFB").as_deref(), Ok("1"));
        use tracing_subscriber::prelude::*;
        tracing::subscriber::set_global_default(tracing_subscriber::registry().with(Delivery))
            .unwrap();
        let paint = Paint::new();
        let mut outcomes = Vec::new();
        let repolls = EMPTY_REPOLLS.load(Ordering::SeqCst);
        let (host, viewer, mut live, cancel, task) = view(60).await;
        assert_eq!(live.conn.uni_routing_stats().routes, 0);
        assert_eq!(live.conn.uni_routing_stats().pending, 0);
        let mut red = live.pull_frame().await.unwrap();
        assert!(red.read_once().unwrap()[2] > 180);
        drop(red);
        paint.blue();
        tokio::time::sleep(Duration::from_millis(4100)).await;
        let mut blue = live.pull_frame().await.unwrap();
        let blue_age = blue.received.elapsed().as_millis();
        assert!(blue_age < 3000);
        let color: [u8; 4] = blue.read_once().unwrap()[..4].try_into().unwrap();
        assert!(color[0] > 180 && color[2] < 70);
        drop(blue);
        // Normal unchanged X11 source remains open through independent IDR
        // refresh; a paused consumer does not stall receive/decode ownership.
        tokio::time::sleep(Duration::from_secs(6)).await;
        let first = live.pull_frame().await.unwrap();
        let second = live.pull_frame().await.unwrap();
        assert_eq!(
            live.pull_frame().await.err().unwrap(),
            ScreenError::Module(ModuleError::Busy)
        );
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(first.check_live().is_err());
        assert!(second.check_live().is_err());
        drop(first);
        drop(second);
        let mut fresh = live.pull_frame().await.unwrap();
        assert!(fresh.check_live().is_ok());
        assert!(fresh.read_once().unwrap()[0] > 180);
        drop(fresh);
        let repolls = EMPTY_REPOLLS.load(Ordering::SeqCst) - repolls;
        assert!(repolls >= 20);
        outcomes.push(serde_json::json!({"cancelled_pending_accept_repolls":repolls,"demux_routes":0,"demux_pending_tasks":0,"consumer_pause_ms":4100,"blue_age_upper_bound_ms":blue_age,"blue_bgra":color,"normal_unchanged_idle_seconds":6,"two_held_tickets_seconds":4,"third_busy":true,"new_output_after_release":true}));
        close(host, viewer, live, cancel, task).await;
        let beats = HEARTBEATS.load(Ordering::SeqCst);
        let decodes = DECODES.load(Ordering::SeqCst);
        let acks = DELIVERY_ACKS.load(Ordering::SeqCst);
        BEFORE_ACCEPT.store(4100, Ordering::SeqCst);
        let (host, viewer, mut live, cancel, task) = view(60).await;
        let error = tokio::time::timeout(Duration::from_secs(6), live.pull_frame())
            .await
            .unwrap()
            .err()
            .unwrap();
        assert_eq!(error, ScreenError::Expired);
        let acks = DELIVERY_ACKS.load(Ordering::SeqCst) - acks;
        let beats = HEARTBEATS.load(Ordering::SeqCst) - beats;
        assert!(
            acks > 0,
            "real native frame delivery ACK during pre-accept pause"
        );
        assert!(beats >= 3, "heartbeat remained live independently");
        assert_eq!(DECODES.load(Ordering::SeqCst), decodes);
        assert!(
            live.frames
                .latest
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none()
        );
        outcomes.push(serde_json::json!({"paused_stage":"before first raw accept/decode","pause_ms":4100,"native_delivery_acks_during_pause":acks,"heartbeat_replies":beats,"actual_decoder_calls":0,"typed_error":"Expired","no_published_output":true,"ready_stream_did_not_reset_anchor":true,"initial_anchor_before_greeting":true}));
        close(host, viewer, live, cancel, task).await;
        for fault in [Fault::Tag, Fault::Header, Fault::Body, Fault::Route] {
            let beats = HEARTBEATS.load(Ordering::SeqCst);
            let decodes = DECODES.load(Ordering::SeqCst);
            let (host, viewer, mut live, cancel, task) = fault_view(fault).await;
            let error = tokio::time::timeout(Duration::from_secs(6), live.pull_frame())
                .await
                .unwrap()
                .err()
                .unwrap();
            let expected = if matches!(fault, Fault::Route) {
                response(())
            } else {
                ScreenError::Expired
            };
            assert_eq!(error, expected);
            let beats = HEARTBEATS.load(Ordering::SeqCst) - beats;
            if !matches!(fault, Fault::Route) {
                assert!(beats >= 2);
            }
            assert_eq!(DECODES.load(Ordering::SeqCst), decodes);
            assert!(
                live.frames
                    .latest
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_none()
            );
            outcomes.push(serde_json::json!({"actual_stream_boundary":format!("{fault:?}"),"hold_ms":if matches!(fault, Fault::Route){0}else{4000},"heartbeat_replies":beats,"typed_error":format!("{error:?}"),"actual_decoder_calls":0,"no_published_output":true,"payload":"actual X11 capture/public native H264Encoder"}));
            close(host, viewer, live, cancel, task).await;
        }
        for decoded in [false, true] {
            if decoded {
                AFTER.store(4000, Ordering::SeqCst);
            } else {
                BEFORE.store(4000, Ordering::SeqCst);
            }
            let (host, viewer, mut live, cancel, task) = view(60).await;
            let error = tokio::time::timeout(Duration::from_secs(6), live.pull_frame())
                .await
                .unwrap()
                .err()
                .unwrap();
            assert_eq!(error, ScreenError::Expired);
            assert!(
                live.frames
                    .latest
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_none()
            );
            outcomes.push(serde_json::json!({"paused_stage":if decoded {"after actual native decode"}else{"before decode after real bounded receive"},"pause_ms":4000,"typed_error":"Expired","no_published_output":true}));
            close(host, viewer, live, cancel, task).await;
        }
        let (host, viewer, mut live, cancel, task) = view(6).await;
        let held = live.pull_frame().await.unwrap();
        tokio::time::sleep(Duration::from_secs(7)).await;
        assert_eq!(held.check_live().err().unwrap(), ScreenError::Expired);
        assert!(live.pull_frame().await.is_err());
        drop(held);
        close(host, viewer, live, cancel, task).await;
        outcomes.push(serde_json::json!({"unpolled_lease_expiry":true,"held_output_denied":true}));
        let path = std::env::var("NDS_RECEIVER_RESULTS").unwrap();
        std::fs::write(path,serde_json::to_vec_pretty(&serde_json::json!({"passed":true,"cases":outcomes,"clock":"conservative local observed-empty accept boundary; no sender/receiver clock subtraction","scope":"real native source/grant/transport/decoder subsystem; no fake authenticated NDS principal","provider_modified_by_fixture":false,"provider":"pinned NDDev Remote Git source","fixture_pause":"cfg(test) delays before raw accept/before actual native decode/after actual native decode; real transport fragmentation at tag/header/body; no fake decoder success","startup_limit":"initial causal anchor before greeting can conservatively shorten nominal five-second startup to three seconds; unchanged idle separately passes"})).unwrap()).unwrap();
    }
}

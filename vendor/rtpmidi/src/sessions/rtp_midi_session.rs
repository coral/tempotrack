use std::collections::HashMap;
use std::ffi::CString;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{Level, event, instrument};
use zerocopy::network_endian::{U32, U64};

use super::host_syncer::HostSyncer;
use super::invite_responder::InviteResponder;
#[cfg(feature = "mdns")]
use super::mdns::advertise_mdns;
use super::rtp_port::RtpPort;
use crate::packets::midi_packets::midi_event::MidiEvent;
use crate::packets::midi_packets::rtp_midi_message::RtpMidiMessage;
use crate::participant::Participant;
use crate::sessions::control_port::{ControlPort, MAX_CONTROL_PACKET_SIZE};
use crate::sessions::events::event_handling::{EventListeners, EventType};
use crate::sessions::midi_port::{MAX_MIDI_PACKET_SIZE, MidiPort};

#[derive(Clone)]
pub struct RtpMidiSession {
    pub(super) participants: Arc<Mutex<HashMap<U32, Participant>>>,              // key by ssrc
    pub(super) pending_invitations: Arc<Mutex<HashMap<U32, PendingInvitation>>>, // key by ssrc
    pub(super) midi_port: Arc<MidiPort>,

    listeners: Arc<Mutex<EventListeners>>,
    control_port: Arc<ControlPort>,
    host_syncer: Arc<HostSyncer>,
    cancel_token: Arc<CancellationToken>,
    task_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
    name: CString,
    #[cfg(feature = "mdns")]
    mdns: mdns_sd::ServiceDaemon,
}

#[derive(Debug, Clone)]
pub(super) struct PendingInvitation {
    pub addr: SocketAddr,
    pub token: U32,
    pub name: CString,
}

impl RtpMidiSession {
    async fn bind(port: u16, name: &str, ssrc: u32) -> std::io::Result<Self> {
        let cstr_name = CString::new(name).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

        let context = RtpMidiSession {
            participants: Arc::new(Mutex::new(HashMap::new())),
            pending_invitations: Arc::new(Mutex::new(HashMap::new())),
            control_port: Arc::new(ControlPort::bind(port, cstr_name.to_owned(), U32::new(ssrc)).await?),
            midi_port: Arc::new(MidiPort::bind(port + 1, cstr_name.to_owned(), U32::new(ssrc)).await?),
            host_syncer: Arc::new(HostSyncer::new()),
            listeners: Arc::new(Mutex::new(EventListeners::new())),
            cancel_token: Arc::new(CancellationToken::new()),
            task_handles: Arc::new(Mutex::new(Vec::new())),
            name: cstr_name,
            #[cfg(feature = "mdns")]
            mdns: advertise_mdns(name, port).map_err(|e| std::io::Error::other(e.to_string()))?,
        };
        Ok(context)
    }

    #[instrument(skip(port),fields(control_port = %port, midi_port = %port + 1))]
    pub async fn start(port: u16, name: &str, ssrc: u32, invite_handler: InviteResponder) -> std::io::Result<Arc<Self>> {
        event!(tracing::Level::INFO, "Starting RTP-MIDI session");
        let ctx = Arc::new(Self::bind(port, name, ssrc).await?);
        ctx.start_threads(invite_handler);
        Ok(ctx)
    }

    fn start_threads(&self, invite_handler: InviteResponder) {
        let mut handles = Vec::new();

        // Control port listener
        let control_port = Arc::clone(&self.control_port);
        let ctx_control = self.clone();
        let control_cancel_token = Arc::clone(&self.cancel_token);

        let handle = tokio::spawn(async move {
            let mut buf = [0u8; MAX_CONTROL_PACKET_SIZE];
            loop {
                tokio::select! {
                    _ = control_cancel_token.cancelled() => {
                        event!(Level::DEBUG, "listen_for_control: cancellation requested");
                        break;
                    },
                    _ = control_port.start(&ctx_control, &invite_handler, &mut buf) => {}
                }
            }
        });
        handles.push(handle);

        // MIDI port listener
        let ctx_midi = self.clone();
        let midi_port_listener = Arc::clone(&self.midi_port);
        let listeners_midi = Arc::clone(&self.listeners);
        let midi_cancel_token = Arc::clone(&self.cancel_token);

        let handle = tokio::spawn(async move {
            let mut buf = [0u8; MAX_MIDI_PACKET_SIZE];
            loop {
                tokio::select! {
                    _ = midi_cancel_token.cancelled() => {
                        event!(Level::DEBUG, "listen_for_midi: cancellation requested");
                        break;
                    },
                    _ = midi_port_listener.start(&ctx_midi, listeners_midi.clone(), &mut buf) => {}
                }
            }
        });
        handles.push(handle);

        // Host clock sync
        let ctx_clock = self.clone();
        let syncer_clock = Arc::clone(&self.host_syncer);
        let syncer_cancel_token = Arc::clone(&self.cancel_token);
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = syncer_cancel_token.cancelled() => {
                        event!(Level::DEBUG, "listen_for_clock_sync: cancellation requested");
                        break;
                    },
                    _ = sleep(Duration::from_secs(10)) => syncer_clock.cleanup(&ctx_clock).await
                }
            }
        });
        handles.push(handle);

        // Store all handles
        let task_handles = self.task_handles.clone();
        tokio::spawn(async move {
            let mut guard = task_handles.lock().await;
            guard.extend(handles);
        });
    }

    #[instrument(skip_all, fields(name = %self.name()))]
    pub fn stop_immediately(&self) {
        event!(Level::INFO, name = self.name(), "Stopping RTP-MIDI session");
        self.cancel_token.cancel();
        #[cfg(feature = "mdns")]
        let _ = self.mdns.shutdown();
    }
    #[instrument(skip_all, fields(name = %self.name()))]
    pub async fn stop_gracefully(&self) {
        self.remove_all_participants().await;
        self.stop_immediately();

        // Wait for all background tasks to complete
        let mut task_handles = self.task_handles.lock().await;
        let handles = std::mem::take(&mut *task_handles);
        drop(task_handles); // Release the lock

        event!(Level::DEBUG, "Waiting for {} background tasks to complete", handles.len());
        for handle in handles {
            if let Err(e) = handle.await {
                event!(Level::WARN, "Task failed to complete cleanly: {}", e);
            }
        }
        event!(Level::INFO, "Graceful shutdown complete");
    }

    #[instrument(skip_all, fields(name = %self.name()))]
    pub async fn remove_all_participants(&self) {
        let participants = self.participants().await;
        for participant in participants {
            self.remove_participant(&participant).await;
        }
    }

    pub async fn invite_participant(&self, addr: SocketAddr) {
        self.control_port.invite_participant(self, addr).await;
    }

    pub async fn participants(&self) -> Vec<Participant> {
        let participants = self.participants.lock().await;
        participants.values().cloned().collect()
    }

    #[instrument(skip_all, fields(participant = %participant.name().to_str().unwrap_or("Unknown")))]
    pub async fn remove_participant(&self, participant: &Participant) {
        event!(Level::INFO, "Removing participant");
        self.control_port.send_termination_packet(participant).await;
        self.midi_port.send_termination_packet(participant).await;
        self.participants.lock().await.remove(&participant.ssrc());
    }

    pub async fn add_listener<E, F>(&self, _event_type: E, callback: F)
    where
        E: EventType,
        F: for<'a> Fn(E::Data<'a>) + Send + 'static,
    {
        let mut listeners = self.listeners.lock().await;
        E::add_listener_to_storage(&mut listeners, callback);
    }

    pub async fn send_midi_batch<'a>(&self, commands: &[MidiEvent<'a>]) -> std::io::Result<()> {
        self.midi_port.send_midi_batch(self, commands).await
    }

    pub async fn send_midi<'a>(&self, command: &RtpMidiMessage<'a>) -> std::io::Result<()> {
        self.midi_port.send_midi(self, command).await
    }

    pub fn name(&self) -> &str {
        self.name.to_str().unwrap_or("Unnamed Session")
    }
}

pub fn current_timestamp(start_time: Instant) -> U64 {
    let time = (Instant::now() - start_time).as_micros() as u64 / 100;
    U64::new(time)
}

pub fn current_timestamp_u32(start_time: Instant) -> U32 {
    let time = (Instant::now() - start_time).as_micros() as u64 / 100;
    U32::new(time as u32)
}

impl Drop for RtpMidiSession {
    fn drop(&mut self) {
        if !self.cancel_token.is_cancelled() {
            self.stop_immediately();
        }
    }
}

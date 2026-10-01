use super::rtp_midi_session::RtpMidiSession;
use std::time::{Duration, Instant};
use tracing::{Level, event, instrument};
use zerocopy::U64;

pub(super) struct HostSyncer {}
impl HostSyncer {
    pub fn new() -> Self {
        Self {}
    }

    async fn cleanup_stale_participants(&self, ctx: &RtpMidiSession) {
        let lock = ctx.participants.lock().await;

        if lock.is_empty() {
            event!(Level::DEBUG, "No participants to clean up");
            return;
        }

        let stale_participants: Vec<_> = lock
            .values()
            .filter(|p| p.is_invited_by_us() && Instant::now().duration_since(p.last_clock_sync()) >= Duration::from_secs(30))
            .cloned()
            .collect();

        drop(lock);

        if !stale_participants.is_empty() {
            event!(Level::INFO, "Removing {} stale participant(s)", stale_participants.len());

            for participant in stale_participants {
                let _ = ctx.remove_participant(&participant).await;
            }
        }
    }

    async fn send_clock_syncs(&self, ctx: &RtpMidiSession) {
        let timestamps = [U64::new(0); 3];
        let lock = ctx.participants.lock().await;
        let participants: Vec<_> = lock.values().cloned().collect();
        drop(lock);

        if !participants.is_empty() {
            event!(Level::DEBUG, "Sending clock sync to {} participants", participants.len());
            ctx.midi_port.send_clock_sync(&participants, timestamps, 0).await;
        } else {
            event!(Level::DEBUG, "No participants to send clock sync to");
        }
    }

    #[instrument(skip_all, fields(name = %ctx.name()))]
    pub async fn cleanup(&self, ctx: &RtpMidiSession) {
        self.cleanup_stale_participants(ctx).await;
        self.send_clock_syncs(ctx).await;
    }
}

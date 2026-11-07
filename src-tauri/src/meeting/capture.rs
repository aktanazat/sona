use super::types::{
    CapturedPacket, MeetingCaptureError, PacketPushResult, SessionClockAnchor, SourceClockEpoch,
    SourceGap, SourceGapReason, SourceProbe, SourceStartPlan, SourceStartReport, SourceStopReport,
    TimestampBridge,
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// The narrow capture boundary used by the microphone and system-audio owners.
/// The session manager owns lifecycle authority; an implementation owns only its
/// approved platform stream and reports observations through this contract.
pub trait MeetingCaptureSource: Send {
    fn probe(&self) -> SourceProbe;

    fn start(
        &mut self,
        plan: SourceStartPlan,
        anchor: SessionClockAnchor,
        sink: PacketSink,
    ) -> Result<SourceStartReport, MeetingCaptureError>;

    fn pause(&mut self) -> Result<(), MeetingCaptureError>;

    fn resume(
        &mut self,
        epoch: super::types::SourceEpoch,
    ) -> Result<SourceStartReport, MeetingCaptureError>;

    fn stop(&mut self) -> Result<SourceStopReport, MeetingCaptureError>;

    fn abort(&mut self) -> Result<(), MeetingCaptureError>;
}

/// A source-local, bounded, nonblocking packet sink. It owns paired sample and
/// descriptor rings, so audio callbacks publish samples before their descriptor.
/// The sink intentionally has no database, event, resampling, or lifecycle edge.
/// Its producers require exclusive access, preserving rtrb's SPSC contract.
pub struct PacketSink {
    track_id: super::types::SourceTrackId,
    samples: Producer<f32>,
    descriptors: Producer<CapturedPacket>,
    gaps: Producer<SourceGap>,
    dropped: Producer<DroppedRun>,
    clock_epochs: Producer<SourceClockEpoch>,
    gap_overflow: Arc<AtomicBool>,
    /// The run the lane is refusing now. It goes out when the lane takes a
    /// packet again, or when the source lets go of the lane.
    refusing: Option<DroppedRun>,
}

/// Consumer side of one source lane. It is intentionally single-consumer and is
/// owned by the ingest/persistence worker rather than by a capture callback.
pub struct PacketLaneReader {
    samples: Consumer<f32>,
    descriptors: Consumer<CapturedPacket>,
    gaps: Consumer<SourceGap>,
    dropped: Consumer<DroppedRun>,
    clock_epochs: Consumer<SourceClockEpoch>,
    gap_overflow: Arc<AtomicBool>,
}

/// Packets a full lane refused one after another, kept as the two ends and
/// the frame total. Only the writer holds the bridge that places a packet on
/// the session clock, so the lane hands it the packets themselves, and a
/// stall of any length costs one entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DroppedRun {
    pub first: CapturedPacket,
    pub last: CapturedPacket,
    pub frames: u64,
}

impl DroppedRun {
    pub fn new(packet: CapturedPacket) -> Self {
        Self {
            first: packet,
            last: packet,
            frames: u64::from(packet.frame_count),
        }
    }
}

/// A run goes out only when the lane takes a packet again, which needs the
/// writer to have made room since it last emptied this ring, so a lane never
/// holds more than a few.
const DROPPED_RUN_CAPACITY: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketLaneReadError {
    DescriptorWithoutSamples(CapturedPacket),
}

impl PacketSink {
    pub fn new(
        track_id: super::types::SourceTrackId,
        sample_capacity: usize,
        descriptor_capacity: usize,
    ) -> (Self, PacketLaneReader) {
        let sample_capacity = sample_capacity.max(1);
        let descriptor_capacity = descriptor_capacity.max(1);
        let (sample_producer, sample_consumer) = RingBuffer::new(sample_capacity);
        let (descriptor_producer, descriptor_consumer) = RingBuffer::new(descriptor_capacity);
        let (gap_producer, gap_consumer) = RingBuffer::new(descriptor_capacity);
        let (dropped_producer, dropped_consumer) = RingBuffer::new(DROPPED_RUN_CAPACITY);
        let (clock_epoch_producer, clock_epoch_consumer) = RingBuffer::new(descriptor_capacity);
        let gap_overflow = Arc::new(AtomicBool::new(false));
        (
            Self {
                track_id,
                samples: sample_producer,
                descriptors: descriptor_producer,
                gaps: gap_producer,
                dropped: dropped_producer,
                clock_epochs: clock_epoch_producer,
                gap_overflow: Arc::clone(&gap_overflow),
                refusing: None,
            },
            PacketLaneReader {
                samples: sample_consumer,
                descriptors: descriptor_consumer,
                gaps: gap_consumer,
                dropped: dropped_consumer,
                clock_epochs: clock_epoch_consumer,
                gap_overflow,
            },
        )
    }

    /// Copies an interleaved f32 packet into the preallocated source lane.
    /// Callback callers never allocate, lock, block, emit an event, or touch disk.
    pub fn try_push_interleaved(
        &mut self,
        packet: CapturedPacket,
        samples: &[f32],
    ) -> PacketPushResult {
        let Some(expected_sample_count) = packet.format().checked_frame_samples(packet.frame_count)
        else {
            self.report_invalid_packet(packet);
            return PacketPushResult::Dropped {
                frames: packet.frame_count,
            };
        };
        if packet.track_id != self.track_id || samples.len() != expected_sample_count {
            self.report_invalid_packet(packet);
            return PacketPushResult::Dropped {
                frames: packet.frame_count,
            };
        }

        if self.samples.slots() < samples.len() || self.descriptors.slots() == 0 {
            return self.refuse(packet);
        }

        // The capacity test is stable for a single producer: the consumer can
        // only free slots. push_entire_slice therefore cannot make a partial
        // publication; publish the descriptor only after the sample copy commits.
        if self.samples.push_entire_slice(samples).is_err()
            || self.descriptors.push(packet).is_err()
        {
            return self.refuse(packet);
        }

        self.publish_refusal();
        PacketPushResult::Accepted
    }

    /// Adds the packet to the run being refused. A packet on another clock
    /// starts a run of its own, so one bridge places both ends of each.
    fn refuse(&mut self, packet: CapturedPacket) -> PacketPushResult {
        match self.refusing.as_mut() {
            Some(run)
                if run.last.source_epoch == packet.source_epoch
                    && run.last.format_epoch == packet.format_epoch =>
            {
                run.last = packet;
                run.frames = run.frames.saturating_add(u64::from(packet.frame_count));
            }
            _ => {
                self.publish_refusal();
                self.refusing = Some(DroppedRun::new(packet));
            }
        }
        PacketPushResult::Dropped {
            frames: packet.frame_count,
        }
    }

    fn publish_refusal(&mut self) {
        if let Some(run) = self.refusing.take() {
            if self.dropped.push(run).is_err() {
                self.gap_overflow.store(true, Ordering::Release);
            }
        }
    }

    pub fn report_gap(&mut self, gap: SourceGap) {
        if self.gaps.push(gap).is_err() {
            self.gap_overflow.store(true, Ordering::Release);
        }
    }

    /// Publishes a new source-epoch bridge from an explicit platform clock
    /// observation. It never estimates a bridge from callback arrival time.
    /// false means the source must report an explicit gap and drop the packet
    /// that depends on this bridge.
    pub fn report_clock_epoch(&mut self, epoch: SourceClockEpoch) -> bool {
        if self.clock_epochs.push(epoch).is_ok() {
            true
        } else {
            self.gap_overflow.store(true, Ordering::Release);
            false
        }
    }

    fn report_invalid_packet(&mut self, packet: CapturedPacket) {
        self.report_gap(SourceGap {
            track_id: packet.track_id,
            epoch: packet.source_epoch,
            start_offset_ns: None,
            end_offset_ns: None,
            reason: SourceGapReason::InvalidFormat,
            dropped_frames: Some(u64::from(packet.frame_count)),
        });
    }
}

impl Drop for PacketSink {
    fn drop(&mut self) {
        self.publish_refusal();
    }
}

impl PacketLaneReader {
    /// Returns one descriptor and copies its matching samples into `output`.
    /// Worker-side allocation is permitted when the caller has not reserved a
    /// sufficiently large output buffer; the real-time producer never allocates.
    pub fn pop_into(
        &mut self,
        output: &mut Vec<f32>,
    ) -> Result<Option<CapturedPacket>, PacketLaneReadError> {
        let packet = match self.descriptors.pop() {
            Ok(packet) => packet,
            Err(_) => return Ok(None),
        };
        let Some(sample_count) = packet.format().checked_frame_samples(packet.frame_count) else {
            return Err(PacketLaneReadError::DescriptorWithoutSamples(packet));
        };
        if self.samples.slots() < sample_count {
            return Err(PacketLaneReadError::DescriptorWithoutSamples(packet));
        }
        output.clear();
        output.resize(sample_count, 0.0);
        if self.samples.pop_entire_slice(output).is_err() {
            return Err(PacketLaneReadError::DescriptorWithoutSamples(packet));
        }
        Ok(Some(packet))
    }

    pub fn pop_gap(&mut self) -> Option<SourceGap> {
        self.gaps.pop().ok()
    }

    pub fn pop_dropped(&mut self) -> Option<DroppedRun> {
        self.dropped.pop().ok()
    }

    pub fn pop_clock_epoch(&mut self) -> Option<SourceClockEpoch> {
        self.clock_epochs.pop().ok()
    }

    pub fn take_gap_overflow(&self) -> bool {
        self.gap_overflow.swap(false, Ordering::AcqRel)
    }
}

/// Runs the calling thread in the class macOS keeps for work a person is
/// waiting on, so default and background work, such as a local model writing
/// notes, cannot starve the threads that empty a lane.
pub(crate) fn run_at_capture_priority() {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: changes only the calling thread's scheduling class.
        let result = unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
        };
        if result != 0 {
            log::warn!("A capture thread kept its default priority: error {result}");
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SessionClock {
    anchor: SessionClockAnchor,
}

impl SessionClock {
    pub const fn new(anchor: SessionClockAnchor) -> Self {
        Self { anchor }
    }

    pub const fn anchor(self) -> SessionClockAnchor {
        self.anchor
    }

    pub fn map_packet(self, bridge: TimestampBridge, packet: CapturedPacket) -> Option<u64> {
        let (native_value, native_timescale) = packet.native_timestamp()?;
        let host_anchor = packet.host_monotonic_anchor_ns?;
        if host_anchor < self.anchor.host_monotonic_anchor_ns {
            return None;
        }
        bridge.map_native(native_value, native_timescale)
    }
}

/// Where a packet starts on the session clock, read through the bridge its
/// epoch announced.
pub fn packet_start_offset_ns(bridge: TimestampBridge, packet: CapturedPacket) -> Option<u64> {
    SessionClock::new(SessionClockAnchor {
        host_monotonic_anchor_ns: bridge.host_monotonic_anchor_ns,
        wall_start_utc_ms: 0,
        clock_policy_version: 1,
    })
    .map_packet(bridge, packet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::types::{PacketDiscontinuityFlags, SourceEpoch, SourceTrackId};

    fn packet(track_id: SourceTrackId) -> CapturedPacket {
        CapturedPacket {
            track_id,
            source_epoch: SourceEpoch::new(1),
            format_epoch: 1,
            sequence: 1,
            native_timestamp_value: Some(100),
            native_timestamp_timescale: Some(1_000_000_000),
            host_monotonic_anchor_ns: Some(100),
            sample_rate_hz: 48_000,
            channels: 1,
            frame_count: 2,
            discontinuity_flags: PacketDiscontinuityFlags::default(),
        }
    }

    #[test]
    fn packet_sink_keeps_descriptor_and_samples_paired() {
        let track_id = SourceTrackId::new();
        let (mut sink, mut reader) = PacketSink::new(track_id, 4, 1);
        assert_eq!(
            sink.try_push_interleaved(packet(track_id), &[0.25, -0.25]),
            PacketPushResult::Accepted
        );

        let mut samples = Vec::new();
        let read = reader.pop_into(&mut samples).unwrap().unwrap();
        assert_eq!(read.sequence, 1);
        assert_eq!(samples, vec![0.25, -0.25]);
    }

    /// A full lane refuses whole packets, and once it takes one again it hands
    /// the writer the stretch it refused: the first and last packet, for
    /// placing it in time, and every frame.
    #[test]
    fn full_lane_reports_the_stretch_it_refused() {
        let track_id = SourceTrackId::new();
        let (mut sink, mut reader) = PacketSink::new(track_id, 2, 1);
        let numbered = |sequence| CapturedPacket {
            sequence,
            ..packet(track_id)
        };
        assert_eq!(
            sink.try_push_interleaved(numbered(1), &[0.0, 0.0]),
            PacketPushResult::Accepted
        );
        for sequence in 2..=4 {
            assert_eq!(
                sink.try_push_interleaved(numbered(sequence), &[0.0, 0.0]),
                PacketPushResult::Dropped { frames: 2 }
            );
        }
        reader.pop_into(&mut Vec::new()).unwrap();
        assert_eq!(
            sink.try_push_interleaved(numbered(5), &[0.0, 0.0]),
            PacketPushResult::Accepted
        );

        assert_eq!(
            std::iter::from_fn(|| reader.pop_dropped()).collect::<Vec<_>>(),
            [DroppedRun {
                first: numbered(2),
                last: numbered(4),
                frames: 6,
            }]
        );
    }

    /// A source that lets its lane go in the middle of a stall still reports
    /// the stall, so a meeting stopped while its writer is stuck says what the
    /// stop cut off.
    #[test]
    fn a_lane_let_go_mid_stall_reports_the_stall() {
        let track_id = SourceTrackId::new();
        let (mut sink, mut reader) = PacketSink::new(track_id, 2, 1);
        assert_eq!(
            sink.try_push_interleaved(packet(track_id), &[0.0, 0.0]),
            PacketPushResult::Accepted
        );
        assert_eq!(
            sink.try_push_interleaved(packet(track_id), &[0.0, 0.0]),
            PacketPushResult::Dropped { frames: 2 }
        );

        drop(sink);

        assert_eq!(
            reader.pop_dropped(),
            Some(DroppedRun {
                first: packet(track_id),
                last: packet(track_id),
                frames: 2,
            })
        );
    }

    #[test]
    fn packet_sink_handoff_keeps_callback_writers_exclusive() {
        let track_id = SourceTrackId::new();
        let (sink, mut reader) = PacketSink::new(track_id, 4, 2);
        let (handoff_tx, handoff_rx) = std::sync::mpsc::sync_channel(0);

        let first_callback = std::thread::spawn(move || {
            let mut sink = sink;
            let mut first_packet = packet(track_id);
            first_packet.sequence = 1;
            assert_eq!(
                sink.try_push_interleaved(first_packet, &[0.25, -0.25]),
                PacketPushResult::Accepted
            );
            handoff_tx
                .send(sink)
                .expect("second callback receives writer");
        });
        let second_callback = std::thread::spawn(move || {
            let mut sink = handoff_rx.recv().expect("first callback releases writer");
            let mut second_packet = packet(track_id);
            second_packet.sequence = 2;
            assert_eq!(
                sink.try_push_interleaved(second_packet, &[0.5, -0.5]),
                PacketPushResult::Accepted
            );
        });

        first_callback.join().expect("first callback panicked");
        second_callback.join().expect("second callback panicked");
        let mut samples = Vec::new();
        assert_eq!(reader.pop_into(&mut samples).unwrap().unwrap().sequence, 1);
        assert_eq!(samples, vec![0.25, -0.25]);
        assert_eq!(reader.pop_into(&mut samples).unwrap().unwrap().sequence, 2);
        assert_eq!(samples, vec![0.5, -0.5]);
    }
}

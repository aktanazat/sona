use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, Manager};

#[derive(Clone, Copy, Debug, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum VoicePhase {
    Listening,
    SpeechStarted,
    Transcribing,
    Transcript,
    Speaking,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, Serialize, Type, tauri_specta::Event)]
pub struct ChatVoiceEvent {
    pub session_id: String,
    pub utterance_id: u64,
    pub phase: VoicePhase,
    pub text: Option<String>,
    pub error: Option<String>,
}

#[tauri::command(async)]
#[specta::specta]
pub fn chat_voice_start(app: AppHandle, session_id: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return app.state::<VoiceManager>().start(session_id);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, session_id);
        Err("Spoken chat requires macOS.".into())
    }
}

#[tauri::command(async)]
#[specta::specta]
pub fn chat_voice_stop(app: AppHandle, session_id: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        app.state::<VoiceManager>().stop(&session_id);
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, session_id);
        Err("Spoken chat requires macOS.".into())
    }
}

#[tauri::command(async)]
#[specta::specta]
pub fn chat_voice_speak(
    app: AppHandle,
    session_id: String,
    utterance_id: u64,
    text: String,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return app
        .state::<VoiceManager>()
        .speak(&session_id, utterance_id, &text);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, session_id, utterance_id, text);
        Err("Spoken chat requires macOS.".into())
    }
}

#[cfg(target_os = "macos")]
pub use runtime::VoiceManager;
#[cfg(target_os = "macos")]
#[path = "chat_voice_native.rs"]
mod native;

#[cfg(target_os = "macos")]
mod runtime {
    use super::{ChatVoiceEvent, VoicePhase};
    use crate::agent_panel::protocol::MAX_ASSISTANT_MESSAGE_BYTES;
    use crate::audio_toolkit::audio::FrameResampler;
    use crate::audio_toolkit::vad::{SmoothedVad, VadFrame, VoiceActivityDetector};
    use crate::managers::audio::{
        create_voice_detector, AudioRecordingManager, NativeMicrophoneLease,
    };
    use crate::managers::model::ModelManager;
    use crate::managers::transcription::TranscriptionManager;
    use crate::modes::AsrPlan;
    use crate::settings::get_settings;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{mpsc, Arc, Mutex, MutexGuard};
    use std::time::Duration;
    use tauri::AppHandle;
    use tauri_specta::Event as _;

    use super::native;

    const MAX_UTTERANCE_SAMPLES: usize = 16_000 * 120;
    const TURN_SILENCE_FRAMES: usize = 25;

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A cancelled session never admits another transcript or spoken answer.
    #[derive(Default)]
    struct VoiceTurns {
        latest: AtomicU64,
        closed: AtomicBool,
    }

    impl VoiceTurns {
        fn begin(&self) -> u64 {
            self.latest.fetch_add(1, Ordering::AcqRel) + 1
        }

        fn accepts(&self, utterance: u64) -> bool {
            !self.closed.load(Ordering::Acquire)
                && utterance != 0
                && self.latest.load(Ordering::Acquire) == utterance
        }

        fn close(&self) -> bool {
            !self.closed.swap(true, Ordering::AcqRel)
        }
    }

    struct Capture {
        // Drop the native stream before handing its microphone to another owner.
        voice: native::NativeVoice,
        _lease: NativeMicrophoneLease,
    }

    struct Session {
        id: String,
        app: AppHandle,
        asr: AsrPlan,
        turns: VoiceTurns,
        capture: Mutex<Option<Capture>>,
    }

    impl Session {
        fn emit(
            &self,
            utterance: u64,
            phase: VoicePhase,
            text: Option<String>,
            error: Option<String>,
        ) {
            let _ = ChatVoiceEvent {
                session_id: self.id.clone(),
                utterance_id: utterance,
                phase,
                text,
                error,
            }
            .emit(&self.app);
        }

        fn close(&self, error: Option<String>) {
            if !self.turns.close() {
                return;
            }
            // No wait for the model: a decode can outlive this microphone session.
            drop(lock(&self.capture).take());
            self.emit(
                self.turns.latest.load(Ordering::Acquire),
                if error.is_some() {
                    VoicePhase::Failed
                } else {
                    VoicePhase::Stopped
                },
                None,
                error,
            );
        }

        fn begin_utterance(&self) -> Option<u64> {
            let mut capture = lock(&self.capture);
            if self.turns.closed.load(Ordering::Acquire) {
                return None;
            }
            let capture = capture.as_mut()?;
            let utterance = self.turns.begin();
            capture.voice.interrupt();
            self.emit(utterance, VoicePhase::SpeechStarted, None, None);
            Some(utterance)
        }
    }

    struct TranscriptionJob {
        session: Arc<Session>,
        utterance: u64,
        samples: Vec<f32>,
    }

    pub struct VoiceManager {
        app: AppHandle,
        audio: Arc<AudioRecordingManager>,
        models: Arc<ModelManager>,
        transcription: Arc<TranscriptionManager>,
        session: Mutex<Option<Arc<Session>>>,
        jobs: Mutex<Option<mpsc::SyncSender<TranscriptionJob>>>,
    }

    impl VoiceManager {
        pub fn new(
            app: &AppHandle,
            audio: Arc<AudioRecordingManager>,
            models: Arc<ModelManager>,
            transcription: Arc<TranscriptionManager>,
        ) -> Self {
            Self {
                app: app.clone(),
                audio,
                models,
                transcription,
                session: Mutex::new(None),
                jobs: Mutex::new(None),
            }
        }

        fn job_sender(&self) -> Result<mpsc::SyncSender<TranscriptionJob>, String> {
            let mut jobs = lock(&self.jobs);
            if let Some(sender) = jobs.as_ref() {
                return Ok(sender.clone());
            }
            let (sender, receiver) = mpsc::sync_channel::<TranscriptionJob>(1);
            let transcription = Arc::clone(&self.transcription);
            std::thread::Builder::new()
                .name("chat-voice-asr".into())
                .spawn(move || {
                    while let Ok(job) = receiver.recv() {
                        if !job.session.turns.accepts(job.utterance) {
                            continue;
                        }
                        let decoded =
                            transcription.transcribe_shared(&job.session.asr, &job.samples);
                        if !job.session.turns.accepts(job.utterance) {
                            continue;
                        }
                        match decoded {
                            Ok(decoded) if !decoded.text.trim().is_empty() => job.session.emit(
                                job.utterance,
                                VoicePhase::Transcript,
                                Some(decoded.text),
                                None,
                            ),
                            Ok(_) => {
                                job.session
                                    .emit(job.utterance, VoicePhase::Listening, None, None)
                            }
                            Err(error) => job
                                .session
                                .close(Some(format!("Speech recognition failed: {error}"))),
                        }
                    }
                })
                .map_err(|error| format!("Could not start speech recognition: {error}"))?;
            *jobs = Some(sender.clone());
            Ok(sender)
        }

        pub fn start(&self, id: String) -> Result<(), String> {
            uuid::Uuid::parse_str(&id).map_err(|_| "Invalid voice session.".to_string())?;
            let mut current = lock(&self.session);
            if current
                .as_ref()
                .is_some_and(|session| !session.turns.closed.load(Ordering::Acquire))
            {
                return Err("A spoken conversation is already running.".into());
            }
            let asr = AsrPlan::from_settings(&get_settings(&self.app));
            if !self
                .models
                .get_model_info(&asr.model_id)
                .is_some_and(|model| model.is_downloaded)
            {
                return Err("Download and select a local speech model in Settings before starting voice chat.".into());
            }
            let lease = self.audio.try_acquire_native_microphone()
                .ok_or("Finish the active dictation, meeting, or screen recording before starting voice chat.")?;
            let detector = create_voice_detector(&self.app, TURN_SILENCE_FRAMES)
                .map_err(|error| format!("Voice detection is unavailable: {error}"))?;
            let jobs = self.job_sender()?;
            let (voice, reader) =
                native::NativeVoice::start(self.audio.native_microphone_name().as_deref())?;
            let session = Arc::new(Session {
                id,
                app: self.app.clone(),
                asr,
                turns: VoiceTurns::default(),
                capture: Mutex::new(Some(Capture {
                    voice,
                    _lease: lease,
                })),
            });
            session.emit(0, VoicePhase::Listening, None, None);
            let worker_session = Arc::clone(&session);
            if let Err(error) = std::thread::Builder::new()
                .name("chat-voice-capture".into())
                .spawn(move || capture_audio(worker_session, reader, detector, jobs))
            {
                session.close(None);
                return Err(format!("Could not start voice capture: {error}"));
            }
            *current = Some(session);
            Ok(())
        }

        pub fn stop(&self, id: &str) {
            let current = lock(&self.session);
            if let Some(session) = current.as_ref().filter(|session| session.id == id) {
                session.close(None);
            }
        }

        pub fn speak(&self, id: &str, utterance: u64, text: &str) -> Result<(), String> {
            if text.trim().is_empty() || text.len() > MAX_ASSISTANT_MESSAGE_BYTES {
                return Err("The answer cannot be read aloud.".into());
            }
            let current = lock(&self.session);
            let session = current
                .as_ref()
                .filter(|session| session.id == id)
                .ok_or("The spoken conversation has ended.")?;
            let mut capture = lock(&session.capture);
            if !session.turns.accepts(utterance) {
                return Err("That voice turn has ended.".into());
            }
            let capture = capture.as_mut().ok_or("The microphone has closed.")?;
            capture
                .voice
                .speak(text, &session.asr.language, utterance)?;
            session.emit(utterance, VoicePhase::Speaking, None, None);
            Ok(())
        }
    }

    impl Drop for VoiceManager {
        fn drop(&mut self) {
            if let Some(session) = lock(&self.session).take() {
                session.close(None);
            }
        }
    }

    fn capture_audio(
        session: Arc<Session>,
        mut reader: native::AudioReader,
        mut detector: SmoothedVad,
        jobs: mpsc::SyncSender<TranscriptionJob>,
    ) {
        let mut resampler =
            FrameResampler::new(reader.sample_rate, 16_000, Duration::from_millis(30));
        let mut buffer = [0.0f32; 4096];
        let mut samples = Vec::new();
        let mut utterance = None;
        while !session.turns.closed.load(Ordering::Acquire) {
            if let Some(error) = reader.failure() {
                session.close(Some(error.into()));
                break;
            }
            let finished = reader.finished();
            if session.turns.accepts(finished) {
                session.emit(finished, VoicePhase::Listening, None, None);
            }
            let count = reader.read(&mut buffer);
            if count == 0 {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            resampler.push(&buffer[..count], |frame| {
                if session.turns.closed.load(Ordering::Acquire) {
                    return;
                }
                match detector.push_frame(frame) {
                    Ok(VadFrame::Speech(speech)) => {
                        if utterance.is_none() {
                            utterance = session.begin_utterance();
                        }
                        if utterance.is_none() {
                            return;
                        }
                        if samples.len() + speech.len() > MAX_UTTERANCE_SAMPLES {
                            session
                                .close(Some("Keep each spoken question under two minutes.".into()));
                            return;
                        }
                        samples.extend_from_slice(speech);
                    }
                    Ok(VadFrame::Noise) => {
                        if let Some(id) = utterance.take() {
                            session.emit(id, VoicePhase::Transcribing, None, None);
                            let job = TranscriptionJob {
                                session: Arc::clone(&session),
                                utterance: id,
                                samples: std::mem::take(&mut samples),
                            };
                            if jobs.try_send(job).is_err() {
                                session.close(Some(
                                    "Speech recognition could not keep up. Start voice chat again."
                                        .into(),
                                ));
                            }
                        }
                    }
                    Err(error) => session.close(Some(format!("Voice detection failed: {error}"))),
                }
            });
        }
    }

    #[cfg(test)]
    mod tests {
        use super::VoiceTurns;

        #[test]
        fn interruption_rejects_previous_transcripts_and_answers() {
            let turns = VoiceTurns::default();
            let first = turns.begin();
            assert!(turns.accepts(first));
            let interrupted_by = turns.begin();
            assert!(!turns.accepts(first));
            assert!(turns.accepts(interrupted_by));
        }

        #[test]
        fn stopped_session_rejects_pending_and_late_speech() {
            let turns = VoiceTurns::default();
            let pending = turns.begin();
            turns.close();
            assert!(!turns.accepts(pending));
            let late = turns.begin();
            assert!(!turns.accepts(late));
        }
    }
}

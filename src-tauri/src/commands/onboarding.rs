use crate::managers::audio::{AudioRecordingManager, RecordingStop};
use crate::managers::model::ModelManager;
use crate::managers::transcription::TranscriptionManager;
use crate::modes::AsrPlan;
use crate::settings::{get_settings, update_settings, AppSettings};
use serde::Serialize;
use specta::Type;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

const READY_TIMEOUT: Duration = Duration::from_secs(10);
const PRACTICE_LIMIT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq)]
enum PracticePhase {
    Starting,
    Listening,
    Processing,
    Delivered,
    Cancelling,
}

struct PracticeSession {
    id: String,
    phase: PracticePhase,
    plan: AsrPlan,
    cancelled: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct OnboardingPracticeManager {
    session: Mutex<Option<PracticeSession>>,
    capture_operation: Mutex<()>,
}

#[derive(Clone, Serialize, Type)]
pub struct OnboardingPracticeTranscript {
    pub session_id: String,
    pub text: String,
}

#[derive(Clone, Serialize)]
struct PracticeExpired {
    session_id: String,
    error: String,
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn language_base(code: &str) -> &str {
    code.split(['-', '_']).next().unwrap_or(code)
}

fn model_supports_practice(
    languages: &[String],
    supported: &[String],
    detects_language: bool,
) -> bool {
    if languages.is_empty() {
        return detects_language && supported.len() > 1;
    }
    languages.iter().all(|language| {
        supported
            .iter()
            .any(|candidate| language_base(candidate).eq_ignore_ascii_case(language_base(language)))
    })
}

fn practice_choices_match(settings: &AppSettings, plan: &AsrPlan) -> bool {
    let (language, candidates): (&str, &[String]) = if settings.selected_language != "auto" {
        (&settings.selected_language, &[])
    } else {
        match settings.dictation_languages.as_slice() {
            [language] => (language, &[]),
            languages => ("auto", languages),
        }
    };
    settings.selected_model == plan.model_id
        && language == plan.language
        && candidates == plan.language_candidates
}
fn practice_audio(capture: RecordingStop) -> Result<Vec<f32>, String> {
    match capture {
        RecordingStop::Complete { samples, level, .. } => {
            if samples.is_empty() || level.peak == 0.0 {
                return Err("No speech heard. Try again.".to_string());
            }
            Ok(samples)
        }
        RecordingStop::NoSpeech { .. } => Err("No speech heard. Try again.".to_string()),
        RecordingStop::Overrun { .. } => {
            Err("Microphone audio was interrupted. Try again.".to_string())
        }
    }
}

impl OnboardingPracticeManager {
    fn begin(&self, id: String, plan: AsrPlan) -> Result<Arc<AtomicBool>, String> {
        let mut held = lock(&self.session);
        if held
            .as_ref()
            .is_some_and(|session| session.phase != PracticePhase::Delivered)
        {
            return Err("A practice dictation is already running.".to_string());
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        *held = Some(PracticeSession {
            id,
            phase: PracticePhase::Starting,
            plan,
            cancelled: Arc::clone(&cancelled),
        });
        Ok(cancelled)
    }

    fn is_phase(&self, id: &str, attempt: &Arc<AtomicBool>, phase: PracticePhase) -> bool {
        lock(&self.session).as_ref().is_some_and(|session| {
            session.id == id && Arc::ptr_eq(&session.cancelled, attempt) && session.phase == phase
        })
    }

    fn transition(
        &self,
        id: &str,
        attempt: &Arc<AtomicBool>,
        from: PracticePhase,
        to: PracticePhase,
    ) -> bool {
        let mut held = lock(&self.session);
        let Some(session) = held.as_mut() else {
            return false;
        };
        if session.id != id || !Arc::ptr_eq(&session.cancelled, attempt) || session.phase != from {
            return false;
        }
        session.phase = to;
        true
    }

    fn clear(&self, id: &str, attempt: &Arc<AtomicBool>) {
        let mut held = lock(&self.session);
        if held
            .as_ref()
            .is_some_and(|session| session.id == id && Arc::ptr_eq(&session.cancelled, attempt))
        {
            *held = None;
        }
    }

    fn cancel_attempt(&self, id: &str, attempt: &Arc<AtomicBool>, close_capture: impl FnOnce()) {
        {
            let mut held = lock(&self.session);
            let Some(session) = held
                .as_mut()
                .filter(|session| session.id == id && Arc::ptr_eq(&session.cancelled, attempt))
            else {
                return;
            };
            session.cancelled.store(true, Ordering::Release);
            session.phase = PracticePhase::Cancelling;
        }
        let _operation = lock(&self.capture_operation);
        if !self.is_phase(id, attempt, PracticePhase::Cancelling) {
            return;
        }
        close_capture();
        self.clear(id, attempt);
    }

    fn cancel(&self, id: &str, audio: &AudioRecordingManager) {
        let attempt = lock(&self.session)
            .as_ref()
            .filter(|session| session.id == id)
            .map(|session| Arc::clone(&session.cancelled));
        if let Some(attempt) = attempt {
            self.cancel_attempt(id, &attempt, || audio.cancel_practice_recording());
        }
    }
}

#[tauri::command]
#[specta::specta]
pub async fn start_onboarding_practice(
    app: AppHandle,
    manager: State<'_, Arc<OnboardingPracticeManager>>,
    audio: State<'_, Arc<AudioRecordingManager>>,
    session_id: String,
) -> Result<(), String> {
    if session_id.is_empty() || session_id.len() > 128 {
        return Err("Invalid practice session.".to_string());
    }
    let settings = get_settings(&app);
    let model_manager = app.state::<Arc<ModelManager>>();
    let model = model_manager
        .get_model_info(&settings.selected_model)
        .filter(|model| model.is_downloaded)
        .ok_or_else(|| "Choose a downloaded speech model first.".to_string())?;
    let languages = if settings.selected_language != "auto" {
        vec![settings.selected_language.clone()]
    } else {
        settings.dictation_languages.clone()
    };
    if !model_supports_practice(
        &languages,
        &model.supported_languages,
        model.supports_language_detection,
    ) {
        return Err("Choose a speech model that supports your dictation languages.".to_string());
    }
    let attempt = manager.begin(session_id.clone(), AsrPlan::from_settings(&settings))?;
    let manager = Arc::clone(&manager);
    let audio = Arc::clone(&audio);
    let worker_manager = Arc::clone(&manager);
    let worker_audio = Arc::clone(&audio);
    let worker_id = session_id.clone();
    let worker_attempt = Arc::clone(&attempt);
    let result = tauri::async_runtime::spawn_blocking(move || {
        let readiness = {
            let _operation = lock(&worker_manager.capture_operation);
            if !worker_manager.is_phase(&worker_id, &worker_attempt, PracticePhase::Starting) {
                return Err("Practice cancelled.".to_string());
            }
            worker_audio.start_practice_recording(&worker_attempt)?
        };
        if !readiness.wait_timeout(READY_TIMEOUT) {
            worker_manager.cancel_attempt(&worker_id, &worker_attempt, || {
                worker_audio.cancel_practice_recording();
            });
            return Err(
                "No microphone audio arrived. Check your microphone and try again.".to_string(),
            );
        }
        if !worker_manager.transition(
            &worker_id,
            &worker_attempt,
            PracticePhase::Starting,
            PracticePhase::Listening,
        ) {
            return Err("Practice cancelled.".to_string());
        }
        Ok(())
    })
    .await
    .map_err(|error| format!("Practice could not start: {error}"))
    .and_then(|result| result);
    if result.is_err() {
        manager.cancel_attempt(&session_id, &attempt, || audio.cancel_practice_recording());
        return result;
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(PRACTICE_LIMIT).await;
        if manager.is_phase(&session_id, &attempt, PracticePhase::Listening) {
            let worker_manager = Arc::clone(&manager);
            let worker_id = session_id.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || {
                worker_manager
                    .cancel_attempt(&worker_id, &attempt, || audio.cancel_practice_recording());
            })
            .await;
            let _ = app.emit(
                "onboarding-practice-expired",
                PracticeExpired {
                    session_id,
                    error: "Practice stopped after one minute. Try a shorter dictation."
                        .to_string(),
                },
            );
        }
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn stop_onboarding_practice(
    manager: State<'_, Arc<OnboardingPracticeManager>>,
    audio: State<'_, Arc<AudioRecordingManager>>,
    transcription: State<'_, Arc<TranscriptionManager>>,
    session_id: String,
) -> Result<OnboardingPracticeTranscript, String> {
    let (plan, attempt) = {
        let mut held = lock(&manager.session);
        let session = held
            .as_mut()
            .filter(|session| session.id == session_id && session.phase == PracticePhase::Listening)
            .ok_or_else(|| "Practice is not listening. Try again.".to_string())?;
        session.phase = PracticePhase::Processing;
        (session.plan.clone(), Arc::clone(&session.cancelled))
    };
    let manager = Arc::clone(&manager);
    let audio = Arc::clone(&audio);
    let transcription = Arc::clone(&transcription);
    let worker_manager = Arc::clone(&manager);
    let worker_id = session_id.clone();
    let worker_audio = Arc::clone(&audio);
    let worker_attempt = Arc::clone(&attempt);
    let result = tauri::async_runtime::spawn_blocking(move || {
        let captured = {
            let _operation = lock(&worker_manager.capture_operation);
            if !worker_manager.is_phase(&worker_id, &worker_attempt, PracticePhase::Processing) {
                return Err("Practice cancelled.".to_string());
            }
            worker_audio.stop_practice_recording()?
        };
        let samples = practice_audio(captured)?;
        if !worker_manager.is_phase(&worker_id, &worker_attempt, PracticePhase::Processing) {
            return Err("Practice cancelled.".to_string());
        }
        let decoded = transcription
            .transcribe_shared(&plan, &samples)
            .map_err(|error| format!("Could not transcribe practice. {error}"))?;
        let text = decoded.text.trim().to_string();
        if text.is_empty() {
            return Err("No words delivered. Try again.".to_string());
        }
        if !worker_manager.transition(
            &worker_id,
            &worker_attempt,
            PracticePhase::Processing,
            PracticePhase::Delivered,
        ) {
            return Err("Practice cancelled.".to_string());
        }
        Ok(OnboardingPracticeTranscript {
            session_id: worker_id,
            text,
        })
    })
    .await
    .map_err(|error| format!("Practice could not finish: {error}"))
    .and_then(|result| result);
    if result.is_err() {
        manager.cancel_attempt(&session_id, &attempt, || audio.cancel_practice_recording());
    }
    result
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_onboarding_practice(
    manager: State<'_, Arc<OnboardingPracticeManager>>,
    audio: State<'_, Arc<AudioRecordingManager>>,
    session_id: String,
) -> Result<(), String> {
    let manager = Arc::clone(&manager);
    let audio = Arc::clone(&audio);
    tauri::async_runtime::spawn_blocking(move || manager.cancel(&session_id, &audio))
        .await
        .map_err(|error| format!("Practice could not cancel: {error}"))
}

#[tauri::command]
#[specta::specta]
pub fn complete_onboarding_practice(
    app: AppHandle,
    manager: State<'_, Arc<OnboardingPracticeManager>>,
    session_id: String,
) -> Result<(), String> {
    let mut held = lock(&manager.session);
    let session = held
        .as_ref()
        .filter(|session| session.id == session_id && session.phase == PracticePhase::Delivered)
        .ok_or_else(|| "Complete a practice dictation first.".to_string())?;
    let completed = update_settings(&app, |settings| {
        if !practice_choices_match(settings, &session.plan) {
            return false;
        }
        settings.onboarding_completed = true;
        true
    })?;
    if !completed {
        return Err("The speech model or languages changed. Try practice again.".to_string());
    }
    *held = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn french_setup_rejects_english_only_model() {
        assert!(!model_supports_practice(
            &codes(&["fr"]),
            &codes(&["en"]),
            false
        ));
    }

    #[test]
    fn multilingual_setup_requires_every_selected_language() {
        assert!(!model_supports_practice(
            &codes(&["en", "ja"]),
            &codes(&["en", "fr"]),
            true
        ));
    }

    #[test]
    fn regional_language_matches_compatible_catalog_language() {
        assert!(model_supports_practice(
            &codes(&["pt-BR", "zh-Hant"]),
            &codes(&["pt", "zh"]),
            true
        ));
    }

    #[test]
    fn unrestricted_auto_detection_rejects_english_only_model() {
        assert!(!model_supports_practice(&[], &codes(&["en"]), false));
    }

    #[test]
    fn cancelled_decode_cannot_deliver_or_complete() {
        let manager = OnboardingPracticeManager::default();
        let attempt = manager
            .begin(
                "cancelled".to_string(),
                AsrPlan::from_settings(&Default::default()),
            )
            .expect("fresh practice begins");
        manager.cancel_attempt("cancelled", &attempt, || {});
        assert!(!manager.transition(
            "cancelled",
            &attempt,
            PracticePhase::Processing,
            PracticePhase::Delivered
        ));
        assert!(!manager.is_phase("cancelled", &attempt, PracticePhase::Delivered));
    }

    #[test]
    fn cancellation_invalidates_a_start_while_device_open_is_blocked() {
        let manager = Arc::new(OnboardingPracticeManager::default());
        let attempt = manager
            .begin(
                "blocked".to_string(),
                AsrPlan::from_settings(&Default::default()),
            )
            .expect("fresh practice begins");
        let operation = lock(&manager.capture_operation);
        let worker_manager = Arc::clone(&manager);
        let worker_attempt = Arc::clone(&attempt);
        let cancelled = std::thread::spawn(move || {
            worker_manager.cancel_attempt("blocked", &worker_attempt, || {});
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !attempt.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        let invalidated_before_device_open_returned = attempt.load(Ordering::Acquire);
        drop(operation);
        cancelled.join().expect("cancellation joins");
        assert!(invalidated_before_device_open_returned);
        assert!(!manager.transition(
            "blocked",
            &attempt,
            PracticePhase::Starting,
            PracticePhase::Listening
        ));
    }

    #[test]
    fn stale_cancellation_cannot_close_a_later_attempt_with_the_same_client_id() {
        let manager = OnboardingPracticeManager::default();
        let old = manager
            .begin(
                "reused".to_string(),
                AsrPlan::from_settings(&Default::default()),
            )
            .expect("first practice begins");
        manager.cancel_attempt("reused", &old, || {});
        let current = manager
            .begin(
                "reused".to_string(),
                AsrPlan::from_settings(&Default::default()),
            )
            .expect("retry begins");
        manager.cancel_attempt("reused", &old, || {
            panic!("stale cancellation reached the new microphone")
        });
        assert!(!current.load(Ordering::Acquire));
        assert!(manager.is_phase("reused", &current, PracticePhase::Starting));
        assert!(!manager.transition(
            "reused",
            &old,
            PracticePhase::Processing,
            PracticePhase::Delivered
        ));
    }

    #[test]
    fn digital_silence_cannot_be_delivered_as_practice_words() {
        let captured = RecordingStop::Complete {
            samples: vec![0.0; 160],
            vad_forwarded_speech: false,
            level: Default::default(),
        };
        assert!(practice_audio(captured).is_err());
    }

    #[test]
    fn interrupted_capture_never_delivers_its_partial_prefix() {
        let captured = RecordingStop::Overrun {
            prefix_samples: vec![0.2; 160],
            level: crate::managers::audio::InputLevel {
                peak: 0.2,
                rms: 0.2,
            },
        };
        assert!(practice_audio(captured).is_err());
    }

    #[test]
    fn quiet_real_samples_are_kept_for_the_existing_recognizer() {
        let captured = RecordingStop::Complete {
            samples: vec![0.0001; 160],
            vad_forwarded_speech: false,
            level: crate::managers::audio::InputLevel {
                peak: 0.0001,
                rms: 0.0001,
            },
        };
        assert_eq!(practice_audio(captured), Ok(vec![0.0001; 160]));
    }

    #[test]
    fn changing_languages_after_practice_requires_another_dictation() {
        let mut settings = AppSettings {
            selected_model: "speech".to_string(),
            selected_language: "fr".to_string(),
            ..Default::default()
        };
        let plan = AsrPlan::from_settings(&settings);
        settings.selected_language = "en".to_string();
        assert!(!practice_choices_match(&settings, &plan));
    }

    #[test]
    fn unchanged_multilingual_choices_can_complete_practice() {
        let settings = AppSettings {
            selected_model: "speech".to_string(),
            selected_language: "auto".to_string(),
            dictation_languages: codes(&["fr", "en"]),
            ..Default::default()
        };
        let plan = AsrPlan::from_settings(&settings);
        assert!(practice_choices_match(&settings, &plan));
    }
}

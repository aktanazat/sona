pub mod audio;
pub mod constants;
pub mod lang_id;
pub mod ort_session;
pub mod spoken_edits;
pub mod text;
pub mod utils;
pub mod vad;

pub use audio::{
    is_microphone_access_denied, is_no_input_device_error, list_input_devices, list_output_devices,
    read_wav_samples, repair_spool, save_wav_file, verify_wav_file, AudioRecorder, CaptureError,
    CaptureOverrun, CpalDeviceInfo, RecordedAudio, VadPolicy,
};
pub use lang_id::detect_output_language;
pub use spoken_edits::{apply_spoken_edits, split_spoken_instruction, SpokenInstruction};
pub use text::{
    apply_british_spelling, apply_emoji_replacements, apply_literal_punctuation,
    apply_prompted_vocabulary_entries, apply_text_replacements, apply_vocabulary_entries,
    normalize_transcription_output, remove_filler_words, OutputLanguageEvidence,
};
pub use utils::get_cpal_host;
pub use vad::{SileroVad, VoiceActivityDetector};

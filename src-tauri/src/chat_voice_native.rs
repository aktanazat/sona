use rtrb::{Consumer, Producer, RingBuffer};
use std::cell::UnsafeCell;
use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::Arc;

const SAMPLE_CAPACITY: usize = 192_000 * 2;

#[derive(Default)]
struct CallbackFlags {
    failure: AtomicI32,
    finished: AtomicU64,
}

struct CallbackContext {
    // AVAudioEngine invokes one tap serially. Only that tap accesses the producer;
    // status callbacks touch the atomics, never this cell.
    samples: UnsafeCell<Producer<f32>>,
    flags: Arc<CallbackFlags>,
}

unsafe extern "C" fn audio_callback(
    context: *mut c_void,
    samples: *const f32,
    count: usize,
    stride: usize,
) {
    // SAFETY: Swift retains the context owner for the whole tap callback.
    let context = unsafe { &*context.cast::<CallbackContext>() };
    // SAFETY: AVAudioEngine serializes this tap; no other callback accesses the producer.
    let producer = unsafe { &mut *context.samples.get() };
    if stride == 0 || producer.slots() < count {
        context.flags.failure.store(5, Ordering::Release);
        return;
    }
    if stride == 1 {
        // SAFETY: the contiguous PCM buffer supplies count valid Float samples.
        let samples = unsafe { std::slice::from_raw_parts(samples, count) };
        if producer.push_entire_slice(samples).is_err() {
            context.flags.failure.store(5, Ordering::Release);
        }
    } else {
        for frame in 0..count {
            // SAFETY: the PCM buffer supplies count frames at its declared stride.
            let sample = unsafe { *samples.add(frame * stride) };
            if producer.push(sample).is_err() {
                context.flags.failure.store(5, Ordering::Release);
                return;
            }
        }
    }
}

unsafe extern "C" fn status_callback(context: *mut c_void, status: i32, utterance: u64) {
    // SAFETY: the Swift callback owner outlives every callback, including status.
    let context = unsafe { &*context.cast::<CallbackContext>() };
    if status == 4 {
        context.flags.finished.store(utterance, Ordering::Release);
    } else {
        context.flags.failure.store(status, Ordering::Release);
    }
}

unsafe extern "C" fn release_callback(context: *mut c_void) {
    // SAFETY: the one Swift owner calls this once, after its final tap reference
    // is released. Rust transferred this box in NativeVoice::start.
    drop(unsafe { Box::from_raw(context.cast::<CallbackContext>()) });
}

unsafe extern "C" {
    fn sona_voice_start(
        microphone: *const c_char,
        context: *mut c_void,
        audio: unsafe extern "C" fn(*mut c_void, *const f32, usize, usize),
        status: unsafe extern "C" fn(*mut c_void, i32, u64),
        release: unsafe extern "C" fn(*mut c_void),
        sample_rate: *mut usize,
        error: *mut c_char,
        error_capacity: usize,
    ) -> *mut c_void;
    fn sona_voice_speak(
        handle: *mut c_void,
        text: *const c_char,
        language: *const c_char,
        utterance: u64,
    );
    fn sona_voice_interrupt(handle: *mut c_void);
    fn sona_voice_stop(handle: *mut c_void);
}

pub(super) struct NativeVoice(NonNull<c_void>);

// SAFETY: the session serializes all handle access. Swift runs each operation on
// its main queue, and stop consumes the retained handle exactly once.
unsafe impl Send for NativeVoice {}

pub(super) struct AudioReader {
    samples: Consumer<f32>,
    flags: Arc<CallbackFlags>,
    pub sample_rate: usize,
}

impl NativeVoice {
    pub fn start(microphone: Option<&str>) -> Result<(Self, AudioReader), String> {
        let microphone = microphone
            .map(CString::new)
            .transpose()
            .map_err(|_| "Invalid microphone name.".to_string())?;
        let (producer, consumer) = RingBuffer::new(SAMPLE_CAPACITY);
        let flags = Arc::new(CallbackFlags::default());
        let context = Box::into_raw(Box::new(CallbackContext {
            samples: UnsafeCell::new(producer),
            flags: Arc::clone(&flags),
        }));
        let mut rate = 0;
        let mut error = [0; 512];
        // Swift takes the box on success and failure.
        // SAFETY: the pointers and callbacks remain valid through this synchronous call.
        let handle = unsafe {
            sona_voice_start(
                microphone
                    .as_ref()
                    .map_or(std::ptr::null(), |name| name.as_ptr()),
                context.cast(),
                audio_callback,
                status_callback,
                release_callback,
                &mut rate,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        let handle = NonNull::new(handle).ok_or_else(|| {
            // SAFETY: Swift writes within this zero-filled buffer and terminates it.
            unsafe { CStr::from_ptr(error.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        })?;
        Ok((
            Self(handle),
            AudioReader {
                samples: consumer,
                flags,
                sample_rate: rate,
            },
        ))
    }

    pub fn speak(&mut self, text: &str, language: &str, utterance: u64) -> Result<(), String> {
        let text = CString::new(text).map_err(|_| "The answer contains invalid speech text.")?;
        let language = CString::new(language).map_err(|_| "Invalid speech language.")?;
        // SAFETY: the handle is owned and live; Swift copies both strings before returning.
        unsafe { sona_voice_speak(self.0.as_ptr(), text.as_ptr(), language.as_ptr(), utterance) };
        Ok(())
    }

    pub fn interrupt(&mut self) {
        // SAFETY: the handle stays live while its session holds this borrow.
        unsafe { sona_voice_interrupt(self.0.as_ptr()) };
    }
}

impl Drop for NativeVoice {
    fn drop(&mut self) {
        // SAFETY: no other handle owner exists, and Swift consumes its retained reference.
        unsafe { sona_voice_stop(self.0.as_ptr()) };
    }
}

impl AudioReader {
    pub fn read(&mut self, output: &mut [f32]) -> usize {
        let count = self.samples.slots().min(output.len());
        if self.samples.pop_entire_slice(&mut output[..count]).is_ok() {
            count
        } else {
            self.flags.failure.store(5, Ordering::Release);
            0
        }
    }

    pub fn failure(&self) -> Option<&'static str> {
        match self.flags.failure.load(Ordering::Acquire) {
            0 => None,
            1 => Some("The microphone audio format changed. Start voice chat again."),
            2 => Some("The audio device disconnected or changed. Start voice chat again."),
            3 => Some("The local voice could not read the answer aloud."),
            _ => Some("Voice capture could not keep up. Start voice chat again."),
        }
    }

    pub fn finished(&self) -> u64 {
        self.flags.finished.swap(0, Ordering::AcqRel)
    }
}

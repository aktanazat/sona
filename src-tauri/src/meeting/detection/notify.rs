//! Meeting-detection prompt delivery.
//!
//! The consent panel is the first delivery tier. Native notifications are its
//! fallback: they respect Focus and Do Not Disturb, remain in Notification
//! Center when missed, and can be silenced at the OS level.
//!
//! `PanelSlot` owns which pending prompt may use the panel. Its transitions are
//! pure and return commands; the runtime applies those commands in transition
//! order while it holds the state lock once. Presenters never decide priority
//! or construct detection events.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tauri::AppHandle;

use super::machine::PromptKind;
use super::DetectionPromptDelivery;
use crate::meeting::consent_panel::ConsentPanelLayout;

/// Action identifier for the affirmative button. Also the value reported back
/// through `PromptResponse`.
pub const ACTION_START: &str = "computer.sona.detection.start";
/// Action identifier for the dismissive button.
pub const ACTION_DISMISS: &str = "computer.sona.detection.dismiss";
/// The one category all detection prompts use, so the two buttons are
/// registered once rather than per notification.
pub const CATEGORY_DETECTION: &str = "computer.sona.detection.prompt";

/// The affirmative button's label, per §5.4.
pub const START_TITLE: &str = "Start Transcribing";
/// The dismissive button's label, per §5.4.
pub const DISMISS_TITLE: &str = "Dismiss";

/// The evening digest's category. It carries no actions: the whole notification
/// is one sentence and one place to go, so the body click is the only gesture
/// and no buttons are registered against it.
///
/// It is also the identifier every digest is posted under, so a second one
/// replaces the first in Notification Center rather than stacking. There is one
/// digest per local day; there is never a reason to see two.
pub const CATEGORY_DIGEST: &str = "computer.sona.digest.evening";

/// PREP/WRAP fallback category. Title only, with no notification actions.
pub const CATEGORY_RITUAL: &str = "computer.sona.meeting.ritual";

/// What the operator did with a prompt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PromptResponse {
    /// "Start Transcribing", or clicking the notification body.
    Start { prompt_id: String },
    /// "Dismiss".
    Dismiss { prompt_id: String },
    /// The evening digest's body was clicked.
    DigestOpened,
}

/// Receives detection action clicks. Implemented by the detection runtime.
pub trait PromptResponder: Send + Sync {
    fn prompt_answered(&self, response: PromptResponse);
}

/// Receives the digest's one gesture. Implemented next to the digest, because
/// where a digest click lands is the digest's decision and not detection's.
pub trait DigestOpener: Send + Sync {
    fn digest_opened(&self);
}

/// The delegate's one target, and the fork behind it.
///
/// The notification centre holds a single delegate for the whole process, but
/// the two things it can deliver belong to different owners: a detection answer
/// is the detection runtime's, and a digest click is the digest's. Both are
/// bound after startup and this is where a response is sorted, so neither owner
/// has to know the other exists.
///
/// A click that arrives before its bind is logged and dropped.
#[derive(Default)]
pub struct ResponderCell {
    inner: std::sync::OnceLock<Arc<dyn PromptResponder>>,
    digest: std::sync::OnceLock<Arc<dyn DigestOpener>>,
}

impl ResponderCell {
    pub fn bind(&self, responder: Arc<dyn PromptResponder>) {
        let _ = self.inner.set(responder);
    }

    pub fn bind_digest(&self, opener: Arc<dyn DigestOpener>) {
        let _ = self.digest.set(opener);
    }
}

impl PromptResponder for ResponderCell {
    fn prompt_answered(&self, response: PromptResponse) {
        if response == PromptResponse::DigestOpened {
            match self.digest.get() {
                Some(opener) => opener.digest_opened(),
                None => log::info!(
                    "The evening digest dropped a notification click that arrived before startup \
                     finished"
                ),
            }
            return;
        }
        match self.inner.get() {
            Some(responder) => responder.prompt_answered(response),
            None => log::info!(
                "Meeting detection dropped a notification click that arrived before startup \
                 finished"
            ),
        }
    }
}

/// Whether the operator has allowed notifications.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum NotificationAccess {
    NotDetermined,
    Authorized,
    Denied,
    /// No notification center reachable: an unbundled build, or a non-macOS
    /// target.
    Unavailable,
}

pub type NotificationAccessFuture =
    Pin<Box<dyn Future<Output = NotificationAccess> + Send + 'static>>;

/// Native notification delivery. All methods return without waiting for an
/// Objective-C completion handler.
pub trait PromptPresenter: Send + Sync {
    fn access(&self) -> NotificationAccess;
    fn request_access(&self) -> NotificationAccessFuture;
    fn present(&self, prompt_id: &str, prompt: &PromptKind) -> bool;
    fn withdraw(&self, prompt_id: &str);
    /// Posts the evening digest. `false` means nothing was delivered, which is
    /// the answer on every build without an authorized notification centre.
    ///
    /// Title and body arrive as finished strings. The OS renders them from a
    /// Rust `&str` and cannot reach the frontend's i18next catalog, which is
    /// the same reason `PromptKind::notification_title` is English.
    fn present_digest(&self, title: &str, body: &str) -> bool;
    /// Posts a title-only PREP/WRAP fallback.
    fn present_ritual(&self, ritual_id: &str, title: &str) -> bool;
}

/// Used when no notification center is reachable. Detection still runs and
/// falls back to the in-app card and toast.
pub struct NoPrompts;

impl PromptPresenter for NoPrompts {
    fn access(&self) -> NotificationAccess {
        NotificationAccess::Unavailable
    }

    fn request_access(&self) -> NotificationAccessFuture {
        Box::pin(async { NotificationAccess::Unavailable })
    }

    fn present(&self, _prompt_id: &str, _prompt: &PromptKind) -> bool {
        false
    }

    fn withdraw(&self, _prompt_id: &str) {}

    fn present_digest(&self, _title: &str, _body: &str) -> bool {
        false
    }

    fn present_ritual(&self, _ritual_id: &str, _title: &str) -> bool {
        false
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum PanelCommand<T> {
    ShowPanel,
    HidePanel,
    WithdrawPrompt { prompt_id: String },
    PresentPanel { prompt_id: String, prompt: T },
    PresentFallback { prompt_id: String, prompt: T },
    Acknowledged { prompt_id: String, prompt: T },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PanelFinish<T> {
    pub removed: Option<T>,
    pub commands: Vec<PanelCommand<T>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PanelCapture<T> {
    pub discarded: Vec<(String, T)>,
    pub commands: Vec<PanelCommand<T>>,
}

#[derive(Clone, Debug)]
struct PanelEntry<T> {
    prompt: T,
    priority: u8,
    sequence: u64,
    panel_eligible: bool,
}

#[derive(Clone, Debug)]
struct PanelOwner {
    prompt_id: String,
    acknowledged: bool,
}

/// The pending prompt set and its single panel owner.
///
/// A prompt remains pending after fallback so an answer from a native
/// notification can still resolve it. `owner` names only the prompt currently
/// awaiting or holding the panel.
#[derive(Clone, Debug)]
pub(super) struct PanelSlot<T> {
    pending: HashMap<String, PanelEntry<T>>,
    owner: Option<PanelOwner>,
    next_sequence: u64,
}

impl<T> Default for PanelSlot<T> {
    fn default() -> Self {
        Self {
            pending: HashMap::new(),
            owner: None,
            next_sequence: 0,
        }
    }
}

impl<T: Clone> PanelSlot<T> {
    pub fn raise(
        &mut self,
        prompt_id: String,
        prompt: T,
        priority: u8,
        panel_available: bool,
    ) -> Vec<PanelCommand<T>> {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.pending.insert(
            prompt_id.clone(),
            PanelEntry {
                prompt: prompt.clone(),
                priority,
                sequence,
                panel_eligible: panel_available,
            },
        );

        if !panel_available {
            return vec![PanelCommand::PresentFallback { prompt_id, prompt }];
        }

        let Some(owner) = self.owner.as_ref() else {
            return self.promote_next().into_iter().collect();
        };
        let owner_id = owner.prompt_id.clone();
        let Some(owner_priority) = self.pending.get(&owner_id).map(|entry| entry.priority) else {
            self.owner = None;
            return self.promote_next().into_iter().collect();
        };
        if priority <= owner_priority {
            return Vec::new();
        }

        let displaced = self.mark_for_fallback(&owner_id);
        self.owner = None;
        let mut commands = displaced.into_iter().collect::<Vec<_>>();
        if let Some(promote) = self.promote_next() {
            commands.push(promote);
        }
        commands
    }

    pub fn acknowledge(&mut self, prompt_id: &str) -> Vec<PanelCommand<T>> {
        let Some(owner) = self.owner.as_mut() else {
            return Vec::new();
        };
        if owner.prompt_id != prompt_id || owner.acknowledged {
            return Vec::new();
        }
        let Some(entry) = self.pending.get(prompt_id) else {
            return Vec::new();
        };
        owner.acknowledged = true;
        vec![PanelCommand::Acknowledged {
            prompt_id: prompt_id.to_string(),
            prompt: entry.prompt.clone(),
        }]
    }

    pub fn fallback_if_unacknowledged(&mut self, prompt_id: &str) -> Vec<PanelCommand<T>> {
        let should_fallback = self
            .owner
            .as_ref()
            .is_some_and(|owner| owner.prompt_id == prompt_id && !owner.acknowledged);
        if !should_fallback {
            return Vec::new();
        }
        self.owner = None;
        let mut commands = vec![PanelCommand::HidePanel];
        if let Some(fallback) = self.mark_for_fallback(prompt_id) {
            commands.push(fallback);
        }
        if let Some(promote) = self.promote_next() {
            commands.push(promote);
        }
        commands
    }

    pub fn begin_capture(&mut self, should_fallback: impl Fn(&T) -> bool) -> PanelCapture<T> {
        self.owner = None;
        let mut pending = self
            .pending
            .iter()
            .map(|(prompt_id, entry)| (entry.sequence, prompt_id.clone()))
            .collect::<Vec<_>>();
        pending.sort_by_key(|(sequence, _)| *sequence);
        let mut discarded = Vec::new();
        let mut commands = Vec::new();
        for (_, prompt_id) in pending {
            let Some(entry) = self.pending.get(&prompt_id) else {
                continue;
            };
            if should_fallback(&entry.prompt) {
                if entry.panel_eligible {
                    if let Some(command) = self.mark_for_fallback(&prompt_id) {
                        commands.push(command);
                    }
                }
                continue;
            }
            if let Some(entry) = self.pending.remove(&prompt_id) {
                discarded.push((prompt_id.clone(), entry.prompt));
                commands.push(PanelCommand::WithdrawPrompt { prompt_id });
            }
        }
        commands.push(PanelCommand::ShowPanel);
        PanelCapture {
            discarded,
            commands,
        }
    }

    pub fn end_capture(&mut self) -> Vec<PanelCommand<T>> {
        vec![PanelCommand::HidePanel]
    }

    pub fn finish(&mut self, prompt_id: &str) -> PanelFinish<T> {
        let Some(entry) = self.pending.remove(prompt_id) else {
            return PanelFinish {
                removed: None,
                commands: Vec::new(),
            };
        };
        let was_owner = self
            .owner
            .as_ref()
            .is_some_and(|owner| owner.prompt_id == prompt_id);
        let mut commands = vec![PanelCommand::WithdrawPrompt {
            prompt_id: prompt_id.to_string(),
        }];
        if !was_owner {
            return PanelFinish {
                removed: Some(entry.prompt),
                commands,
            };
        }

        self.owner = None;
        commands.push(PanelCommand::HidePanel);
        if let Some(promote) = self.promote_next() {
            commands.push(promote);
        }
        PanelFinish {
            removed: Some(entry.prompt),
            commands,
        }
    }

    fn mark_for_fallback(&mut self, prompt_id: &str) -> Option<PanelCommand<T>> {
        let entry = self.pending.get_mut(prompt_id)?;
        entry.panel_eligible = false;
        Some(PanelCommand::PresentFallback {
            prompt_id: prompt_id.to_string(),
            prompt: entry.prompt.clone(),
        })
    }

    fn promote_next(&mut self) -> Option<PanelCommand<T>> {
        let (prompt_id, prompt) = self
            .pending
            .iter()
            .filter(|(_, entry)| entry.panel_eligible)
            .max_by_key(|(_, entry)| (entry.priority, Reverse(entry.sequence)))
            .map(|(prompt_id, entry)| (prompt_id.clone(), entry.prompt.clone()))?;
        self.owner = Some(PanelOwner {
            prompt_id: prompt_id.clone(),
            acknowledged: false,
        });
        Some(PanelCommand::PresentPanel { prompt_id, prompt })
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &T)> {
        self.pending
            .iter()
            .map(|(prompt_id, entry)| (prompt_id.as_str(), &entry.prompt))
    }

    pub fn get(&self, prompt_id: &str) -> Option<&T> {
        self.pending.get(prompt_id).map(|entry| &entry.prompt)
    }

    pub fn get_mut(&mut self, prompt_id: &str) -> Option<&mut T> {
        self.pending
            .get_mut(prompt_id)
            .map(|entry| &mut entry.prompt)
    }

    #[cfg(test)]
    fn owner_id(&self) -> Option<&str> {
        self.owner.as_ref().map(|owner| owner.prompt_id.as_str())
    }
}

/// The runtime-facing presenter seam. It keeps Tauri windows and
/// `UNUserNotificationCenter` out of the pure slot tests.
pub trait ConsentPromptSurface: Send + Sync {
    fn access(&self) -> NotificationAccess;
    fn request_access(&self) -> NotificationAccessFuture;
    fn show_panel(&self, layout: ConsentPanelLayout) -> bool;
    fn hide_panel(&self);
    fn present_fallback(&self, prompt_id: &str, prompt: &PromptKind) -> DetectionPromptDelivery;
    fn present_ritual_fallback(&self, ritual_id: &str, title: &str) -> DetectionPromptDelivery;
    fn withdraw(&self, prompt_id: &str);
}

pub struct ConsentPromptPresenter {
    app: AppHandle,
    native: Arc<dyn PromptPresenter>,
}

impl ConsentPromptPresenter {
    pub fn new(app: AppHandle, native: Arc<dyn PromptPresenter>) -> Self {
        Self { app, native }
    }
}

impl ConsentPromptSurface for ConsentPromptPresenter {
    fn access(&self) -> NotificationAccess {
        self.native.access()
    }

    fn request_access(&self) -> NotificationAccessFuture {
        self.native.request_access()
    }

    fn show_panel(&self, layout: ConsentPanelLayout) -> bool {
        crate::meeting::consent_panel::show(&self.app, layout)
    }

    fn hide_panel(&self) {
        crate::meeting::consent_panel::hide(&self.app);
    }

    fn present_fallback(&self, prompt_id: &str, prompt: &PromptKind) -> DetectionPromptDelivery {
        if self.native.present(prompt_id, prompt) {
            DetectionPromptDelivery::Notification
        } else {
            DetectionPromptDelivery::InAppOnly
        }
    }

    fn present_ritual_fallback(&self, ritual_id: &str, title: &str) -> DetectionPromptDelivery {
        if self.native.present_ritual(ritual_id, title) {
            DetectionPromptDelivery::Notification
        } else {
            DetectionPromptDelivery::InAppOnly
        }
    }

    fn withdraw(&self, prompt_id: &str) {
        self.native.withdraw(prompt_id);
    }
}

#[cfg(target_os = "macos")]
pub use macos::UserNotificationPrompts;

#[cfg(target_os = "macos")]
mod macos {
    use super::{
        NotificationAccess, NotificationAccessFuture, PromptKind, PromptPresenter, PromptResponder,
        PromptResponse, ACTION_DISMISS, ACTION_START, CATEGORY_DETECTION, CATEGORY_DIGEST,
        CATEGORY_RITUAL, DISMISS_TITLE, START_TITLE,
    };
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, ProtocolObject};
    use objc2::{define_class, msg_send, AnyThread, DefinedClass};
    use objc2_foundation::{
        NSArray, NSBundle, NSError, NSObject, NSObjectProtocol, NSSet, NSString,
    };
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent,
        UNNotificationAction, UNNotificationActionOptions, UNNotificationCategory,
        UNNotificationCategoryOptions, UNNotificationRequest, UNNotificationResponse,
        UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };
    use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;

    const AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(120);
    const ACCESS_NOT_DETERMINED: u8 = 0;
    const ACCESS_AUTHORIZED: u8 = 1;
    const ACCESS_DENIED: u8 = 2;

    fn access_code(status: UNAuthorizationStatus) -> u8 {
        match status {
            UNAuthorizationStatus::NotDetermined => ACCESS_NOT_DETERMINED,
            UNAuthorizationStatus::Denied => ACCESS_DENIED,
            _ => ACCESS_AUTHORIZED,
        }
    }

    fn cached_access(code: u8) -> NotificationAccess {
        match code {
            ACCESS_AUTHORIZED => NotificationAccess::Authorized,
            ACCESS_DENIED => NotificationAccess::Denied,
            _ => NotificationAccess::NotDetermined,
        }
    }

    /// True only for a real `.app` bundle. `currentNotificationCenter` throws
    /// otherwise, and an Objective-C exception crossing back into Rust is
    /// undefined behavior, so this gate must hold before any call below.
    fn is_bundled_application() -> bool {
        objc2::rc::autoreleasepool(|_| {
            let bundle = NSBundle::mainBundle();
            let has_identifier = bundle.bundleIdentifier().is_some();
            let is_app_bundle = bundle.bundlePath().to_string().ends_with(".app");
            has_identifier && is_app_bundle
        })
    }

    pub struct DelegateIvars {
        responder: Arc<dyn PromptResponder>,
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and the single ivar
        // is a reference-counted Rust value with no Objective-C lifecycle.
        #[unsafe(super(NSObject))]
        #[name = "SonaDetectionNotificationDelegate"]
        #[ivars = DelegateIvars]
        pub struct DetectionDelegate;

        impl DetectionDelegate {
            // UNUserNotificationCenterDelegate: the operator clicked an action or
            // the notification body.
            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive_response(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                completion: &block2::DynBlock<dyn Fn()>,
            ) {
                let action = response.actionIdentifier().to_string();
                let request = response.notification().request();
                let prompt_id = request.identifier().to_string();
                let responder = Arc::clone(&self.ivars().responder);
                // Title-only rituals register no action and deliberately do not
                // turn a body click into recording authority.
                let category = request.content().categoryIdentifier().to_string();
                if category == CATEGORY_RITUAL {
                    completion.call(());
                    return;
                }
                // The digest is told apart by its category, not its identifier:
                // the category is what the notification was posted under and
                // cannot drift from the content the way a parsed id would. It
                // registers no actions, so its only gesture is the body.
                if category == CATEGORY_DIGEST {
                    if action == DEFAULT_ACTION {
                        responder.prompt_answered(PromptResponse::DigestOpened);
                    }
                    completion.call(());
                    return;
                }
                // The default action is clicking the notification itself. Treating
                // it as "start" matches how the copy reads: the notification asks
                // whether to transcribe, so acting on it is an affirmative.
                let started = action == ACTION_START || action == DEFAULT_ACTION;
                if started {
                    responder.prompt_answered(PromptResponse::Start { prompt_id });
                } else if action == ACTION_DISMISS || action == DISMISS_ACTION {
                    responder.prompt_answered(PromptResponse::Dismiss { prompt_id });
                }
                completion.call(());
            }

            // Without this, a notification posted while Sona is frontmost is
            // silently dropped by the system.
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &objc2_user_notifications::UNNotification,
                completion: &block2::DynBlock<dyn Fn(usize)>,
            ) {
                // UNNotificationPresentationOptionBanner | ...Sound is 1 << 4 | 1 << 1
                // in the modern SDK; the banner bit alone is what a prompt needs.
                completion.call((PRESENTATION_BANNER,));
            }
        }

        unsafe impl NSObjectProtocol for DetectionDelegate {}

        unsafe impl UNUserNotificationCenterDelegate for DetectionDelegate {}
    );

    /// `UNNotificationDefaultActionIdentifier` — clicking the notification body.
    const DEFAULT_ACTION: &str = "com.apple.UNNotificationDefaultActionIdentifier";
    /// `UNNotificationDismissActionIdentifier` — swiping the notification away.
    const DISMISS_ACTION: &str = "com.apple.UNNotificationDismissActionIdentifier";
    /// `UNNotificationPresentationOptionBanner`.
    const PRESENTATION_BANNER: usize = 1 << 4;

    impl DetectionDelegate {
        fn new(responder: Arc<dyn PromptResponder>) -> Retained<Self> {
            let this = Self::alloc().set_ivars(DelegateIvars { responder });
            // `this` is a freshly allocated DetectionDelegate with valid NSObject
            // ivars. SAFETY: Objective-C initialization retains that ownership.
            unsafe { msg_send![super(this), init] }
        }
    }

    /// Owns the notification center's delegate and category registration, and
    /// creates all three lazily.
    ///
    /// `start` runs inside Tauri's `setup`, before the window and its webview
    /// exist. Nothing in this type may touch UserNotifications there: that is
    /// main-thread framework work in the middle of app launch, and it is the
    /// only code in this slice whose execution differs between a `cargo` build
    /// and a bundled app — so it is also the only code no test has ever run.
    /// Deferring it means the earliest possible touch is the first detection
    /// tick, on the detection thread, well after the window is up.
    pub struct UserNotificationPrompts {
        responder: Arc<dyn PromptResponder>,
        center: OnceLock<CenterBinding>,
        cached_access: Arc<AtomicU8>,
        access_query_in_flight: Arc<AtomicBool>,
    }

    /// The center together with the delegate that must outlive every
    /// notification it delivers.
    struct CenterBinding {
        center: Retained<UNUserNotificationCenter>,
        /// Held for the process lifetime: the center keeps only a weak delegate
        /// reference, and a dropped delegate silently stops every action click.
        _delegate: Retained<DetectionDelegate>,
    }

    // SAFETY: `UNUserNotificationCenter` is documented as thread-safe, the
    // delegate is only read by the Objective-C runtime, and `OnceLock` gives the
    // one-time initialization its own synchronization.
    unsafe impl Send for UserNotificationPrompts {}
    unsafe impl Sync for UserNotificationPrompts {}

    impl UserNotificationPrompts {
        /// `None` when there is no notification center to talk to, which is the
        /// normal state for an unbundled build.
        ///
        /// The bundle check is the only work done here. It reads the main
        /// bundle's identifier and touches no notification API, so this is safe
        /// to call during `setup`.
        pub fn start(responder: Arc<dyn PromptResponder>) -> Option<Self> {
            if !is_bundled_application() {
                return None;
            }
            Some(Self {
                responder,
                center: OnceLock::new(),
                cached_access: Arc::new(AtomicU8::new(ACCESS_NOT_DETERMINED)),
                access_query_in_flight: Arc::new(AtomicBool::new(false)),
            })
        }

        /// Creates the center, binds the delegate, and registers the two actions
        /// once, on whichever call needs it first.
        fn center(&self) -> &UNUserNotificationCenter {
            &self
                .center
                .get_or_init(|| {
                    let center = UNUserNotificationCenter::currentNotificationCenter();
                    let delegate = DetectionDelegate::new(Arc::clone(&self.responder));
                    let protocol_delegate = ProtocolObject::from_ref(&*delegate);
                    center.setDelegate(Some(protocol_delegate));
                    register_categories(&center);
                    CenterBinding {
                        center,
                        _delegate: delegate,
                    }
                })
                .center
        }

        fn refresh_access(&self) {
            if self
                .access_query_in_flight
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return;
            }
            let cached_access = Arc::clone(&self.cached_access);
            let query_in_flight = Arc::clone(&self.access_query_in_flight);
            let completion = RcBlock::new(
                move |settings: core::ptr::NonNull<
                    objc2_user_notifications::UNNotificationSettings,
                >| {
                    // SAFETY: the system hands a live settings object to this block.
                    let status = unsafe { settings.as_ref().authorizationStatus() };
                    cached_access.store(access_code(status), Ordering::Release);
                    query_in_flight.store(false, Ordering::Release);
                },
            );
            self.center()
                .getNotificationSettingsWithCompletionHandler(&completion);
        }
    }

    /// Registers all notification categories in one call.
    ///
    /// `setNotificationCategories` replaces the whole set, so they have to be
    /// declared together — registering them from their own owners would leave
    /// whichever ran last holding the only category the system knows about.
    fn register_categories(center: &UNUserNotificationCenter) {
        objc2::rc::autoreleasepool(|_| {
            // `Foreground` brings Sona forward on click, which is what the
            // affirmative action needs: the next step is a consent screen.
            let start = UNNotificationAction::actionWithIdentifier_title_options(
                &NSString::from_str(ACTION_START),
                &NSString::from_str(START_TITLE),
                UNNotificationActionOptions::Foreground,
            );
            let dismiss = UNNotificationAction::actionWithIdentifier_title_options(
                &NSString::from_str(ACTION_DISMISS),
                &NSString::from_str(DISMISS_TITLE),
                UNNotificationActionOptions::empty(),
            );
            let actions = NSArray::from_retained_slice(&[start, dismiss]);
            let intents: Retained<NSArray<NSString>> = NSArray::new();
            let detection =
                UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                    &NSString::from_str(CATEGORY_DETECTION),
                    &actions,
                    &intents,
                    UNNotificationCategoryOptions::empty(),
                );
            // No actions: the digest has one body-click destination, while a
            // ritual fallback is informational only.
            let no_actions: Retained<NSArray<UNNotificationAction>> = NSArray::new();
            let digest =
                UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                    &NSString::from_str(CATEGORY_DIGEST),
                    &no_actions,
                    &intents,
                    UNNotificationCategoryOptions::empty(),
                );
            let ritual =
                UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                    &NSString::from_str(CATEGORY_RITUAL),
                    &no_actions,
                    &intents,
                    UNNotificationCategoryOptions::empty(),
                );
            let categories = NSSet::from_retained_slice(&[detection, digest, ritual]);
            center.setNotificationCategories(&categories);
        });
    }

    impl PromptPresenter for UserNotificationPrompts {
        fn access(&self) -> NotificationAccess {
            self.refresh_access();
            cached_access(self.cached_access.load(Ordering::Acquire))
        }

        fn request_access(&self) -> NotificationAccessFuture {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let sender = Arc::new(Mutex::new(Some(sender)));
            let cached_access = Arc::clone(&self.cached_access);
            let completion = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
                let access = if granted.as_bool() {
                    NotificationAccess::Authorized
                } else {
                    NotificationAccess::Denied
                };
                cached_access.store(
                    if granted.as_bool() {
                        ACCESS_AUTHORIZED
                    } else {
                        ACCESS_DENIED
                    },
                    Ordering::Release,
                );
                let sender = sender
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take();
                if let Some(sender) = sender {
                    let _ = sender.send(access);
                }
            });
            self.center()
                .requestAuthorizationWithOptions_completionHandler(
                    UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                    &completion,
                );
            Box::pin(async move {
                match tokio::time::timeout(AUTHORIZATION_TIMEOUT, receiver).await {
                    Ok(Ok(access)) => access,
                    Ok(Err(_)) | Err(_) => NotificationAccess::NotDetermined,
                }
            })
        }

        fn present(&self, prompt_id: &str, prompt: &PromptKind) -> bool {
            if self.access() != NotificationAccess::Authorized {
                return false;
            }
            objc2::rc::autoreleasepool(|_| {
                let content = UNMutableNotificationContent::new();
                content.setTitle(&NSString::from_str(&prompt.notification_title()));
                content.setBody(&NSString::from_str(PROMPT_BODY));
                content.setCategoryIdentifier(&NSString::from_str(CATEGORY_DETECTION));
                let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
                    &NSString::from_str(prompt_id),
                    &content,
                    None,
                );
                self.center()
                    .addNotificationRequest_withCompletionHandler(&request, None);
                true
            })
        }

        fn withdraw(&self, prompt_id: &str) {
            objc2::rc::autoreleasepool(|_| {
                let identifiers = NSArray::from_retained_slice(&[NSString::from_str(prompt_id)]);
                self.center()
                    .removePendingNotificationRequestsWithIdentifiers(&identifiers);
                self.center()
                    .removeDeliveredNotificationsWithIdentifiers(&identifiers);
            });
        }

        fn present_digest(&self, title: &str, body: &str) -> bool {
            if self.access() != NotificationAccess::Authorized {
                return false;
            }
            objc2::rc::autoreleasepool(|_| {
                let content = UNMutableNotificationContent::new();
                content.setTitle(&NSString::from_str(title));
                content.setBody(&NSString::from_str(body));
                content.setCategoryIdentifier(&NSString::from_str(CATEGORY_DIGEST));
                // The category doubles as the request identifier, so a second
                // digest replaces the first instead of stacking beside it.
                let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
                    &NSString::from_str(CATEGORY_DIGEST),
                    &content,
                    None,
                );
                self.center()
                    .addNotificationRequest_withCompletionHandler(&request, None);
                true
            })
        }

        fn present_ritual(&self, ritual_id: &str, title: &str) -> bool {
            if self.access() != NotificationAccess::Authorized {
                return false;
            }
            objc2::rc::autoreleasepool(|_| {
                let content = UNMutableNotificationContent::new();
                content.setTitle(&NSString::from_str(title));
                content.setCategoryIdentifier(&NSString::from_str(CATEGORY_RITUAL));
                let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
                    &NSString::from_str(ritual_id),
                    &content,
                    None,
                );
                self.center()
                    .addNotificationRequest_withCompletionHandler(&request, None);
                true
            })
        }
    }

    /// One line, so the two buttons carry the decision.
    const PROMPT_BODY: &str = "Sona can take local notes for this call.";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_priority_prompt_waits_without_displacing_panel_owner() {
        let mut slot = PanelSlot::default();
        slot.raise("calendar".to_string(), "calendar", 3, true);

        let commands = slot.raise("browser".to_string(), "browser", 1, true);

        assert!(commands.is_empty());
        assert_eq!(slot.owner_id(), Some("calendar"));
    }

    #[test]
    fn higher_priority_prompt_displaces_the_owner_to_fallback() {
        let mut slot = PanelSlot::default();
        slot.raise("browser".to_string(), "browser", 1, true);

        let commands = slot.raise("calendar".to_string(), "calendar", 3, true);

        assert_eq!(
            commands,
            vec![
                PanelCommand::PresentFallback {
                    prompt_id: "browser".to_string(),
                    prompt: "browser",
                },
                PanelCommand::PresentPanel {
                    prompt_id: "calendar".to_string(),
                    prompt: "calendar",
                },
            ]
        );
        assert_eq!(slot.owner_id(), Some("calendar"));
    }

    #[test]
    fn displaced_prompt_is_not_promoted_after_fallback() {
        let mut slot = PanelSlot::default();
        slot.raise("browser".to_string(), "browser", 1, true);
        slot.raise("calendar".to_string(), "calendar", 3, true);

        let finish = slot.finish("calendar");

        assert_eq!(
            finish.commands,
            vec![
                PanelCommand::WithdrawPrompt {
                    prompt_id: "calendar".to_string(),
                },
                PanelCommand::HidePanel,
            ]
        );
        assert_eq!(slot.owner_id(), None);
    }

    #[test]
    fn finishing_the_owner_promotes_the_highest_priority_waiter() {
        let mut slot = PanelSlot::default();
        slot.raise("calendar".to_string(), "calendar", 3, true);
        slot.raise("browser".to_string(), "browser", 1, true);
        slot.raise("app".to_string(), "app", 2, true);

        let finish = slot.finish("calendar");

        assert_eq!(finish.removed, Some("calendar"));
        assert_eq!(
            finish.commands,
            vec![
                PanelCommand::WithdrawPrompt {
                    prompt_id: "calendar".to_string(),
                },
                PanelCommand::HidePanel,
                PanelCommand::PresentPanel {
                    prompt_id: "app".to_string(),
                    prompt: "app",
                },
            ]
        );
        assert_eq!(slot.owner_id(), Some("app"));
    }

    #[test]
    fn retracting_a_queued_prompt_leaves_the_owner_unchanged() {
        let mut slot = PanelSlot::default();
        slot.raise("calendar".to_string(), "calendar", 3, true);
        slot.raise("browser".to_string(), "browser", 1, true);

        let finish = slot.finish("browser");

        assert_eq!(finish.removed, Some("browser"));
        assert_eq!(
            finish.commands,
            vec![PanelCommand::WithdrawPrompt {
                prompt_id: "browser".to_string(),
            }]
        );
        assert_eq!(slot.owner_id(), Some("calendar"));
    }

    #[test]
    fn finishing_the_last_owner_drains_and_hides_the_panel() {
        let mut slot = PanelSlot::default();
        slot.raise("calendar".to_string(), "calendar", 3, true);

        let finish = slot.finish("calendar");

        assert_eq!(finish.removed, Some("calendar"));
        assert_eq!(
            finish.commands,
            vec![
                PanelCommand::WithdrawPrompt {
                    prompt_id: "calendar".to_string(),
                },
                PanelCommand::HidePanel,
            ]
        );
        assert_eq!(slot.owner_id(), None);
    }

    #[test]
    fn missing_ack_falls_back_and_promotes_without_blocking_a_thread() {
        let mut slot = PanelSlot::default();
        slot.raise("calendar".to_string(), "calendar", 3, true);
        slot.raise("browser".to_string(), "browser", 1, true);

        let commands = slot.fallback_if_unacknowledged("calendar");

        assert_eq!(
            commands,
            vec![
                PanelCommand::HidePanel,
                PanelCommand::PresentFallback {
                    prompt_id: "calendar".to_string(),
                    prompt: "calendar",
                },
                PanelCommand::PresentPanel {
                    prompt_id: "browser".to_string(),
                    prompt: "browser",
                },
            ]
        );
        assert_eq!(slot.owner_id(), Some("browser"));
    }

    #[test]
    fn acknowledgement_cancels_the_fallback_transition() {
        let mut slot = PanelSlot::default();
        slot.raise("calendar".to_string(), "calendar", 3, true);

        assert_eq!(
            slot.acknowledge("calendar"),
            vec![PanelCommand::Acknowledged {
                prompt_id: "calendar".to_string(),
                prompt: "calendar",
            }]
        );
        assert!(slot.fallback_if_unacknowledged("calendar").is_empty());
    }

    #[test]
    fn capture_displaces_acknowledged_and_queued_prompts_to_fallback() {
        let mut slot = PanelSlot::default();
        slot.raise("calendar".to_string(), "calendar", 3, true);
        slot.acknowledge("calendar");
        slot.raise("browser".to_string(), "browser", 1, true);

        let capture = slot.begin_capture(|_| true);
        assert_eq!(
            capture.commands,
            vec![
                PanelCommand::PresentFallback {
                    prompt_id: "calendar".to_string(),
                    prompt: "calendar",
                },
                PanelCommand::PresentFallback {
                    prompt_id: "browser".to_string(),
                    prompt: "browser",
                },
                PanelCommand::ShowPanel,
            ]
        );
        assert!(capture.discarded.is_empty());
        assert_eq!(slot.owner_id(), None);
    }

    #[test]
    fn capture_discards_rituals_instead_of_notifying_over_recording() {
        let mut slot = PanelSlot::default();
        slot.raise("prompt".to_string(), "prompt", 10, true);
        slot.raise("wrap".to_string(), "wrap", 1, true);

        let capture = slot.begin_capture(|pending| *pending == "prompt");

        assert_eq!(capture.discarded, vec![("wrap".to_string(), "wrap")]);
        assert!(capture.commands.contains(&PanelCommand::PresentFallback {
            prompt_id: "prompt".to_string(),
            prompt: "prompt",
        }));
        assert!(!capture.commands.contains(&PanelCommand::PresentFallback {
            prompt_id: "wrap".to_string(),
            prompt: "wrap",
        }));
    }

    #[test]
    fn ending_a_tracked_capture_hides_the_panel() {
        let mut slot = PanelSlot::<&str>::default();

        assert_eq!(slot.end_capture(), vec![PanelCommand::HidePanel]);
    }
}

use crate::meeting::detection::calendar::{platform_calendar, CalendarAccess};

/// The calendar this app writes its events into. Named, not configurable, the
/// way Reminders get a "Sona" list: one place to find, hide or delete
/// everything Sona added.
#[cfg(target_os = "macos")]
const SONA_CALENDAR: &str = "Sona";

pub(crate) async fn request_access() -> Result<(), String> {
    let access = tauri::async_runtime::spawn_blocking(|| platform_calendar().request_access())
        .await
        .map_err(|_| "Calendar permission could not be requested.")?;
    if access == CalendarAccess::Authorized {
        Ok(())
    } else {
        Err("Allow calendar access in System Settings before enabling calendar actions.".into())
    }
}

pub(crate) async fn create(
    title: &str,
    start: i64,
    end: i64,
    notes: &str,
    location: &str,
) -> Result<String, String> {
    let (title, notes, location) = (title.to_owned(), notes.to_owned(), location.to_owned());
    tauri::async_runtime::spawn_blocking(move || {
        create_on_thread(&title, start, end, &notes, &location)
    })
    .await
    .map_err(|_| "The calendar request was interrupted.".to_owned())?
}

#[cfg(target_os = "macos")]
fn unix_seconds(milliseconds: i64) -> f64 {
    // Duration performs the floating-point rounding; unsigned_abs handles i64::MIN.
    let seconds = std::time::Duration::from_millis(milliseconds.unsigned_abs()).as_secs_f64();
    if milliseconds < 0 {
        -seconds
    } else {
        seconds
    }
}

#[cfg(target_os = "macos")]
fn create_on_thread(
    title: &str,
    start: i64,
    end: i64,
    notes: &str,
    location: &str,
) -> Result<String, String> {
    use objc2_event_kit::{EKEvent, EKEventStore, EKSpan};
    use objc2_foundation::{NSDate, NSString};
    if platform_calendar().access() != CalendarAccess::Authorized {
        return Err("Allow calendar access in System Settings. No event was created.".into());
    }
    // The calendar and event belong to this store; setters copy or retain their arguments.
    // SAFETY: EventKit objects stay on this blocking thread and borrowed arguments live through each call.
    unsafe {
        let store = EKEventStore::new();
        let calendar = sona_calendar(&store)
            .ok_or("Sona could not find or make its \"Sona\" calendar. No event was created.")?;
        let event = EKEvent::eventWithEventStore(&store);
        event.setCalendar(Some(&calendar));
        event.setTitle(Some(&NSString::from_str(title)));
        event.setStartDate(Some(&NSDate::dateWithTimeIntervalSince1970(unix_seconds(
            start,
        ))));
        event.setEndDate(Some(&NSDate::dateWithTimeIntervalSince1970(unix_seconds(
            end,
        ))));
        event.setNotes(Some(&NSString::from_str(notes)));
        event.setLocation(Some(&NSString::from_str(location)));
        store
            .saveEvent_span_error(&event, EKSpan::ThisEvent)
            .map_err(|_| {
                "Calendar refused the event. Check that the \"Sona\" calendar is writable."
            })?;
        event.eventIdentifier().map(|id|id.to_string()).ok_or("Calendar saved the event without an identifier. Check Calendar before creating it again.".into())
    }
}

/// The "Sona" calendar, created on first use in the account that holds the
/// default calendar.
///
/// `None` when it neither exists nor can be made, which fails the action
/// rather than writing into whatever calendar happens to be the default.
#[cfg(target_os = "macos")]
fn sona_calendar(
    store: &objc2_event_kit::EKEventStore,
) -> Option<objc2::rc::Retained<objc2_event_kit::EKCalendar>> {
    use objc2_event_kit::{EKCalendar, EKEntityType};
    use objc2_foundation::NSString;
    // SAFETY: a plain query for this store's event calendars.
    let existing = unsafe { store.calendarsForEntityType(EKEntityType::Event) };
    for calendar in existing.iter() {
        // SAFETY: plain property read on a live calendar.
        if unsafe { calendar.title() }.to_string() == SONA_CALENDAR {
            return Some(calendar);
        }
    }
    // Homed on the default calendar's source so it sits beside the operator's own.
    // SAFETY: a new calendar on this store; every argument outlives its call.
    unsafe {
        let calendar = EKCalendar::calendarForEntityType_eventStore(EKEntityType::Event, store);
        calendar.setTitle(&NSString::from_str(SONA_CALENDAR));
        calendar.setSource(store.defaultCalendarForNewEvents()?.source().as_deref());
        store
            .saveCalendar_commit_error(&calendar, true)
            .ok()
            .map(|()| calendar)
    }
}

#[cfg(not(target_os = "macos"))]
fn create_on_thread(
    _title: &str,
    _start: i64,
    _end: i64,
    _notes: &str,
    _location: &str,
) -> Result<String, String> {
    Err("Calendar actions are available on macOS.".into())
}

pub(crate) async fn undo(id: &str) -> Result<(), String> {
    let id = id.to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "macos")]
        {
            use objc2_event_kit::{EKEventStore, EKSpan};
            use objc2_foundation::NSString;
            if platform_calendar().access() != CalendarAccess::Authorized {
                return Err("Allow calendar access before removing this event.".into());
            }
            // The event is fetched from the same store that removes it.
            // SAFETY: EventKit objects stay on this blocking thread and the identifier lives through the lookup.
            unsafe {
                let store = EKEventStore::new();
                if let Some(event) = store.eventWithIdentifier(&NSString::from_str(&id)) {
                    store
                        .removeEvent_span_error(&event, EKSpan::ThisEvent)
                        .map_err(|_| "Calendar could not remove this event.")?;
                }
                Ok(())
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = id;
            Err("Calendar actions are available on macOS.".into())
        }
    })
    .await
    .map_err(|_| "The calendar request was interrupted.".to_owned())?
}

//! Mail is an opt-in, local-only context source. The bridge never sends mail.
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MailContextState {
    Off,
    Ready,
    Empty,
    NotConfigured,
    PermissionDenied,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MailThread {
    pub subject: String,
    pub sender: String,
    pub received_at: String,
    pub excerpt: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MailContext {
    pub state: MailContextState,
    pub threads: Vec<MailThread>,
}

impl MailContext {
    pub fn off() -> Self {
        Self {
            state: MailContextState::Off,
            threads: Vec::new(),
        }
    }

    pub fn status_text(&self) -> &'static str {
        match self.state {
            MailContextState::Off => "Email context is off. You can enable it in Meeting settings.",
            MailContextState::Ready => {
                "Recent email from Mail is included. Email stays on this Mac."
            }
            MailContextState::Empty => "No recent email with these attendees was found in Mail.",
            MailContextState::NotConfigured => "Add an account in Mail to include recent email.",
            MailContextState::PermissionDenied => {
                "Allow Sona to control Mail in System Settings > Privacy & Security > Automation."
            }
            MailContextState::Unavailable => "Mail could not be read. Open Mail and try again.",
        }
    }
}

/// Data is passed as an argument, never interpolated into executable script.
const READ_SCRIPT: &str = r#"
function run(argv) {
    const input = JSON.parse(argv[0]);
    const addresses = new Set(input.addresses.map(x => x.trim().toLowerCase()));
    const mail = Application('com.apple.mail');
    try {
        if (mail.accounts().length === 0) return JSON.stringify({state:'not_configured', threads:[]});
        if (input.probe || addresses.size === 0) return JSON.stringify({state:'empty', threads:[]});
        const since = new Date(input.since);
        const until = new Date(input.until);
        const rows = [];
        const seen = new Set();
        const boxes = [mail.inbox, mail.sentMailbox];
        function address(text) {
            const found = /<([^>]+)>/.exec(text);
            return (found ? found[1] : text).trim().toLowerCase();
        }
        for (const box of boxes) {
            const messages = box.messages.whose({dateReceived:{_greaterThan:since}})();
            for (let index = 0; index < Math.min(messages.length, 200); index++) {
                const message = messages[index];
                const date = message.dateReceived();
                if (date > until) continue;
                const sender = message.sender();
                const recipients = message.toRecipients.address();
                if (!addresses.has(address(sender)) && !recipients.some(x => addresses.has(address(x)))) continue;
                const key = message.messageId();
                if (seen.has(key)) continue;
                seen.add(key);
                rows.push({subject:message.subject().slice(0,240), sender:sender.slice(0,240), received_at:date.toISOString(), excerpt:message.content().slice(0,1200)});
            }
        }
        rows.sort((a,b) => b.received_at.localeCompare(a.received_at));
        const threads = rows.slice(0,12);
        return JSON.stringify({state:threads.length ? 'ready' : 'empty', threads:threads});
    } catch (error) {
        return JSON.stringify({state:Number(error.errorNumber) === -1743 ? 'permission_denied' : 'unavailable', threads:[]});
    }
}
"#;

const COMPOSE_SCRIPT: &str = r#"
function run(argv) {
    const input = JSON.parse(argv[0]);
    const mail = Application('com.apple.mail');
    try {
        if (mail.accounts().length === 0) return JSON.stringify({state:'not_configured', threads:[]});
        const draft = mail.OutgoingMessage({subject:input.subject, content:input.body, visible:true});
        mail.outgoingMessages.push(draft);
        for (const address of input.recipients) draft.toRecipients.push(mail.ToRecipient({address:address}));
        mail.activate();
        return JSON.stringify({state:'ready', threads:[]});
    } catch (error) {
        return JSON.stringify({state:Number(error.errorNumber) === -1743 ? 'permission_denied' : 'unavailable', threads:[]});
    }
}
"#;

/// What the read script receives. With `probe` set it only checks that Mail
/// has an account and that Sona may ask it.
#[derive(Serialize)]
struct ReadRequest<'a> {
    addresses: &'a [String],
    since: i64,
    until: i64,
    probe: bool,
}

#[derive(Serialize)]
struct ComposeRequest<'a> {
    recipients: &'a [String],
    subject: &'a str,
    body: &'a str,
}

async fn run(script: &str, input: &impl Serialize) -> MailContext {
    #[cfg(target_os = "macos")]
    if let Ok(input) = serde_json::to_string(input) {
        let child = tokio::process::Command::new("/usr/bin/osascript")
            .args(["-l", "JavaScript", "-e", script])
            .arg(input)
            .kill_on_drop(true)
            .output();
        if let Ok(Ok(output)) =
            tokio::time::timeout(std::time::Duration::from_secs(30), child).await
        {
            if output.status.success() {
                if let Ok(context) = serde_json::from_slice(&output.stdout) {
                    return context;
                }
            }
            // osascript can report TCC denial before the JavaScript handler runs.
            if String::from_utf8_lossy(&output.stderr).contains("-1743") {
                return MailContext {
                    state: MailContextState::PermissionDenied,
                    threads: Vec::new(),
                };
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (script, input);
    MailContext {
        state: MailContextState::Unavailable,
        threads: Vec::new(),
    }
}

pub(crate) async fn read(enabled: bool, addresses: &[String], until_utc_ms: i64) -> MailContext {
    if !enabled {
        return MailContext::off();
    }
    run(
        READ_SCRIPT,
        &ReadRequest {
            addresses,
            since: until_utc_ms.saturating_sub(30 * 24 * 60 * 60_000),
            until: until_utc_ms,
            probe: false,
        },
    )
    .await
}

pub(crate) async fn probe() -> MailContext {
    run(
        READ_SCRIPT,
        &ReadRequest {
            addresses: &[],
            since: 0,
            until: 0,
            probe: true,
        },
    )
    .await
}

pub(crate) async fn compose(recipients: &[String], subject: &str, body: &str) -> MailContext {
    run(
        COMPOSE_SCRIPT,
        &ComposeRequest {
            recipients,
            subject,
            body,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mail_denial_is_a_readable_state_not_empty_mail() {
        let context: MailContext =
            serde_json::from_str(r#"{"state":"permission_denied","threads":[]}"#).unwrap();
        assert_eq!(context.state, MailContextState::PermissionDenied);
        assert!(context.status_text().contains("Automation"));
    }

    #[test]
    fn unconfigured_mail_does_not_claim_a_successful_read() {
        let context: MailContext =
            serde_json::from_str(r#"{"state":"not_configured","threads":[]}"#).unwrap();
        assert_eq!(context.state, MailContextState::NotConfigured);
        assert!(context.status_text().contains("Add an account"));
    }

    #[test]
    fn disabled_reader_returns_without_platform_access() {
        let result = tauri::async_runtime::block_on(read(false, &["guest@example.com".into()], 0));
        assert_eq!(result, MailContext::off());
    }
}

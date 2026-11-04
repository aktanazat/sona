use super::{bump_people_revision_in, person_by_id_in, require_people_revision_in, SCHEMA_VERSION};
use crate::meeting::people_types::{
    PeopleMutationResult, PersonContactBirthday, PersonContactCard, PersonContactSetRequest,
    PersonContactValue, PersonId,
};
use crate::meeting::store::{MeetingStore, StoreError};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

const MAX_VALUES: usize = 16;
const MAX_LINE_CHARS: usize = 256;
const MAX_PHONE_CHARS: usize = 64;
const MAX_LABEL_CHARS: usize = 64;
const MAX_ADDRESS_CHARS: usize = 512;
const MAX_URL_BYTES: usize = 2048;

impl MeetingStore {
    pub(crate) fn set_person_contact_card(
        &self,
        request: PersonContactSetRequest,
        now_utc_ms: i64,
    ) -> Result<PeopleMutationResult, StoreError> {
        let card_json = request
            .contact
            .as_ref()
            .map(|card| {
                if !valid_card(card) {
                    return Err(StoreError::Invalid);
                }
                serde_json::to_string(card).map_err(|_| StoreError::Invalid)
            })
            .transpose()?;
        let person_id = request.person_id.uuid().to_string();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_people_revision_in(&transaction, request.expected_revision)?;
        person_by_id_in(&transaction, request.person_id)?;
        match card_json {
            Some(card_json) => transaction.execute(
                "INSERT INTO person_contact_cards (person_id, card_json, updated_at_utc_ms)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(person_id) DO UPDATE SET
                     card_json = excluded.card_json, updated_at_utc_ms = excluded.updated_at_utc_ms",
                params![person_id, card_json, now_utc_ms],
            )?,
            None => transaction.execute(
                "DELETE FROM person_contact_cards WHERE person_id = ?1",
                [&person_id],
            )?,
        };
        transaction.execute(
            "UPDATE persons SET updated_at_utc_ms = ?1 WHERE id = ?2",
            params![now_utc_ms, person_id],
        )?;
        let revision = bump_people_revision_in(&transaction)?;
        let person = person_by_id_in(&transaction, request.person_id)?;
        transaction.commit()?;
        Ok(PeopleMutationResult {
            schema_version: SCHEMA_VERSION,
            revision,
            person: Some(person),
            removed: false,
        })
    }
}

pub(super) fn contact_card_in(
    connection: &Connection,
    person_id: PersonId,
) -> Result<Option<PersonContactCard>, StoreError> {
    connection
        .query_row(
            "SELECT card_json FROM person_contact_cards WHERE person_id = ?1",
            [person_id.uuid().to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|json| serde_json::from_str(&json).map_err(|_| StoreError::Corrupt))
        .transpose()
}

fn valid_card(card: &PersonContactCard) -> bool {
    let has_detail = !card.emails.is_empty()
        || !card.phones.is_empty()
        || card.organization.is_some()
        || card.job_title.is_some()
        || !card.addresses.is_empty()
        || !card.urls.is_empty()
        || card.birthday.is_some();
    has_detail
        && line(&card.name, MAX_LINE_CHARS)
        && card
            .organization
            .as_deref()
            .is_none_or(|text| line(text, MAX_LINE_CHARS))
        && card
            .job_title
            .as_deref()
            .is_none_or(|text| line(text, MAX_LINE_CHARS))
        && values(&card.emails, email)
        && values(&card.phones, |text| line(text, MAX_PHONE_CHARS))
        && values(&card.addresses, address)
        && values(&card.urls, web_address)
        && card.birthday.is_none_or(birthday)
}

fn values(values: &[PersonContactValue], valid: impl Fn(&str) -> bool) -> bool {
    values.len() <= MAX_VALUES
        && values.iter().all(|value| {
            valid(&value.value)
                && value
                    .label
                    .as_deref()
                    .is_none_or(|label| line(label, MAX_LABEL_CHARS))
        })
}

fn line(text: &str, max_chars: usize) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= max_chars
        && !text.chars().any(char::is_control)
}

fn email(text: &str) -> bool {
    line(text, MAX_LINE_CHARS) && text.contains('@') && !text.contains(char::is_whitespace)
}

fn address(text: &str) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= MAX_ADDRESS_CHARS
        && !text
            .chars()
            .any(|character| character.is_control() && character != '\n')
}

fn web_address(text: &str) -> bool {
    text.len() <= MAX_URL_BYTES
        && url::Url::parse(text).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
        })
}

fn birthday(birthday: PersonContactBirthday) -> bool {
    let year = birthday.year.unwrap_or(2000);
    (1..=9999).contains(&year)
        && chrono::NaiveDate::from_ymd_opt(year, u32::from(birthday.month), u32::from(birthday.day))
            .is_some()
}

#[cfg(test)]
mod tests {
    use crate::meeting::people_types::{
        PersonContactBirthday, PersonContactCard, PersonContactSetRequest, PersonContactValue,
        PersonId,
    };
    use crate::meeting::store::workflow_core_tests::{person, store};
    use crate::meeting::store::{MeetingStore, StoreError};

    fn value(label: Option<&str>, value: &str) -> PersonContactValue {
        PersonContactValue {
            label: label.map(str::to_string),
            value: value.to_string(),
        }
    }

    fn card() -> PersonContactCard {
        PersonContactCard {
            name: "Noah Sample".to_string(),
            emails: vec![value(Some("work"), "noah@example.com")],
            phones: vec![value(Some("mobile"), "+1 555 0100")],
            organization: Some("Sample studio".to_string()),
            job_title: Some("Design lead".to_string()),
            addresses: vec![value(Some("work"), "1 Example Way\nSpringfield")],
            urls: vec![value(None, "https://example.com/noah")],
            birthday: Some(PersonContactBirthday {
                month: 2,
                day: 29,
                year: None,
            }),
        }
    }

    fn attach(
        store: &MeetingStore,
        person_id: PersonId,
        contact: Option<PersonContactCard>,
    ) -> Result<u64, StoreError> {
        let expected_revision = store.person_detail(person_id)?.revision;
        store
            .set_person_contact_card(
                PersonContactSetRequest {
                    person_id,
                    expected_revision,
                    contact,
                },
                10,
            )
            .map(|result| result.revision)
    }

    #[test]
    fn an_attached_card_is_on_the_person_page_until_detached() {
        let (_directory, store) = store();
        let noah = person(&store, "Noah", &[], &[]);

        attach(&store, noah, Some(card())).unwrap();
        assert_eq!(
            store.person_detail(noah).unwrap().detail.contact,
            Some(card())
        );

        attach(&store, noah, None).unwrap();
        assert_eq!(store.person_detail(noah).unwrap().detail.contact, None);
    }

    #[test]
    fn a_write_at_a_stale_revision_is_refused_and_stores_nothing() {
        let (_directory, store) = store();
        let noah = person(&store, "Noah", &[], &[]);
        let stale = store.person_detail(noah).unwrap().revision;
        attach(&store, noah, None).unwrap();

        let refused = store.set_person_contact_card(
            PersonContactSetRequest {
                person_id: noah,
                expected_revision: stale,
                contact: Some(card()),
            },
            10,
        );

        assert_eq!(refused.err(), Some(StoreError::Conflict));
        assert_eq!(store.person_detail(noah).unwrap().detail.contact, None);
    }

    #[test]
    fn a_card_that_is_empty_or_unsafe_is_refused() {
        let (_directory, store) = store();
        let noah = person(&store, "Noah", &[], &[]);
        let cases: [(&str, fn(&mut PersonContactCard)); 7] = [
            ("nothing beyond the name", |card| {
                card.emails.clear();
                card.phones.clear();
                card.organization = None;
                card.job_title = None;
                card.addresses.clear();
                card.urls.clear();
                card.birthday = None;
            }),
            ("a control character in the name", |card| {
                card.name = "Noah\u{7}".to_string()
            }),
            ("a script link", |card| {
                card.urls[0].value = "javascript:alert(1)".to_string()
            }),
            ("a file link", |card| {
                card.urls[0].value = "file:///etc/hosts".to_string()
            }),
            ("an email without an at sign", |card| {
                card.emails[0].value = "noah.example.com".to_string()
            }),
            ("a day the month does not have", |card| {
                card.birthday = Some(PersonContactBirthday {
                    month: 2,
                    day: 30,
                    year: None,
                })
            }),
            ("more values than a card holds", |card| {
                card.phones = vec![card.phones[0].clone(); 17]
            }),
        ];

        let accepted: Vec<&str> = cases
            .into_iter()
            .filter_map(|(case, change)| {
                let mut contact = card();
                change(&mut contact);
                (attach(&store, noah, Some(contact)).err() != Some(StoreError::Invalid))
                    .then_some(case)
            })
            .collect();

        assert_eq!(accepted, Vec::<&str>::new());
        assert_eq!(store.person_detail(noah).unwrap().detail.contact, None);
    }

    #[test]
    fn merging_carries_the_card_to_a_person_without_one() {
        let (_directory, store) = store();
        let source = person(&store, "Noah S", &[], &[]);
        let target = person(&store, "Noah Sample", &[], &[]);
        attach(&store, source, Some(card())).unwrap();
        let revision = store.person_detail(target).unwrap().revision;

        store
            .merge_persons_with_voice_resolution(source, target, revision, None, 20)
            .unwrap();

        assert_eq!(
            store.person_detail(target).unwrap().detail.contact,
            Some(card())
        );
    }
}

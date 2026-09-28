//! Mapping uses only observed speaking intervals on the recording clock.
//! A roster entry alone, a selected tile, and an expired observation are not speech.

use std::collections::HashMap;

pub(crate) struct SpeechSpan {
    pub speaker: usize,
    pub start_ns: u64,
    pub end_ns: u64,
}

pub(crate) struct ActiveSpan<'a> {
    pub participant: &'a str,
    pub start_ns: u64,
    pub end_ns: u64,
}

pub(crate) struct NamedParticipant<'a> {
    pub id: &'a str,
    pub name: &'a str,
}

#[derive(Debug, PartialEq)]
pub(crate) struct NameMatch<'a> {
    pub speaker: usize,
    pub name: &'a str,
    pub overlap_ns: u64,
    pub speech_ns: u64,
}

/// Revisions protect even a manually typed name that looks like a generated label.
/// A remembered voice wins even when its display name happens to look generated.
pub(crate) fn may_name(name: &str, revision: u64, voice_matched: bool) -> bool {
    revision == 0
        && !voice_matched
        && (name == "Unknown speaker"
            || name.strip_prefix("Speaker ").is_some_and(|number| {
                !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
            }))
}

/// Inputs are chronological, non-overlapping observations. A participant's stable
/// identity, not roster position or name similarity, carries a rename forward.
/// At least 80% of a voice's speech and one second must agree on one participant.
/// Conflicting voices cannot both claim a participant or an indistinguishable name.
pub(crate) fn match_names<'a>(
    speech: &[SpeechSpan],
    active: &[ActiveSpan<'a>],
    roster: &[NamedParticipant<'a>],
    eligible: &[bool],
) -> Vec<NameMatch<'a>> {
    let names: HashMap<_, _> = roster
        .iter()
        .map(|person| (person.id, person.name))
        .collect();
    let mut totals = vec![0_u64; eligible.len()];
    let mut overlaps: Vec<HashMap<&str, u64>> = vec![HashMap::new(); eligible.len()];
    for segment in speech {
        let Some(total) = totals.get_mut(segment.speaker) else {
            continue;
        };
        let duration = segment.end_ns.saturating_sub(segment.start_ns);
        *total = total.saturating_add(duration);
        let first = active.partition_point(|span| span.end_ns <= segment.start_ns);
        for span in active[first..]
            .iter()
            .take_while(|span| span.start_ns < segment.end_ns)
        {
            let overlap = span
                .end_ns
                .min(segment.end_ns)
                .saturating_sub(span.start_ns.max(segment.start_ns));
            let value = overlaps[segment.speaker]
                .entry(span.participant)
                .or_default();
            *value = value.saturating_add(overlap);
        }
    }
    // Include protected voices in the conflict check. Otherwise a second voice
    // could claim somebody already named manually or matched by their voice.
    let mut candidates = Vec::new();
    for (speaker, counts) in overlaps.iter().enumerate() {
        let Some((&participant, &overlap_ns)) = counts.iter().max_by_key(|(_, ns)| *ns) else {
            continue;
        };
        let speech_ns = totals[speaker];
        if overlap_ns < 1_000_000_000
            || u128::from(overlap_ns) * 5 < u128::from(speech_ns) * 4
            || counts
                .iter()
                .any(|(id, ns)| *id != participant && *ns == overlap_ns)
        {
            continue;
        }
        let Some(&name) = names.get(participant) else {
            continue;
        };
        if name.is_empty()
            || roster
                .iter()
                .any(|person| person.id != participant && person.name == name)
        {
            continue;
        }
        candidates.push((
            participant,
            NameMatch {
                speaker,
                name,
                overlap_ns,
                speech_ns,
            },
        ));
    }
    let mut claims = HashMap::new();
    for (participant, _) in &candidates {
        *claims.entry(*participant).or_insert(0_usize) += 1;
    }
    candidates
        .into_iter()
        .filter(|(participant, matched)| eligible[matched.speaker] && claims[participant] == 1)
        .map(|(_, matched)| matched)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speech() -> [SpeechSpan; 1] {
        [SpeechSpan {
            speaker: 0,
            start_ns: 0,
            end_ns: 10_000_000_000,
        }]
    }

    #[test]
    fn equal_speaking_overlap_leaves_the_voice_unnamed() {
        let roster = [
            NamedParticipant {
                id: "a",
                name: "Alice",
            },
            NamedParticipant {
                id: "b",
                name: "Bob",
            },
        ];
        let active = [
            ActiveSpan {
                participant: "a",
                start_ns: 0,
                end_ns: 5_000_000_000,
            },
            ActiveSpan {
                participant: "b",
                start_ns: 5_000_000_000,
                end_ns: 10_000_000_000,
            },
        ];
        assert_eq!(
            match_names(&speech(), &active, &roster, &[true]),
            Vec::new()
        );
    }

    #[test]
    fn a_gap_does_not_inherit_the_last_active_name() {
        let roster = [NamedParticipant {
            id: "a",
            name: "Alice",
        }];
        let active = [ActiveSpan {
            participant: "a",
            start_ns: 0,
            end_ns: 2_000_000_000,
        }];
        assert_eq!(
            match_names(&speech(), &active, &roster, &[true]),
            Vec::new()
        );
    }

    #[test]
    fn a_changed_name_follows_only_the_same_participant_identity() {
        let roster = [
            NamedParticipant {
                id: "a",
                name: "Alice",
            },
            NamedParticipant {
                id: "a",
                name: "Alice Smith",
            },
        ];
        let active = [ActiveSpan {
            participant: "a",
            start_ns: 0,
            end_ns: 10_000_000_000,
        }];
        assert_eq!(
            match_names(&speech(), &active, &roster, &[true]),
            vec![NameMatch {
                speaker: 0,
                name: "Alice Smith",
                overlap_ns: 10_000_000_000,
                speech_ns: 10_000_000_000
            }]
        );
    }

    #[test]
    fn a_typed_generated_looking_name_still_wins() {
        let roster = [NamedParticipant {
            id: "a",
            name: "Alice",
        }];
        let active = [ActiveSpan {
            participant: "a",
            start_ns: 0,
            end_ns: 10_000_000_000,
        }];
        assert_eq!(
            match_names(
                &speech(),
                &active,
                &roster,
                &[may_name("Speaker 2", 1, false)]
            ),
            Vec::new()
        );
        assert!(may_name("Speaker 2", 0, false));
    }

    #[test]
    fn a_voice_match_wins_over_the_call_signal() {
        assert!(!may_name("Unknown speaker", 0, true));
        assert!(!may_name("Alice", 0, false));
        assert!(!may_name("Local speaker", 0, false));
    }

    #[test]
    fn two_voices_cannot_claim_the_same_person() {
        let roster = [NamedParticipant {
            id: "a",
            name: "Alice",
        }];
        let active = [ActiveSpan {
            participant: "a",
            start_ns: 0,
            end_ns: 10_000_000_000,
        }];
        let speech = [
            SpeechSpan {
                speaker: 0,
                start_ns: 0,
                end_ns: 5_000_000_000,
            },
            SpeechSpan {
                speaker: 1,
                start_ns: 5_000_000_000,
                end_ns: 10_000_000_000,
            },
        ];
        assert_eq!(
            match_names(&speech, &active, &roster, &[false, true]),
            Vec::new()
        );
    }
}

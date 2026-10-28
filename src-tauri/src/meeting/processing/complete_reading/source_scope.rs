use super::{
    ArtifactCitation, MeetingEvidence, RawArtifactOutput, RawCitedText, RawLedgerOutput,
    TranscriptSegmentId,
};
use indexmap::IndexSet;
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use std::{borrow::Cow, cell::RefCell, collections::HashMap, fmt, ops::Range};

pub(in crate::meeting::processing) struct SourceScope<'a> {
    evidence: &'a [MeetingEvidence],
    indexes: HashMap<TranscriptSegmentId, usize>,
    groups: RefCell<IndexSet<Vec<usize>>>,
}

impl<'a> SourceScope<'a> {
    pub(super) fn new(evidence: &'a [MeetingEvidence]) -> Result<Self, ()> {
        let mut indexes = HashMap::with_capacity(evidence.len());
        for (index, item) in evidence.iter().enumerate() {
            let id = uuid::Uuid::parse_str(&item.citation.entity_id).map_err(|_| ())?;
            indexes.insert(TranscriptSegmentId::from_uuid(id), index);
        }
        Ok(Self {
            evidence,
            indexes,
            groups: RefCell::new(IndexSet::new()),
        })
    }

    pub(super) fn evidence(&self) -> &'a [MeetingEvidence] {
        self.evidence
    }

    pub(super) fn raw_run_end(
        &self,
        start: usize,
        speakers: Option<&HashMap<String, &str>>,
    ) -> usize {
        let first = &self.evidence[start];
        let speaker = |item: &MeetingEvidence| {
            speakers.and_then(|values| values.get(&item.citation.entity_id).copied())
        };
        let mut end = start + 1;
        while let Some(next) = self.evidence.get(end) {
            if first.text != next.text
                || first.citation.kind != next.citation.kind
                || first.citation.session_id != next.citation.session_id
                || speaker(first) != speaker(next)
            {
                break;
            }
            end += 1;
        }
        end
    }

    pub(super) fn raw_source(
        &self,
        range: Range<usize>,
        ledger: bool,
    ) -> (Cow<'_, str>, Option<SourceOccurrences<'_, 'a>>) {
        if range.len() == 1 {
            return (
                Cow::Borrowed(&self.evidence[range.start].citation.entity_id),
                None,
            );
        }
        let label = SourceLabel(self.group_index(range.clone().collect()) + 1).to_string();
        (
            Cow::Owned(label),
            Some(SourceOccurrences {
                scope: self,
                range,
                ledger,
            }),
        )
    }

    fn reference_index(&self, reference: &str) -> Result<usize, ()> {
        if reference.starts_with('S') {
            reference_index(reference)
        } else {
            let id = uuid::Uuid::parse_str(reference).map_err(|_| ())?;
            self.indexes
                .get(&TranscriptSegmentId::from_uuid(id))
                .copied()
                .ok_or(())
        }
    }

    fn source_index<T: SourceIdentity>(&self, source: &T) -> Result<usize, ()> {
        self.indexes.get(&source.source_id()?).copied().ok_or(())
    }

    pub(super) fn extend_sources<T: SourceIdentity>(
        &self,
        sources: &[T],
        indexes: &mut IndexSet<usize>,
    ) -> Result<(), ()> {
        for source in sources {
            indexes.insert(self.source_index(source)?);
        }
        Ok(())
    }

    fn group_index(&self, mut indexes: Vec<usize>) -> usize {
        indexes.sort_unstable();
        indexes.dedup();
        if indexes.len() == 1 {
            indexes[0]
        } else {
            let (group, _) = self.groups.borrow_mut().insert_full(indexes);
            self.evidence.len() + group
        }
    }

    pub(super) fn restore_ids(&self, ids: &mut Vec<String>) -> Result<(), ()> {
        let groups = self.groups.borrow();
        let expanded_len = ids.iter().try_fold(0usize, |total, id| {
            let index = self.reference_index(id)?;
            let count = if index < self.evidence.len() {
                1
            } else {
                groups
                    .get_index(index - self.evidence.len())
                    .ok_or(())?
                    .len()
            };
            total.checked_add(count).ok_or(())
        })?;
        if expanded_len == ids.len() {
            for id in ids {
                let source = self.evidence.get(self.reference_index(id)?).ok_or(())?;
                id.clone_from(&source.citation.entity_id);
            }
            return Ok(());
        }
        let mut expanded = Vec::with_capacity(expanded_len);
        for id in ids.iter() {
            let index = self.reference_index(id)?;
            if index < self.evidence.len() {
                expanded.push(self.evidence[index].citation.entity_id.clone());
            } else {
                for source in groups.get_index(index - self.evidence.len()).ok_or(())? {
                    expanded.push(self.evidence[*source].citation.entity_id.clone());
                }
            }
        }
        *ids = expanded;
        Ok(())
    }

    fn restore_cited(&self, value: &mut RawCitedText) -> Result<(), ()> {
        self.restore_ids(&mut value.citations)
    }

    pub(super) fn restore_notes(&self, output: &mut RawArtifactOutput) -> Result<(), ()> {
        for cited in output
            .summary
            .iter_mut()
            .chain(&mut output.decisions)
            .chain(&mut output.key_questions)
            .chain(&mut output.risks)
        {
            self.restore_cited(cited)?;
        }
        for topic in &mut output.outline {
            self.restore_cited(&mut topic.title)?;
            if let Some(detail) = &mut topic.detail {
                self.restore_cited(detail)?;
            }
        }
        for action in &mut output.action_items {
            self.restore_cited(&mut action.text)?;
        }
        self.restore_cited(&mut output.follow_up_draft)
    }

    pub(in crate::meeting::processing) fn restore_ledger(
        &self,
        output: &mut RawLedgerOutput,
    ) -> Result<(), ()> {
        for thread in &mut output.threads {
            self.restore_ids(&mut thread.receipt.citations)?;
        }
        for open_loop in &mut output.open_loops {
            self.restore_ids(&mut open_loop.citations)?;
        }
        for commitment in &mut output.commitments {
            self.restore_ids(&mut commitment.receipt.citations)?;
        }
        for stance in &mut output.stances {
            self.restore_ids(&mut stance.citations)?;
        }
        Ok(())
    }
}

pub(super) struct SourceOccurrences<'scope, 'source> {
    scope: &'scope SourceScope<'source>,
    range: Range<usize>,
    ledger: bool,
}

#[derive(Serialize)]
struct RegularOccurrences {
    first_source: usize,
    count: usize,
    start_offset_ns: u64,
    end_offset_ns: u64,
    step_ns: u64,
}

impl SourceOccurrences<'_, '_> {
    fn regular_timing(&self) -> Option<RegularOccurrences> {
        let first = &self.scope.evidence[self.range.start].citation;
        let second = &self.scope.evidence[self.range.start + 1].citation;
        let start_offset_ns = first.start_offset_ns?;
        let end_offset_ns = first.end_offset_ns?;
        let step_ns = second.start_offset_ns?.checked_sub(start_offset_ns)?;
        let regular = self.range.clone().enumerate().all(|(offset, index)| {
            let Some(delta) = u64::try_from(offset)
                .ok()
                .and_then(|offset| step_ns.checked_mul(offset))
            else {
                return false;
            };
            let citation = &self.scope.evidence[index].citation;
            citation.start_offset_ns == start_offset_ns.checked_add(delta)
                && citation.end_offset_ns == end_offset_ns.checked_add(delta)
        });
        regular.then_some(RegularOccurrences {
            first_source: self.range.start,
            count: self.range.len(),
            start_offset_ns,
            end_offset_ns,
            step_ns,
        })
    }
}

impl Serialize for SourceOccurrences<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let Some(regular) = self.regular_timing() {
            return regular.serialize(serializer);
        }
        struct Occurrence<'a> {
            item: &'a MeetingEvidence,
            ledger: bool,
        }
        impl Serialize for Occurrence<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry(
                    if self.ledger {
                        "segment_uuid"
                    } else {
                        "entity_id"
                    },
                    &self.item.citation.entity_id,
                )?;
                map.serialize_entry("start_offset_ns", &self.item.citation.start_offset_ns)?;
                map.serialize_entry("end_offset_ns", &self.item.citation.end_offset_ns)?;
                map.end()
            }
        }
        let mut sequence = serializer.serialize_seq(Some(self.range.len()))?;
        for index in self.range.clone() {
            sequence.serialize_element(&Occurrence {
                item: &self.scope.evidence[index],
                ledger: self.ledger,
            })?;
        }
        sequence.end()
    }
}

fn reference_index(reference: &str) -> Result<usize, ()> {
    reference
        .strip_prefix('S')
        .and_then(|index| index.parse::<usize>().ok())
        .and_then(|index| index.checked_sub(1))
        .ok_or(())
}

struct SourceLabel(usize);

impl fmt::Display for SourceLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "S{}", self.0)
    }
}

impl Serialize for SourceLabel {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

pub(super) trait SourceIdentity {
    fn source_id(&self) -> Result<TranscriptSegmentId, ()>;
}

impl SourceIdentity for ArtifactCitation {
    fn source_id(&self) -> Result<TranscriptSegmentId, ()> {
        Ok(self.segment_id)
    }
}

impl SourceIdentity for String {
    fn source_id(&self) -> Result<TranscriptSegmentId, ()> {
        uuid::Uuid::parse_str(self)
            .map(TranscriptSegmentId::from_uuid)
            .map_err(|_| ())
    }
}

pub(super) struct SourceIndexes<'a, 'source>(
    pub(super) &'a SourceScope<'source>,
    pub(super) &'a IndexSet<usize>,
);

impl Serialize for SourceIndexes<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let index = match self.1.len() {
            0 => return serializer.collect_seq(std::iter::empty::<SourceLabel>()),
            1 => self.1[0],
            _ => self.0.group_index(self.1.iter().copied().collect()),
        };
        let mut sequence = serializer.serialize_seq(Some(1))?;
        sequence.serialize_element(&SourceLabel(index + 1))?;
        sequence.end()
    }
}

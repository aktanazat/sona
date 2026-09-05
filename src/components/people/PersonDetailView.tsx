import React from "react";
import { useTranslation } from "react-i18next";
import type {
  Document,
  PersonDetail,
  PersonListEntry,
  PersonMeetingLink,
  PersonSplitRequest,
} from "@/bindings";
import { SettingsPage } from "@/components/settings/rows";
import { ChartCard } from "@/components/charts";
import { Bars } from "@/components/vg/chart";
import { PersonDocuments } from "./PersonDocuments";
import { PersonEvidence } from "./PersonEvidence";
import { PersonHeader } from "./PersonHeader";
import { PersonCommitments, PersonOpenLoops } from "./PersonLedgerSections";
import { PersonMeetings } from "./PersonMeetings";
import { PersonSummarySection } from "./PersonSummarySection";
import { confirmedPersonLinks, monthlyMeetingCadence } from "./peopleModel";

export interface PersonDetailViewProps {
  detail: PersonDetail;
  people: PersonListEntry[];
  documents: Document[];
  documentsLoadFailed: boolean;
  pending: boolean;
  onBack: () => void;
  onRename: (displayName: string) => void;
  onMerge: (targetPersonId: string) => void;
  onDelete: () => void;
  onSplit: (
    request: Omit<PersonSplitRequest, "source_person_id" | "expected_revision">,
  ) => void;
  onConfirmLink: (link: PersonMeetingLink) => void;
  onUnlink: (link: PersonMeetingLink) => void;
  onImportDocument: () => void;
  onDeleteDocument: (document: Document) => void;
  /** Opens the meeting a line came from. Every ledger line is a link to one. */
  onOpenMeeting: (meetingId: string) => void;
  /** Opens the page for the organization the header names. Absent in the
   * person dialog, which has nowhere to put one. */
  onOpenOrganization?: (organization: string) => void;
  /** Rewrites the relationship paragraph under the header. */
  onRegenerateSummary: () => void;
  onRemoveVoiceProfile?: () => void;
}

export const PersonDetailView: React.FC<PersonDetailViewProps> = ({
  detail,
  people,
  documents,
  documentsLoadFailed,
  pending,
  onBack,
  onRename,
  onMerge,
  onDelete,
  onSplit,
  onConfirmLink,
  onUnlink,
  onImportDocument,
  onDeleteDocument,
  onOpenMeeting,
  onOpenOrganization,
  onRegenerateSummary,
  onRemoveVoiceProfile,
}) => {
  const { t } = useTranslation();
  const confirmedLinks = confirmedPersonLinks(detail.links);
  const cadence = monthlyMeetingCadence(confirmedLinks);
  const talkShare =
    detail.talk_share_avg_permille === null
      ? null
      : `${(detail.talk_share_avg_permille / 10).toLocaleString(undefined, {
          maximumFractionDigits: 1,
        })}%`;
  /* When you last met is the header's line now, so the chart's footer keeps
   * only the fact the chart itself cannot draw. */
  const footerFacts =
    talkShare === null
      ? []
      : [{ label: t("people.detail.talkShare"), value: talkShare }];

  return (
    <SettingsPage
      data-slot="person-detail"
      /* Tighter than the page's eight-point section gap: this is seven
       * labelled blocks about one person, and at gap-8 the catalogue reads as
       * a stack of unrelated pages. */
      className="gap-6"
      header={
        <PersonHeader
          key={`${detail.person.id}:${detail.person.display_name}`}
          person={detail.person}
          people={people}
          links={detail.links}
          documents={detail.documents}
          pending={pending}
          onBack={onBack}
          onRename={onRename}
          onMerge={onMerge}
          onDelete={onDelete}
          onSplit={onSplit}
          onOpenOrganization={onOpenOrganization}
          onRegenerateSummary={onRegenerateSummary}
          onImportDocument={onImportDocument}
          onRemoveVoiceProfile={onRemoveVoiceProfile}
        />
      }
    >
      {/* The paragraph first, when there is one: three sentences about who
       * this is to you. Then what the page is for - what is still open
       * between you, in both directions - and only then the archive it came
       * out of: the meetings, the chart drawn from them, the files, and last
       * of all how Sona connected this person to any of it. Provenance is the
       * line a reader checks once, so it reads last.
       *
       * Every one of these renders nothing at all when it holds nothing: an
       * empty person is a name and a menu, not eight labelled absences. */}
      <PersonSummarySection summary={detail.person.summary} />
      <PersonOpenLoops
        openLoops={detail.open_loops}
        personName={detail.person.display_name}
        onOpenMeeting={onOpenMeeting}
      />
      <PersonCommitments
        commitments={detail.commitments}
        personName={detail.person.display_name}
        onOpenMeeting={onOpenMeeting}
      />
      <PersonMeetings
        links={detail.links}
        pending={pending}
        onConfirm={onConfirmLink}
        onUnlink={onUnlink}
      />

      {confirmedLinks.length === 0 ? null : (
        <ChartCard
          data-slot="person-cadence"
          label={t("people.detail.cadence")}
          /* A bare "12" over a chart labelled "Meeting cadence" reads as a
           * cadence, which it is not: it is how many meetings the bars are
           * drawn from. The noun costs nothing and answers it. */
          metric={t("people.list.meetings", { count: confirmedLinks.length })}
          footerFacts={footerFacts}
        >
          <Bars
            data-slot="person-cadence-bars"
            values={cadence}
            ariaLabel={t("people.detail.cadenceAria", {
              values: cadence.join(", "),
            })}
          />
        </ChartCard>
      )}

      <PersonDocuments
        documents={documents}
        loadFailed={documentsLoadFailed}
        pending={pending}
        onDelete={onDeleteDocument}
      />
      <PersonEvidence person={detail.person} links={detail.links} />
    </SettingsPage>
  );
};

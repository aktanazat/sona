import React, { useState } from "react";
import { ArrowLeft, MoreHorizontal } from "lucide-react";
import { useTranslation } from "react-i18next";
import type {
  DocumentSummary,
  Person,
  PersonListEntry,
  PersonMeetingLink,
  PersonSplitRequest,
} from "@/bindings";
import { PageTitle } from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/vg/dropdown-menu";
import { Input } from "@/components/vg/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/vg/select";
import { formatEntryTimestamp } from "@/lib/utils/format";
import { PeopleConfirmDialog } from "./PeopleConfirmDialog";
import { PersonSplitDialog } from "./PersonSplitDialog";
import { confirmedPersonLinks, latestConfirmedMeetingAt } from "./peopleModel";

interface PersonHeaderProps {
  person: Person;
  people: PersonListEntry[];
  links: PersonMeetingLink[];
  documents: DocumentSummary[];
  pending: boolean;
  onBack: () => void;
  onRename: (displayName: string) => void;
  onMerge: (targetPersonId: string) => void;
  /* Absent in the person dialog the meeting-review band opens: a modal has no
   * place to put a page, and a label that looks like a link and goes nowhere
   * is worse than a label. The same absence `onOpenMeeting` resolves one level
   * up, for the same reason. */
  onOpenOrganization?: (organization: string) => void;
  onDelete: () => void;
  onSplit: (
    request: Omit<PersonSplitRequest, "source_person_id" | "expected_revision">,
  ) => void;
  /** Rewrites the relationship paragraph. Empty, that paragraph is not on the
   * page at all, so the verb that writes the first one lives here. */
  onRegenerateSummary: () => void;
  /** Adds context about this person. Same reason: with no documents there is
   * no Documents section to hang it off. */
  onImportDocument: () => void;
  onRemoveVoiceProfile?: () => void;
}

/**
 * A person's page reads as a page about that person, in the same three lines
 * every document page in the app uses: the way back, the name, and one Meta
 * line — where they are, and when you last met.
 *
 * Every verb is in the one menu. Rename opens the title as a field, which
 * commits on Enter or on leaving it and reverts on Escape; there is no Save,
 * because a rename is one value with a receipt behind it, and no "rename this
 * person?" dialog, because confirming a reversible edit of a name is ceremony.
 * The two verbs that write a section — the relationship paragraph and an
 * imported document — are here rather than in those sections, because a
 * section with nothing in it is not rendered and a verb that disappears with
 * its own empty state can never be pressed. Splitting, merging, forgetting a
 * saved voice and deleting change who this person is, so they sit under a
 * separator, and every irreversible one keeps its confirmation.
 */
export const PersonHeader: React.FC<PersonHeaderProps> = ({
  person,
  people,
  links,
  documents,
  pending,
  onBack,
  onRename,
  onMerge,
  onDelete,
  onSplit,
  onOpenOrganization,
  onRegenerateSummary,
  onImportDocument,
  onRemoveVoiceProfile,
}) => {
  const { t } = useTranslation();
  const [editing, setEditing] = useState(false);
  const [nameDraft, setNameDraft] = useState(person.display_name);
  const [splitting, setSplitting] = useState(false);
  const [mergeConfirming, setMergeConfirming] = useState(false);
  const [deleteConfirming, setDeleteConfirming] = useState(false);
  const [voiceProfileRemovalConfirming, setVoiceProfileRemovalConfirming] =
    useState(false);
  const [mergeTarget, setMergeTarget] = useState<string | null>(null);
  const mergeOptions = people.filter((entry) => entry.person.id !== person.id);
  const mergeTargetName = mergeOptions.find(
    (entry) => entry.person.id === mergeTarget,
  )?.person.display_name;
  const moreLabel = t("common.more");
  const organization = person.organization ?? "";
  const lastMetAtMs = latestConfirmedMeetingAt(confirmedPersonLinks(links));

  /* One commit path. Enter and Escape both blur the field; Escape puts the
   * saved name back first, so leaving the field is the only thing that ever
   * writes, and it writes only when the name actually changed. */
  const commitName = () => {
    setEditing(false);
    const trimmed = nameDraft.trim();
    if (trimmed === "" || trimmed === person.display_name) {
      setNameDraft(person.display_name);
      return;
    }
    onRename(trimmed);
  };

  /* The organization is the one word on the line that goes somewhere: it has a
   * page of its own — everybody Sona knows there, and what is open across them
   * — and this is the only place its name already appears. */
  const organizationNode =
    person.organization === null ? null : onOpenOrganization === undefined ? (
      <span data-slot="person-organization">{organization}</span>
    ) : (
      <button
        type="button"
        data-slot="person-organization"
        onClick={() => onOpenOrganization(organization)}
        className="-mx-1 rounded px-1 underline decoration-gray-alpha-400 underline-offset-2 hover:text-gray-1000 hover:decoration-gray-700 focus-visible:ring-2 focus-visible:ring-focus-ring focus-visible:outline-none"
      >
        {organization}
      </button>
    );
  const lastMetNode =
    lastMetAtMs === null
      ? null
      : t("peopleV2.list.lastMet", {
          date: formatEntryTimestamp(lastMetAtMs),
        });

  return (
    <div className="flex flex-col gap-3" data-slot="person-header">
      <Button
        type="button"
        variant="ghost"
        size="sm"
        className="w-fit -ms-2"
        onClick={onBack}
      >
        <ArrowLeft aria-hidden="true" />
        {t("people.title")}
      </Button>

      <div className="flex items-start justify-between gap-4">
        <div className="flex min-w-0 flex-col gap-1.5">
          {editing ? (
            <Input
              autoFocus
              value={nameDraft}
              onChange={(event) => setNameDraft(event.target.value)}
              onBlur={commitName}
              onKeyDown={(event) => {
                if (event.key === "Enter") event.currentTarget.blur();
                if (event.key === "Escape") {
                  setNameDraft(person.display_name);
                  event.currentTarget.blur();
                }
              }}
              aria-label={t("people.detail.nameLabel")}
              className="h-10 min-w-0 flex-1 sm:max-w-[360px] text-[24px] leading-[30px] font-semibold tracking-[-0.01em]"
            />
          ) : (
            <PageTitle className="truncate">{person.display_name}</PageTitle>
          )}
          {organizationNode === null && lastMetNode === null ? null : (
            <p className="text-[13px] leading-[18px] text-gray-900 tabular-nums">
              {organizationNode}
              {organizationNode !== null && lastMetNode !== null ? " · " : null}
              {lastMetNode}
            </p>
          )}
        </div>

        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              className="flex-none text-gray-700 hover:text-gray-1000"
              aria-label={moreLabel}
              title={moreLabel}
            >
              <MoreHorizontal aria-hidden="true" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="min-w-52">
            <DropdownMenuItem
              disabled={pending}
              onSelect={() => {
                setNameDraft(person.display_name);
                setEditing(true);
              }}
            >
              {t("people.detail.rename")}
            </DropdownMenuItem>
            <DropdownMenuItem disabled={pending} onSelect={onRegenerateSummary}>
              {t("people.summary.regenerate")}
            </DropdownMenuItem>
            <DropdownMenuItem disabled={pending} onSelect={onImportDocument}>
              {t("people.detail.importDocument")}
            </DropdownMenuItem>
            <DropdownMenuSeparator />
            <DropdownMenuItem
              disabled={pending || mergeOptions.length === 0}
              onSelect={() => setMergeConfirming(true)}
            >
              {t("people.detail.merge")}
            </DropdownMenuItem>
            <DropdownMenuItem
              disabled={pending}
              onSelect={() => setSplitting(true)}
            >
              {t("people.detail.split")}
            </DropdownMenuItem>
            <DropdownMenuSeparator />
            {onRemoveVoiceProfile === undefined ? null : (
              <DropdownMenuItem
                disabled={pending}
                variant="destructive"
                onSelect={() => setVoiceProfileRemovalConfirming(true)}
              >
                {t("people.detail.removeVoiceProfile")}
              </DropdownMenuItem>
            )}
            <DropdownMenuItem
              disabled={pending}
              variant="destructive"
              onSelect={() => setDeleteConfirming(true)}
            >
              {t("people.detail.deletePerson")}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      <PersonSplitDialog
        open={splitting}
        onOpenChange={setSplitting}
        person={person}
        people={people}
        links={links}
        documents={documents}
        pending={pending}
        onSplit={onSplit}
      />

      <PeopleConfirmDialog
        open={mergeConfirming}
        onOpenChange={setMergeConfirming}
        title={t("people.detail.mergeTitle")}
        description={t("people.detail.mergeDescription", {
          source: person.display_name,
          target: mergeTargetName ?? t("people.detail.mergeTarget"),
        })}
        confirmLabel={t("people.detail.merge")}
        pending={pending || mergeTarget === null}
        destructive
        onConfirm={() => {
          if (mergeTarget !== null) onMerge(mergeTarget);
        }}
      >
        <Select value={mergeTarget ?? undefined} onValueChange={setMergeTarget}>
          <SelectTrigger
            size="sm"
            className="w-full"
            aria-label={t("people.detail.mergeTarget")}
          >
            <SelectValue placeholder={t("people.detail.mergeTarget")} />
          </SelectTrigger>
          <SelectContent>
            {mergeOptions.map((entry) => (
              <SelectItem key={entry.person.id} value={entry.person.id}>
                {entry.person.display_name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </PeopleConfirmDialog>

      <PeopleConfirmDialog
        open={deleteConfirming}
        onOpenChange={setDeleteConfirming}
        title={t("people.detail.deleteTitle")}
        description={t("people.detail.deleteDescription", {
          name: person.display_name,
        })}
        confirmLabel={t("people.detail.deletePerson")}
        pending={pending}
        destructive
        onConfirm={onDelete}
      />

      {onRemoveVoiceProfile === undefined ? null : (
        <PeopleConfirmDialog
          open={voiceProfileRemovalConfirming}
          onOpenChange={setVoiceProfileRemovalConfirming}
          title={t("people.detail.removeVoiceProfileTitle")}
          description={t("people.detail.removeVoiceProfileDescription")}
          confirmLabel={t("people.detail.removeVoiceProfile")}
          pending={pending}
          destructive
          onConfirm={onRemoveVoiceProfile}
        />
      )}
    </div>
  );
};

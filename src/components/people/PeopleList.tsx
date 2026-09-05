import React from "react";
import { ChevronRight } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { PersonListEntry } from "@/bindings";
import {
  Microlabel,
  SETTINGS_SURFACE,
  SettingsPage,
  SettingsSurface,
} from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import { formatRelativeTime } from "@/lib/utils/format";
import { EmptyStateRow } from "./EmptyStateRow";
import { organizationsFromEntries } from "./peopleModel";

export interface PeopleListViewProps {
  entries: PersonListEntry[] | null;
  error: boolean;
  onOpenPerson: (personId: string) => void;
  onOpenOrganization: (organization: string) => void;
  onRetry: () => void;
}

/* The organizations the list already carries, as a strip above it.
 *
 * Derived from the loaded rows rather than asked for: every person row already
 * says which organization they are at, so a second command for the same fact
 * would be a second answer to it. A corpus with no calendar domains on it draws
 * nothing here, which is the honest empty state — there is no organization to
 * name. */
const OrganizationStrip: React.FC<{
  entries: readonly PersonListEntry[];
  onOpen: (organization: string) => void;
}> = ({ entries, onOpen }) => {
  const { t } = useTranslation();
  const organizations = organizationsFromEntries(entries);
  if (organizations.length === 0) return null;

  return (
    <SettingsSurface data-slot="organizations-strip">
      <div className="flex flex-col gap-2 px-6 py-4">
        <Microlabel>{t("organizations.title")}</Microlabel>
        <div className="flex flex-wrap gap-2">
          {organizations.map((organization) => (
            <Button
              key={organization.name}
              type="button"
              variant="outline"
              size="sm"
              data-slot="organization-chip"
              onClick={() => onOpen(organization.name)}
            >
              <span>{organization.name}</span>
              <span className="text-gray-800 tabular-nums">
                {organization.count}
              </span>
            </Button>
          ))}
        </div>
      </div>
    </SettingsSurface>
  );
};

/* One person, one row: the name, and one line of relationship facts under it
 * — where they are, how many meetings you have had, and how long ago the last
 * one was. The elapsed phrasing is the fact somebody scans a list of people
 * for; the exact date is on the person's own page, which is what the row
 * opens, along with everything else about them. */
const PersonRow: React.FC<{
  entry: PersonListEntry;
  onOpen: () => void;
}> = ({ entry, onOpen }) => {
  const { t } = useTranslation();
  const lastMeeting = entry.last_meeting;
  const meetings = t("people.list.meetings", {
    count: entry.confirmed_count,
  });
  const lastMet =
    lastMeeting === null
      ? null
      : t("peopleV2.list.lastMet", {
          date: formatRelativeTime(lastMeeting.at_ms),
        });

  return (
    <li data-slot="person-card">
      <button
        type="button"
        onClick={onOpen}
        className="hover-fast flex w-full min-w-0 items-center gap-4 px-6 py-3.5 text-start hover:bg-background-200 focus-visible:ring-2 focus-visible:ring-focus-ring focus-visible:ring-inset focus-visible:outline-none"
      >
        <span className="flex min-w-0 flex-1 flex-col gap-1">
          <span className="min-w-0 truncate text-[14px] leading-[21px] font-medium text-gray-1000">
            {entry.person.display_name}
          </span>
          {/* One line, in the order it is read: the place, the count, then the
           * elapsed time. Interpuncts join it rather than separating cells,
           * because it is a sentence about a person and not a table of them —
           * and the last fact of that sentence is not a second column with an
           * alignment of its own. */}
          <span className="snap-measured min-w-0 truncate text-[13px] leading-[18px] text-gray-900 tabular-nums">
            {entry.person.organization === null ? null : (
              <>
                <span data-slot="person-organization">
                  {entry.person.organization}
                </span>
                {" · "}
              </>
            )}
            {meetings}
            {lastMet === null ? null : ` · ${lastMet}`}
          </span>
        </span>
        <ChevronRight
          aria-hidden="true"
          className="size-3.5 flex-none text-gray-700 rtl:rotate-180"
        />
      </button>
    </li>
  );
};

export const PeopleListView: React.FC<PeopleListViewProps> = ({
  entries,
  error,
  onOpenPerson,
  onOpenOrganization,
  onRetry,
}) => {
  const { t } = useTranslation();

  return (
    <SettingsPage title={t("people.title")} data-slot="people-page">
      {error ? (
        <SettingsSurface>
          <EmptyStateRow
            action={
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={onRetry}
              >
                {t("common.retry")}
              </Button>
            }
          >
            {t("people.list.loadError")}
          </EmptyStateRow>
        </SettingsSurface>
      ) : entries === null ? (
        <SettingsSurface>
          <EmptyStateRow>{t("common.loading")}</EmptyStateRow>
        </SettingsSurface>
      ) : entries.length === 0 ? (
        <SettingsSurface>
          <EmptyStateRow>{t("people.list.empty")}</EmptyStateRow>
        </SettingsSurface>
      ) : (
        <>
          <OrganizationStrip entries={entries} onOpen={onOpenOrganization} />
          <ul
            role="list"
            aria-label={t("people.title")}
            data-slot="people-list"
            className={SETTINGS_SURFACE}
          >
            {entries.map((entry) => (
              <PersonRow
                key={entry.person.id}
                entry={entry}
                onOpen={() => onOpenPerson(entry.person.id)}
              />
            ))}
          </ul>
        </>
      )}
    </SettingsPage>
  );
};

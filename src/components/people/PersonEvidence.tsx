import React from "react";
import { useTranslation } from "react-i18next";
import type { Person, PersonLinkSource, PersonMeetingLink } from "@/bindings";
import { SettingsSection } from "@/components/settings/rows";

/* The order these are read in, which is also how strong they are: an invite
 * names a person outright, a voice match is recognition, a title is a guess,
 * and a manual link is you. */
const EVIDENCE_ORDER = [
  "calendar",
  "speaker",
  "title",
  "manual",
] as const satisfies readonly PersonLinkSource[];

/* `satisfies` keeps the literal keyed type while proving every
 * `PersonLinkSource` has a slot, so `counts[link.source]` stays checked. */
const countLinksBySource = (links: PersonMeetingLink[]) => {
  const counts = {
    calendar: 0,
    speaker: 0,
    title: 0,
    manual: 0,
  } satisfies Record<PersonLinkSource, number>;
  for (const link of links) counts[link.source] += 1;
  return counts;
};

/**
 * How Sona knows this is this person: what the links were made from, what else
 * they are called, and the addresses an invite reaches them at.
 *
 * Three quiet lines rather than a table of one row per kind of evidence — the
 * counts are one sentence, and the names and addresses used to sit under the
 * title as two more Meta lines competing with it. Nothing here is pressable,
 * so nothing here is a chip. With none of the three, the section is not on the
 * page: there is no honest empty state for "why", only silence.
 */
export const PersonEvidence: React.FC<{
  person: Person;
  links: PersonMeetingLink[];
}> = ({ person, links }) => {
  const { t } = useTranslation();
  const counts = countLinksBySource(links);
  const sources = EVIDENCE_ORDER.filter((source) => counts[source] > 0);

  const lines = [
    ...(sources.length === 0
      ? []
      : [
          {
            slot: "person-evidence-sources",
            text: sources
              .map(
                (source) =>
                  `${t(`people.source.${source}`)} ${t("people.list.meetings", {
                    count: counts[source],
                  })}`,
              )
              .join(" · "),
          },
        ]),
    ...(person.aliases.length === 0
      ? []
      : [
          {
            slot: "person-aliases",
            text: t("people.detail.aliases", {
              aliases: person.aliases.join(" · "),
            }),
          },
        ]),
    ...(person.calendar_emails.length === 0
      ? []
      : [
          {
            slot: "person-addresses",
            text: t("people.detail.addresses", {
              addresses: person.calendar_emails.join(" · "),
            }),
          },
        ]),
  ];
  if (lines.length === 0) return null;

  return (
    <SettingsSection label={t("peopleV2.detail.howSonaKnows")}>
      <div className="flex flex-col gap-1.5 px-6 py-3.5">
        {lines.map((line) => (
          <p
            key={line.slot}
            data-slot={line.slot}
            className="snap-measured min-w-0 text-[13px] leading-[18px] text-gray-900 text-pretty tabular-nums"
          >
            {line.text}
          </p>
        ))}
      </div>
    </SettingsSection>
  );
};

import React from "react";
import { useTranslation } from "react-i18next";
import type {
  MeetingLoopRow,
  MeetingLoopStatus,
  PersonListEntry,
} from "@/bindings";
import { Microlabel, Notice } from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import { Checkbox } from "@/components/vg/checkbox";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/vg/select";
import { LedgerReceiptRow } from "./LedgerReceiptRow";

/* Loops that close.
 *
 * A ledger row used to be something you read. These are rows you act on: tick
 * one off, drop it, or hand it to somebody. The words still come from the
 * ledger and the state still comes from the store, which is why every row
 * carries its receipt underneath — closing a loop is a claim about the
 * conversation, and the quote is what makes it checkable.
 *
 * Colour is the second channel: the status word says it either way. */

const STATUS_CLASSES = {
  open: "text-amber-900",
  done: "text-gray-1000",
  dropped: "text-red-900",
  carried: "text-gray-800",
} as const satisfies Record<MeetingLoopStatus, string>;

/** The option value standing for "nobody": Select has no empty-string value. */
const UNASSIGNED = "unassigned";

export const LoopStatusChip: React.FC<{ status: MeetingLoopStatus }> = ({
  status,
}) => {
  const { t } = useTranslation();

  return (
    <span
      data-slot="loop-status"
      className={`flex-none text-[13px] leading-[18px] whitespace-nowrap ${STATUS_CLASSES[status]}`}
    >
      {t(`meetings.loops.status.${status}`)}
    </span>
  );
};

/** What a person did to a row. One shape so one command path answers it. */
export type LoopChange =
  | { kind: "resolve"; dropped: boolean }
  | { kind: "reopen" }
  | { kind: "assign"; personId: string | null };

export interface LoopRowsProps {
  rows: MeetingLoopRow[];
  /** Everybody who could own a loop, for the owner picker. */
  people: PersonListEntry[];
  /** True while a loop mutation is in flight, or the meeting is not editable. */
  disabled: boolean;
  emptyText: string;
  onChange: (row: MeetingLoopRow, change: LoopChange) => void;
  onJumpToSegment: (segmentId: string) => void;
}

export const LoopRows: React.FC<LoopRowsProps> = ({
  rows,
  people,
  disabled,
  emptyText,
  onChange,
  onJumpToSegment,
}) => {
  const { t } = useTranslation();

  if (rows.length === 0) {
    return (
      <Notice tone="muted" live={false}>
        {emptyText}
      </Notice>
    );
  }

  return (
    <ul role="list" className="flex flex-col gap-4">
      {rows.map((row) => {
        const done = row.status === "done";
        return (
          <li
            key={row.loop_id}
            data-slot="loop-row"
            className="flex flex-col gap-1.5"
          >
            <div className="flex items-start gap-2.5">
              <Checkbox
                checked={done}
                disabled={disabled}
                onCheckedChange={(checked) =>
                  onChange(
                    row,
                    checked === true
                      ? { kind: "resolve", dropped: false }
                      : { kind: "reopen" },
                  )
                }
                aria-label={t("meetings.loops.markDone")}
                className="mt-1"
              />
              <div className="flex min-w-0 flex-1 flex-col gap-1">
                <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-0.5">
                  <span
                    className={`min-w-0 text-[14px] leading-[21px] text-pretty text-gray-1000 ${done ? "line-through opacity-60" : ""}`}
                  >
                    {row.text}
                  </span>
                  <LoopStatusChip status={row.status} />
                </div>
                {row.instead === null ? null : (
                  <span className="text-[13px] leading-[18px] text-gray-900">
                    {t("meetings.loops.instead", { instead: row.instead })}
                  </span>
                )}
                {row.firmness === null ? null : (
                  <Microlabel>
                    {t(`meetings.ledger.firmness.${row.firmness}`)}
                  </Microlabel>
                )}
                {row.carried_since_at_utc_ms === null ? null : (
                  <span className="text-[13px] leading-[18px] text-gray-900">
                    {t("meetings.loops.carriedForward")}
                  </span>
                )}
                {row.resolved_by === "external" ? (
                  <span className="text-[13px] leading-[18px] text-gray-900">
                    {t("meetings.loops.closedExternally")}
                  </span>
                ) : null}
                <div className="flex flex-wrap items-center gap-2">
                  {/* The row says who owns it in text. A combobox states its
                   * value only once opened, and who a loop belongs to is the
                   * second thing a reader needs after the loop itself. */}
                  <span
                    data-slot="loop-owner"
                    className="text-[13px] leading-[18px] whitespace-nowrap text-gray-900"
                  >
                    {t("meetings.loops.ownerIs", {
                      owner:
                        row.owner_display_name ??
                        row.owner_text ??
                        t("meetings.loops.ownerUnassigned"),
                    })}
                  </span>
                  <OwnerPicker
                    row={row}
                    people={people}
                    disabled={disabled}
                    onChange={onChange}
                  />
                  {row.status === "open" ? (
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      onClick={() =>
                        onChange(row, { kind: "resolve", dropped: true })
                      }
                      disabled={disabled}
                    >
                      {t("meetings.loops.drop")}
                    </Button>
                  ) : null}
                  {row.status === "dropped" || row.status === "carried" ? (
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      onClick={() => onChange(row, { kind: "reopen" })}
                      disabled={disabled}
                    >
                      {t("meetings.loops.reopen")}
                    </Button>
                  ) : null}
                </div>
                <LedgerReceiptRow
                  quote={row.quote}
                  speaker={row.speaker}
                  atMs={row.at_ms}
                  citations={row.citations}
                  onJumpToSegment={onJumpToSegment}
                />
              </div>
            </div>
          </li>
        );
      })}
    </ul>
  );
};

/* Who owns this loop. The name the ledger read off the transcript is the
 * placeholder, not the value: it is the model's reading, and picking somebody
 * is the user's. */
const OwnerPicker: React.FC<{
  row: MeetingLoopRow;
  people: PersonListEntry[];
  disabled: boolean;
  onChange: (row: MeetingLoopRow, change: LoopChange) => void;
}> = ({ row, people, disabled, onChange }) => {
  const { t } = useTranslation();
  const value = row.owner_person_id ?? UNASSIGNED;
  const placeholder = row.owner_text ?? t("meetings.loops.ownerUnassigned");

  return (
    <Select
      value={value}
      disabled={disabled || people.length === 0}
      onValueChange={(next) =>
        onChange(row, {
          kind: "assign",
          personId: next === UNASSIGNED ? null : next,
        })
      }
    >
      <SelectTrigger
        size="sm"
        aria-label={t("meetings.loops.owner")}
        className="max-w-56 text-[14px]"
      >
        <SelectValue placeholder={placeholder} />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value={UNASSIGNED}>
          {t("meetings.loops.ownerUnassigned")}
        </SelectItem>
        {people.map((entry) => (
          <SelectItem key={entry.person.id} value={entry.person.id}>
            {entry.person.display_name}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
};

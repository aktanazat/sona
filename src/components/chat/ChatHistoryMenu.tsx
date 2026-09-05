import React from "react";
import { useTranslation } from "react-i18next";
import type { AgentChatConversationSummaryV1 } from "@/bindings";
import {
  DropdownMenuItem,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
} from "@/components/vg/dropdown-menu";
import { cn } from "@/lib/cn";

export interface ChatHistorySubmenuProps {
  conversations: readonly AgentChatConversationSummaryV1[];
  /** The one in the column, so the list is a place rather than a pile. */
  currentId: string | null;
  open: boolean;
  /** Opening is when the list is read, so it is when the list is fetched. */
  onOpenChange: (open: boolean) => void;
  onSelect: (conversationId: string) => void;
}

/**
 * The last twenty questions, by the first thing you said in each — one item
 * inside the column's single `…` menu rather than a control of its own.
 *
 * Titles only. A preview of the answer would make every row two lines and a
 * scan of twenty rows a page of reading, and the reason you are here is that
 * you remember asking something, not what came back.
 *
 * A submenu and not a popover beside the header, because the header is now the
 * title and a close glyph: everything a reader does to the conversation rather
 * than in it hangs off the one menu, and Radix's own roving focus is what
 * carries the keyboard into this list.
 */
export const ChatHistorySubmenu: React.FC<ChatHistorySubmenuProps> = ({
  conversations,
  currentId,
  open,
  onOpenChange,
  onSelect,
}) => {
  const { t } = useTranslation();

  return (
    <DropdownMenuSub open={open} onOpenChange={onOpenChange}>
      <DropdownMenuSubTrigger data-slot="chat-history">
        {t("chat.history")}
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent className="max-h-64 w-60 overflow-y-auto">
        {conversations.length === 0 ? (
          /* What would fill it, in the one Meta line an empty list gets. */
          <p className="px-3 py-2 text-[13px] leading-5 text-gray-800">
            {t("chat.historyEmpty")}
          </p>
        ) : (
          conversations.map((conversation) => (
            <DropdownMenuItem
              key={conversation.conversation_id}
              aria-current={
                conversation.conversation_id === currentId ? "true" : undefined
              }
              onSelect={() => onSelect(conversation.conversation_id)}
              className={cn(
                "block truncate",
                conversation.conversation_id === currentId
                  ? "text-gray-1000"
                  : "text-gray-900",
              )}
            >
              {conversation.title}
            </DropdownMenuItem>
          ))
        )}
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  );
};

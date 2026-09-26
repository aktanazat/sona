declare module "*?viewer-behavior-test" {
  export function parseFragment(hash: string): Uint8Array;
  export function renderMarkdown(container: Element, source: string): void;
}

declare module "*?viewer-schema-test" {}

declare module "*?viewer-document-test" {
  export interface SharedBlock {
    type: "paragraph" | "item";
    text: string;
    meta?: string | null;
    time?: string | null;
  }
  export interface SharedDocument {
    title: string;
    include: string;
    notesOutOfDate: boolean;
    sections: { heading: string; blocks: SharedBlock[] }[];
  }
  export function parseShareDocument(source: string): SharedDocument;
  export function renderShareDocument(
    container: Element,
    shared: SharedDocument,
  ): void;
}

declare module "*?viewer-document-boot-test" {}

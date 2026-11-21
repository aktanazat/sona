export type JsonValue =
  | JsonRecord
  | JsonValue[]
  | boolean
  | null
  | number
  | string;

export interface JsonRecord {
  [key: string]: JsonValue;
}

export function isJsonRecord<Value>(value: Value): value is Value & JsonRecord {
  return (
    !Array.isArray(value) &&
    !(value instanceof Function) &&
    Object(value) === value
  );
}

export function isJsonArray<Value>(value: Value): value is Value & JsonValue[] {
  return Array.isArray(value);
}

export function isJsonString(value: JsonValue | undefined): value is string {
  return (
    Object.prototype.toString.call(value) === "[object String]" &&
    !(value instanceof String)
  );
}

export function isJsonInteger(value: JsonValue | undefined): value is number {
  return Number.isSafeInteger(value);
}

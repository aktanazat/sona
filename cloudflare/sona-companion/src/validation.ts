import { MAX_JSON_BYTES } from "./constants";
import {
  base64UrlDecode,
  base64UrlEncode,
  decodeUtf8,
  isOpaqueId,
} from "./encoding";
import { problem } from "./errors";
import {
  isJsonInteger,
  isJsonRecord,
  isJsonString,
  type JsonRecord,
} from "./json";

export async function readLimitedBody(
  request: Request,
  maximumBytes: number,
  requireContentLength = false,
): Promise<Uint8Array> {
  const contentEncoding = request.headers.get("content-encoding");
  if (contentEncoding !== null) throw problem("invalid_request");

  const contentLength = request.headers.get("content-length");
  if (contentLength === null && requireContentLength)
    throw problem("invalid_request");
  if (
    contentLength !== null &&
    (!/^\d+$/u.test(contentLength) || Number(contentLength) > maximumBytes)
  ) {
    throw problem("invalid_request");
  }

  if (request.body === null) return new Uint8Array();
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  try {
    for (;;) {
      const next = await reader.read();
      if (next.done) break;
      length += next.value.length;
      if (length > maximumBytes) {
        await reader.cancel();
        throw problem("invalid_request");
      }
      chunks.push(next.value);
    }
  } finally {
    reader.releaseLock();
  }

  if (contentLength !== null && Number(contentLength) !== length) {
    throw problem("invalid_request");
  }

  const body = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    body.set(chunk, offset);
    offset += chunk.length;
  }
  return body;
}

export function parseJsonBody(body: Uint8Array): JsonRecord {
  if (body.length === 0 || body.length > MAX_JSON_BYTES)
    throw problem("invalid_request");
  try {
    return asRecord(JSON.parse(decodeUtf8(body)));
  } catch {
    throw problem("invalid_request");
  }
}

export function asRecord<Value>(value: Value): JsonRecord {
  if (!isJsonRecord(value)) throw problem("invalid_request");
  return value;
}

export function assertExactKeys(
  value: JsonRecord,
  keys: readonly string[],
): void {
  const allowedKeys = new Set(keys);
  for (const key of Object.keys(value)) {
    if (!allowedKeys.has(key)) throw problem("invalid_request");
  }
}

export function requiredString(
  value: JsonRecord,
  key: string,
  maximumLength = 4096,
): string {
  const candidate = value[key];
  if (
    !isJsonString(candidate) ||
    candidate.length === 0 ||
    candidate.length > maximumLength
  ) {
    throw problem("invalid_request");
  }
  return candidate;
}

export function optionalString(
  value: JsonRecord,
  key: string,
  maximumLength = 4096,
): string | null {
  const candidate = value[key];
  if (candidate === undefined || candidate === null) return null;
  if (
    !isJsonString(candidate) ||
    candidate.length === 0 ||
    candidate.length > maximumLength
  ) {
    throw problem("invalid_request");
  }
  return candidate;
}

export function requiredInteger(
  value: JsonRecord,
  key: string,
  minimum: number,
  maximum: number,
): number {
  const candidate = value[key];
  if (!isJsonInteger(candidate) || candidate < minimum || candidate > maximum) {
    throw problem("invalid_request");
  }
  return candidate;
}

export function requiredOpaqueId(value: JsonRecord, key: string): string {
  const candidate = requiredString(value, key, 128);
  if (!isOpaqueId(candidate)) throw problem("invalid_request");
  return candidate;
}

export function optionalOpaqueId(
  value: JsonRecord,
  key: string,
): string | null {
  const candidate = optionalString(value, key, 128);
  if (candidate !== null && !isOpaqueId(candidate))
    throw problem("invalid_request");
  return candidate;
}

export function fixedBase64Url(value: string, byteLength: number): Uint8Array {
  const decoded = base64UrlDecode(value);
  if (
    decoded === null ||
    decoded.length !== byteLength ||
    base64UrlEncode(decoded) !== value
  ) {
    throw problem("invalid_request");
  }
  return decoded;
}

export function boundedBase64Url(
  value: string,
  maximumBytes: number,
): Uint8Array {
  const decoded = base64UrlDecode(value);
  if (
    decoded === null ||
    decoded.length > maximumBytes ||
    base64UrlEncode(decoded) !== value
  ) {
    throw problem("invalid_request");
  }
  return decoded;
}

export function digest(value: string): string {
  fixedBase64Url(value, 32);
  return value;
}

export function canonicalQuery(
  url: URL,
  allowedKeys: readonly string[],
): readonly [string, string][] {
  const values: [string, string][] = [];
  const allowedKeySet = new Set(allowedKeys);
  const seen = new Set<string>();
  url.searchParams.forEach((value, key) => {
    if (!allowedKeySet.has(key) || seen.has(key) || value.length > 512) {
      throw problem("invalid_request");
    }
    if (!/^[A-Za-z0-9._~-]*$/u.test(value)) throw problem("invalid_request");
    seen.add(key);
    values.push([key, value]);
  });
  values.sort(([leftKey], [rightKey]) =>
    leftKey < rightKey ? -1 : leftKey > rightKey ? 1 : 0,
  );
  return values;
}

export function requireContentType(request: Request, expected: string): void {
  if (request.headers.get("content-type") !== expected)
    throw problem("invalid_request");
}

export function routeId(value: string | undefined): string {
  if (value === undefined || !isOpaqueId(value))
    throw problem("invalid_request");
  return value;
}

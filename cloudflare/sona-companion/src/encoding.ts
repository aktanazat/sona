const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });

export function utf8(value: string): Uint8Array {
  return encoder.encode(value);
}

export function decodeUtf8(value: Uint8Array): string {
  return decoder.decode(value);
}

export function toBytes(value: ArrayBuffer | Uint8Array): Uint8Array {
  if (value instanceof Uint8Array) return value;
  return new Uint8Array(value);
}

export function base64UrlEncode(value: Uint8Array): string {
  let binary = "";
  const blockSize = 0x8000;
  for (let offset = 0; offset < value.length; offset += blockSize) {
    const block = value.subarray(
      offset,
      Math.min(offset + blockSize, value.length),
    );
    binary += String.fromCharCode(...block);
  }
  return btoa(binary)
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/u, "");
}

export function base64UrlDecode(value: string): Uint8Array | null {
  if (!/^[A-Za-z0-9_-]*$/u.test(value) || value.length % 4 === 1) return null;
  const padded =
    value.replaceAll("-", "+").replaceAll("_", "/") +
    "=".repeat((4 - (value.length % 4)) % 4);
  try {
    const binary = atob(padded);
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) {
      bytes[index] = binary.charCodeAt(index);
    }
    return bytes;
  } catch {
    return null;
  }
}

export function isOpaqueId(value: string): boolean {
  return /^[A-Za-z0-9_-]{16,128}$/u.test(value);
}

export function isIdempotencyKey(value: string): boolean {
  return /^[A-Za-z0-9_-]{16,128}$/u.test(value);
}

export function randomId(byteLength = 24): string {
  const value = new Uint8Array(byteLength);
  crypto.getRandomValues(value);
  return base64UrlEncode(value);
}

export function encodeChangeCursor(sequence: number): string {
  return `c.${base64UrlEncode(utf8(JSON.stringify({ v: 1, a: sequence })))}`;
}

export async function sha256Base64Url(value: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", value);
  return base64UrlEncode(new Uint8Array(digest));
}

export function equalBytes(left: Uint8Array, right: Uint8Array): boolean {
  if (left.length !== right.length) return false;
  let difference = 0;
  for (let index = 0; index < left.length; index += 1) {
    const leftByte = left[index];
    const rightByte = right[index];
    if (leftByte === undefined || rightByte === undefined) return false;
    difference |= leftByte ^ rightByte;
  }
  return difference === 0;
}

export async function equalSecret(
  left: string,
  right: string,
): Promise<boolean> {
  const [leftDigest, rightDigest] = await Promise.all([
    crypto.subtle.digest("SHA-256", utf8(left)),
    crypto.subtle.digest("SHA-256", utf8(right)),
  ]);
  return equalBytes(new Uint8Array(leftDigest), new Uint8Array(rightDigest));
}

export function u32(value: number): Uint8Array {
  const bytes = new Uint8Array(4);
  new DataView(bytes.buffer).setUint32(0, value, false);
  return bytes;
}

export function concatBytes(parts: readonly Uint8Array[]): Uint8Array {
  const total = parts.reduce((sum, part) => sum + part.length, 0);
  const result = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    result.set(part, offset);
    offset += part.length;
  }
  return result;
}

export function byteLength(value: string): number {
  return utf8(value).length;
}

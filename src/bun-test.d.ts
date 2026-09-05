/**
 * Ambient types for `bun:test` so `tsc` type-checks test files that run under
 * Bun (see package.json "test:scripts" and the bun test workflow). The Bun
 * runtime provides these at test time; the declaration only satisfies the
 * compiler, which never executes them.
 */
declare module "bun:test" {
  /* `not` carries the same matchers as the positive form, so it is one named
   * shape rather than a bag of unknowns. Typed as `Record<string, unknown>` it
   * compiled to "Object is of type 'unknown'" at every `.not.toContain(...)`,
   * which is the assertion a test reaches for to pin a defect dead.
   *
   * The shape is parameterised by the value under assertion so the comparison
   * matchers take that same type: `expect(count).toBe("3")` is a mistake the
   * compiler can see, and it could not while `expected` was `unknown`. */
  interface Matchers<Actual> {
    toBe(expected: Actual): void;
    toEqual(expected: Actual): void;
    /* A string contains a substring; a collection contains one of its items. */
    toContain(
      expected: Actual extends readonly (infer Item)[] ? Item : string,
    ): void;
    toBeTruthy(): void;
    toBeFalsy(): void;
    toBeNull(): void;
    toBeDefined(): void;
    toBeUndefined(): void;
    toHaveLength(length: number): void;
    toBeGreaterThan(value: number): void;
    toBeLessThan(value: number): void;
    toMatch(regex: RegExp | string): void;
    /* Bun matches a thrown error by message substring, message pattern, or
     * equality with the error itself. */
    toThrow(expected?: string | RegExp | Error): void;
  }

  export function describe(name: string, fn: () => void): void;
  export function test(name: string, fn: () => void | Promise<void>): void;
  /* File-scoped lifecycle hooks; Bun also accepts async setup functions. */
  export function beforeAll(fn: () => void | Promise<void>): void;
  export function afterAll(fn: () => void | Promise<void>): void;
  /* Per-test lifecycle hooks, for state a test installs on a global and has
   * to hand back before the next file in the process sees it. */
  export function beforeEach(fn: () => void | Promise<void>): void;
  export function afterEach(fn: () => void | Promise<void>): void;
  export function expect<Actual>(
    value: Actual,
  ): Matchers<Actual> & { not: Matchers<Actual> };
}

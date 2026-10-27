import { describe, expect, test } from "bun:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { motion, spring, useReducedMotionConfig } from "motion/react";
import type { Transition } from "motion/react";
import { prefersReducedMotion } from "motion";
import {
  MotionProvider,
  MotionScope,
  SPRING_PRESETS,
  springDrag,
  springSnappy,
} from ".";
import { Disclosure } from "./Disclosure";

/* The interaction layer's standing proof.
 *
 * There is no DOM here — every component test in this repo renders to static
 * markup — so these tests drive Motion's own animators directly instead of
 * asserting on pixels. That is the stronger test anyway: the spring generator
 * and the JS animator are the exact code that runs in the app, and stepping
 * them by hand makes the timing assertions deterministic rather than flaky.
 *
 * A `window` has to exist for Motion to treat a render as a client render at
 * all — `createMotionComponent` skips its strict-mode check without one, which
 * would make the bundle guard below silently vacuous. The value is the same one
 * ModeEditor.test.tsx plants, so the two files can land in either order under a
 * whole-src run. `defineProperty`, not assignment: the slot is readonly once
 * another file has planted it. Motion captured `isBrowser` as false when it was
 * imported above, so its layout effects stay inert and the render stays SSR. */
Object.defineProperty(globalThis, "window", {
  configurable: true,
  value: { __TAURI_OS_PLUGIN_INTERNALS__: { os_type: "macos" } },
});

const TRAVEL = 100;


interface SpringProfile {
  /** ms after which the value is permanently within 1% of the target. */
  arrival99: number;
  /** ms until Motion's own rest thresholds report the spring finished. */
  settle: number;
  /** Peak excursion past the target, as a percentage of the travel. */
  overshoot: number;
}

const profile = (preset: Transition, velocity = 0): SpringProfile => {
  /* SAFETY: every SPRING_PRESETS entry is written as a spring literal with
   * these three fields; Motion's `Transition` union is what erases them. */
  const { stiffness, damping, mass } = preset as {
    stiffness: number;
    damping: number;
    mass: number;
  };
  const generator = spring({
    keyframes: [0, TRAVEL],
    stiffness,
    damping,
    mass,
    velocity,
  });
  let peak = 0;
  let arrival99 = -1;
  let settle = -1;
  for (let t = 0; t <= 3000; t += 1) {
    const state = generator.next(t);
    /* SAFETY: the generator above was built from numeric keyframes, so every
     * state it yields carries a number. */
    const value = state.value as number;
    peak = Math.max(peak, value);
    if (Math.abs(value - TRAVEL) <= TRAVEL / 100) {
      if (arrival99 < 0) arrival99 = t;
    } else {
      arrival99 = -1;
    }
    if (settle < 0 && state.done) settle = t;
  }
  return { arrival99, settle, overshoot: peak - TRAVEL };
};

describe("spring presets", () => {
  /* §3 of the design directive caps interaction motion at a duration a hand
   * reads as immediate. Springs have no duration, so the enforceable version
   * is arrival: the value has to be where it is going inside the band, and it
   * must not sail past and come back. */
  for (const [name, preset] of Object.entries(SPRING_PRESETS)) {
    test(`${name} arrives inside the 150-350ms band without visible overshoot`, () => {
      const measured = profile(preset);
      expect(measured.arrival99).toBeGreaterThan(149);
      expect(measured.arrival99).toBeLessThan(351);
      expect(measured.overshoot).toBeLessThan(1);
    });
  }


  /* The whole reason springDrag exists. A row released mid-flick hands its
   * velocity to the spring; the snappy tuning turns that into a bounce, and a
   * bounce on a list row is the toy this codebase refuses to be. */
  test("springDrag absorbs a fling where springSnappy rings", () => {
    const flick = 2400;
    expect(profile(springDrag, flick).overshoot).toBeLessThan(1);
    expect(profile(springSnappy, flick).overshoot).toBeGreaterThan(
      profile(springDrag, flick).overshoot,
    );
  });

});

describe("MotionProvider", () => {

  /* The bundle guard, asserted rather than trusted. `motion.*` components carry
   * every feature Motion has; if one ever lands in the eager tree the split is
   * gone and nothing else in the build would say so. Under `strict` it throws
   * on first render instead. */
  test("throws when a full-bundle motion component renders beneath it", () => {
    expect(() =>
      renderToStaticMarkup(
        <MotionProvider>
          <MotionScope>
            <motion.div />
          </MotionScope>
        </MotionProvider>,
      ),
    ).toThrow(/tree shaking/);
  });
});

/* `prefersReducedMotion` is Motion's own single source of truth for the device
 * setting — `initPrefersReducedMotion` writes it from matchMedia in a browser.
 * Writing it here is the emulation, and it is the same value the real hook
 * reads. */
const ReducedMotionProbe: React.FC = () => (
  <span>{String(useReducedMotionConfig())}</span>
);

describe("reduced motion", () => {
  test("the provider hands the decision to the device", () => {
    prefersReducedMotion.current = true;
    expect(
      renderToStaticMarkup(
        <MotionProvider>
          <ReducedMotionProbe />
        </MotionProvider>,
      ),
    ).toBe("<span>true</span>");
  });

  test("a device that has not asked for it animates normally", () => {
    prefersReducedMotion.current = false;
    expect(
      renderToStaticMarkup(
        <MotionProvider>
          <ReducedMotionProbe />
        </MotionProvider>,
      ),
    ).toBe("<span>false</span>");
  });

});

describe("Disclosure", () => {
  test("renders its content when open", () => {
    expect(
      renderToStaticMarkup(
        <MotionProvider>
          <Disclosure open id="panel">
            <p className="body" />
          </Disclosure>
        </MotionProvider>,
      ),
    ).toContain('<p class="body">');
  });

  test("renders nothing when closed", () => {
    expect(
      renderToStaticMarkup(
        <MotionProvider>
          <Disclosure open={false}>
            <p className="body" />
          </Disclosure>
        </MotionProvider>,
      ),
    ).toBe("");
  });

});


import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import {
  animateValue,
  m,
  motion,
  spring,
  useReducedMotionConfig,
} from "motion/react";
import type { Transition, ValueTransition } from "motion/react";
import {
  makeAnimationInstant,
  positionalKeys,
  prefersReducedMotion,
} from "motion";
import {
  MotionProvider,
  MotionScope,
  SPRING_MEASUREMENTS,
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

/** Steps an animation to completion with no rAF: one tick per simulated ms. */
const syncDriver = (limitMs: number) => (tick: (t: number) => void) => ({
  start: () => {
    for (let t = 0; t <= limitMs; t += 1) tick(t);
  },
  stop: () => undefined,
  now: () => 0,
});

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

  for (const [name, preset] of Object.entries(SPRING_PRESETS)) {
    test(`${name} matches the tuning quoted beside it`, () => {
      const measured = profile(preset);
      /* SAFETY: `name` came out of Object.entries(SPRING_PRESETS) two lines
       * up, and SPRING_MEASUREMENTS is keyed by the same preset names. */
      const quoted = SPRING_MEASUREMENTS[name as keyof typeof SPRING_PRESETS];
      expect(measured.arrival99).toBe(quoted.arrival99);
      expect(measured.settle).toBe(quoted.settle);
      expect(Number(measured.overshoot.toFixed(2))).toBe(quoted.overshoot);
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

  test("springDrag arrives sooner when thrown than when nudged", () => {
    expect(profile(springDrag, 2400).arrival99).toBeLessThan(
      profile(springDrag).arrival99,
    );
  });
});

describe("MotionProvider", () => {
  test("renders m components", () => {
    const markup = renderToStaticMarkup(
      <MotionProvider>
        <m.div className="probe" />
      </MotionProvider>,
    );
    expect(markup).toContain('class="probe"');
  });

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

  /* Motion's default is `reducedMotion: "never"`, so without the provider's
   * config the same device preference is ignored. This is what proves the
   * provider is doing the work rather than the library defaulting our way. */
  test("without the provider the device preference is ignored", () => {
    prefersReducedMotion.current = true;
    expect(renderToStaticMarkup(<ReducedMotionProbe />)).toBe(
      "<span>false</span>",
    );
  });

  /**
   * The two steps Motion takes for a reduced-motion device, both quoted from
   * the library rather than restated: `visual-element-target` swaps a
   * positional key's transition for `{ type: false }`, and `animateMotionValue`
   * collapses that to a single final keyframe through `makeAnimationInstant`.
   * `positionalKeys` and `makeAnimationInstant` are Motion's own, so the only
   * thing standing in for the DOM here is the wiring between them.
   */
  const run = (key: string, preset: Transition, reduce: boolean) => {
    const reduceThisKey = reduce && positionalKeys.has(key);
    const options = {
      keyframes: [0, TRAVEL],
      driver: syncDriver(1200),
      onUpdate: (value: number) => frames.push(value),
      /* SAFETY: both branches are already valid `ValueTransition` shapes —
       * `{ type: false }` is Motion's own instant form, and a preset is a
       * spring transition. The assertions only stop the spread from widening
       * the object literal's inferred type. */
      ...(reduceThisKey
        ? ({ type: false } as ValueTransition)
        : (preset as ValueTransition)),
    };
    const frames: number[] = [];
    if (options.type === false) makeAnimationInstant(options);
    animateValue(options);
    return frames;
  };

  for (const key of ["scale", "height", "x"]) {
    test(`a preset spring on ${key} resolves instantly under reduced motion`, () => {
      prefersReducedMotion.current = null;
      expect(run(key, springSnappy, true)).toEqual([TRAVEL]);
    });
  }

  for (const key of ["scale", "height", "x"]) {
    test(`the same spring on ${key} takes real time when nobody asked to reduce it`, () => {
      prefersReducedMotion.current = null;
      const frames = run(key, springSnappy, false);
      expect(frames.length).toBeGreaterThan(10);
      expect(frames[0]).toBe(0);
      expect(frames[frames.length - 1]).toBe(TRAVEL);
    });
  }

  /* Reduced motion is not "no feedback": a cross-fade carries no movement, so
   * opacity keeps its spring and the surface still acknowledges the command. */
  test("opacity still animates under reduced motion", () => {
    expect(run("opacity", springSnappy, true).length).toBeGreaterThan(10);
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

  test("clips while it travels, so a spring cannot spill content", () => {
    expect(
      renderToStaticMarkup(
        <MotionProvider>
          <Disclosure open>
            <p />
          </Disclosure>
        </MotionProvider>,
      ),
    ).toContain("overflow:hidden");
  });
});

/* ------------------------------------------------ the measured-value law */

const SRC = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "..");

const sourceFiles = (dir: string): string[] =>
  fs
    .readdirSync(dir, { withFileTypes: true })
    .flatMap((entry) =>
      entry.isDirectory()
        ? sourceFiles(path.join(dir, entry.name))
        : /\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name)
          ? [path.join(dir, entry.name)]
          : [],
    );

/** Comments discuss the law; only code can break it. */
const code = (source: string) =>
  source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");

const IMPORTS_MOTION =
  /from\s+"(motion|motion\/react|motion-dom|framer-motion|@\/lib\/motion)"/;
const APPLIES_MEASURED_CLASS = /snap-measured/;

const MOTION_SURFACES = [
  "components/settings/modes/ModesReorder.tsx",
  "lib/motion/index.tsx",
  "lib/motion/Disclosure.tsx",
  "lib/motion/provider.tsx",
];

describe("measured values never tween", () => {
  /* theme.css kills `transition` and `animation` on the measured-value class,
   * and that was enough while CSS was the only thing that could move a value.
   * Motion writes style values frame by frame, so CSS cannot stop it — a meter
   * inside a motion component would display numbers the backend never
   * reported, with the guard rule sitting right there looking satisfied.
   *
   * The enforceable version is at file scope: a module either renders measured
   * values or it animates with Motion, never both. That is coarser than a
   * subtree check and deliberately so — it needs no parser, it cannot be
   * defeated by moving an element one level up, and the fix when it fires is
   * the one the law wants anyway, which is to split the component. */
  test("no module both animates with Motion and renders a measured value", () => {
    const offenders = sourceFiles(SRC)
      .filter((file) => {
        const source = code(fs.readFileSync(file, "utf8"));
        return (
          IMPORTS_MOTION.test(source) && APPLIES_MEASURED_CLASS.test(source)
        );
      })
      .map((file) => path.relative(SRC, file));
    expect(offenders).toEqual([]);
  });

  for (const relative of MOTION_SURFACES) {
    test(`${relative} is a motion surface`, () => {
      const source = code(fs.readFileSync(path.join(SRC, relative), "utf8"));
      expect(IMPORTS_MOTION.test(source)).toBe(true);
    });
  }

  /* ModesList is the loader rather than a motion surface, but it renders every
   * row body for both lists, so it is the file a measured value would most
   * plausibly be added to. */
  for (const relative of [
    ...MOTION_SURFACES,
    "components/settings/modes/ModesList.tsx",
    "components/CommandPalette.tsx",
  ]) {
    test(`${relative} carries no measured value`, () => {
      const source = code(fs.readFileSync(path.join(SRC, relative), "utf8"));
      expect(APPLIES_MEASURED_CLASS.test(source)).toBe(false);
    });
  }
});

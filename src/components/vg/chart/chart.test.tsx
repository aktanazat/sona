import { describe, expect, test } from "bun:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { Bars, Ring, Sparkline } from ".";

const render = (node: React.ReactElement): string => renderToStaticMarkup(node);

describe("Bars", () => {
  test("maps known values to deterministic rect geometry", () => {
    const markup = render(
      <Bars
        values={[0, 5, 10]}
        highlightIndex={2}
        ariaLabel="Dictations per day, highest 10 on Thursday"
      />,
    );

    expect(markup).toContain(
      'aria-label="Dictations per day, highest 10 on Thursday"',
    );
    expect(markup).toContain(
      '<rect x="0" y="64" width="32" height="0" rx="2" ry="2"',
    );
    expect(markup).toContain(
      '<rect x="34" y="32" width="32" height="32" rx="2" ry="2"',
    );
    expect(markup).toContain(
      '<rect x="68" y="0" width="32" height="64" rx="2" ry="2"',
    );
  });
});

describe("Sparkline", () => {
  test("maps known values to a line, area and terminal dot", () => {
    const markup = render(
      <Sparkline
        values={[0, 5, 10]}
        area
        ariaLabel="Words per day, 15 total, ending at 10"
      />,
    );

    expect(markup).toContain(
      'aria-label="Words per day, 15 total, ending at 10"',
    );
    expect(markup).toContain('points="0,62 50,32 100,2"');
    expect(markup).toContain('d="M 0 64 L 0 62 L 50 32 L 100 2 L 100 64 Z"');
    expect(markup).toContain('<circle cx="100" cy="2" r="2"');
  });

  test("normalizes opposite finite extremes without invalid SVG geometry", () => {
    const markup = render(
      <Sparkline
        values={[Number.MAX_VALUE, -Number.MAX_VALUE]}
        ariaLabel="Opposite finite extremes"
      />,
    );

    expect(markup).toContain('points="0,2 100,62"');
    expect(markup.includes("NaN")).toBe(false);
    expect(markup.includes("Infinity")).toBe(false);
  });

  test("renders non-finite samples at zero without changing series length", () => {
    const markup = render(
      <Sparkline
        values={[Number.POSITIVE_INFINITY, 1, Number.NaN]}
        ariaLabel="Series with unavailable samples"
      />,
    );

    expect(markup).toContain('points="0,62 50,2 100,62"');
    expect(markup.includes("NaN")).toBe(false);
    expect(markup.includes("Infinity")).toBe(false);
  });
});

describe("Ring", () => {
  test("maps a known fraction to one rounded value arc", () => {
    const markup = render(
      <Ring
        value={25}
        max={100}
        center="25"
        ariaLabel="Current streak, 25 days"
      />,
    );

    expect(markup).toContain('aria-label="Current streak, 25 days"');
    expect(markup).toContain('stroke-dasharray="45.553 182.212"');
  });
});

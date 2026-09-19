import { expect, test } from "@playwright/test";

test("FAQ slides and fades both ways, including interrupted toggles", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.goto("/#faq");
  const item = page.locator(".faq-item").nth(1);
  for (const opening of [true, false]) {
    const result = await item.evaluate(async (element) => {
      const before = element.getBoundingClientRect().height;
      element.querySelector("summary")!.click();
      const duration = parseFloat(getComputedStyle(element, "::details-content").transitionDuration) * 1000;
      const samples: { height: number; opacity: number }[] = [];
      // Chromium doesn't expose ::details-content transitions in getAnimations().
      // Observe rendered frames instead, without asserting a specific frame rate.
      const start = performance.now();
      while (performance.now() - start < duration + 100) {
        await new Promise(requestAnimationFrame);
        samples.push({ height: element.getBoundingClientRect().height,
          opacity: Number(getComputedStyle(element, "::details-content").opacity) });
      }
      return { before, samples, after: element.getBoundingClientRect().height, duration };
    });
    expect(result.duration).toBe(opening ? 240 : 180);
    expect(result.samples.some((sample) => sample.height > Math.min(result.before, result.after)
      && sample.height < Math.max(result.before, result.after))).toBe(true);
    expect(result.samples.some((sample) => sample.opacity > 0 && sample.opacity < 1)).toBe(true);
  }
  const reversed = await item.evaluate(async (element) => {
    const summary = element.querySelector("summary")!;
    const closed = element.getBoundingClientRect().height;
    summary.click();
    const start = performance.now();
    do { await new Promise(requestAnimationFrame); }
    while (element.getBoundingClientRect().height <= closed && performance.now() - start < 1000);
    const middle = element.getBoundingClientRect().height;
    summary.click();
    const reverseStart = element.getBoundingClientRect().height;
    do { await new Promise(requestAnimationFrame); }
    while (element.getBoundingClientRect().height > closed && performance.now() - start < 1500);
    return { closed, middle, reverseStart, end: element.getBoundingClientRect().height };
  });
  expect(reversed.middle).toBeGreaterThan(reversed.closed);
  expect(Math.abs(reversed.reverseStart - reversed.middle)).toBeLessThan(1);
  expect(reversed.end).toBeCloseTo(reversed.closed, 1);
  await expect(item).not.toHaveAttribute("open");
  await expect(item.locator(".faq-answer")).toBeHidden();
});

test("FAQ reduced motion skips the content transition", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/#faq");
  const item = page.locator(".faq-item").nth(1);
  for (const opening of [true, false]) {
    await item.locator("summary").click();
    const durations = await item.evaluate((element) => getComputedStyle(element, "::details-content").transitionDuration);
    expect(durations.split(",").every((duration) => parseFloat(duration) === 0)).toBe(true);
    if (opening) await expect(item.locator(".faq-answer")).toBeVisible();
    else await expect(item.locator(".faq-answer")).toBeHidden();
  }
});

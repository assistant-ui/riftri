import { expect, test } from "@playwright/test";

const install = "curl -fsSL https://riftri.dev/install.sh | bash";

test("footer stays compact with comfortable link targets", async ({ page }, testInfo) => {
  await page.goto("/");
  const footer = page.getByRole("contentinfo");
  const heights = await footer.getByRole("link").evaluateAll((links) =>
    links.map((link) => link.getBoundingClientRect().height));
  expect(heights).toHaveLength(3);
  const rowHeight = testInfo.project.name === "mobile" ? 48 : 60;
  for (const height of heights) {
    expect(height).toBeGreaterThanOrEqual(44);
    expect(height).toBeLessThanOrEqual(rowHeight);
  }
  await footer.screenshot({ path: testInfo.outputPath(`footer-${testInfo.project.name}.png`) });
});

test("homepage renders without browser errors and captures the final layout", async ({ page }, testInfo) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => { if (message.type() === "error") errors.push(message.text()); });
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Git worktrees. Shared storage.", exact: true })).toBeVisible();
  await expect(page.locator(".hero-highlight")).toHaveText("storage.");
  await expect(page.locator(".hero-highlight")).toHaveCSS("background-color", "rgb(240, 106, 58)");
  await expect(page.locator(".hero-graph")).toHaveCSS("background-image", "none");
  await expect(page.locator(".hero-graph")).toHaveCSS("background-color", "rgb(5, 5, 5)");
  const frames = page.locator(".graph-frame");
  await expect(frames).toHaveCount(3);
  for (const frame of await frames.all()) {
    for (const side of ["top", "right", "bottom", "left"]) {
      await expect(frame).toHaveCSS(`border-${side}-style`, "dashed");
    }
  }
  await expect(page.getByRole("heading", { name: "Worktree disk usage" })).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  const clipped = await page.locator(".site-header a, .storage-backends, .hero-highlight, .hero-install, .command, .graph-frame, .frame-title, .diagram-controls").evaluateAll((elements) =>
    elements.filter((element) => {
      const rect = element.getBoundingClientRect();
      return rect.width > 0 && (rect.left < -1 || rect.right > innerWidth + 1);
    }).map((element) => element.className));
  expect(clipped).toEqual([]);
  expect(errors).toEqual([]);
  await page.screenshot({ path: testInfo.outputPath(`hero-${testInfo.project.name}.png`) });
  await page.screenshot({ path: testInfo.outputPath(`homepage-${testInfo.project.name}.png`), fullPage: true });
});

test("header links and skip link transfer keyboard focus to their destinations", async ({ page }) => {
  await page.goto("/");
  await page.keyboard.press("Tab");
  await expect(page.getByRole("link", { name: "Skip to content" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.locator("#main-content")).toBeFocused();
  const navigation = page.getByRole("navigation", { name: "Main navigation" });
  for (const [name, id] of [["How it works", "overview"], ["Savings", "savings"], ["FAQ", "faq"]]) {
    const link = navigation.getByRole("link", { name, exact: true });
    await link.focus();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(new RegExp(`#${id}$`));
    await expect(page.locator(`#${id}`)).toBeFocused();
  }
  await page.getByRole("link", { name: "Riftri home" }).click();
  await expect(page.locator("#top")).toBeFocused();
});

test("Get started transfers keyboard focus and continues inside quick start", async ({ page }) => {
  await page.goto("/");
  const link = page.getByRole("link", { name: "Get started" });
  await link.focus();
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/#start$/);
  await expect(page.locator("#start")).toBeFocused();
  await page.keyboard.press("Tab");
  // Chromium makes overflowing code keyboard-scrollable at narrow widths.
  const code = page.locator("#start .command code").first();
  if (await code.evaluate((element) => element === document.activeElement)) {
    await page.keyboard.press("Tab");
  }
  await expect(page.locator("#start").getByRole("button", { name: `Copy command: ${install}`, exact: true })).toBeFocused();
});

test("breakpoint edges keep framed content inside the viewport", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "exercise exact CSS breakpoints once");
  for (const width of [320, 430, 760, 761, 1040, 1041]) {
    await page.setViewportSize({ width, height: 900 });
    await page.goto("/");
    await page.evaluate(() => document.fonts.ready);
    const clipped = await page.locator(".site-header a, .storage-backends, .hero-highlight, .hero-install, .command, .graph-frame, .frame-title, .diagram-controls, .faq-item summary, .faq-answer").evaluateAll((elements) =>
      elements.filter((element) => {
        const rect = element.getBoundingClientRect();
        return rect.width > 0 && (rect.left < -1 || rect.right > innerWidth + 1);
      }).map((element) => element.className));
    expect(clipped, `clipped content at ${width}px`).toEqual([]);
  }
});

test("FAQ answers toggle with the keyboard and keep focus on the question", async ({ page }, testInfo) => {
  await page.goto("/#faq");
  const faq = page.getByRole("region", { name: "Common questions" });
  const first = faq.locator("details").first();
  const second = faq.locator("details").nth(1);
  await expect(first.locator(".faq-answer")).toBeVisible();
  await expect(second.locator(".faq-answer")).toBeHidden();
  const question = first.locator("summary");
  await expect(question.locator(".faq-prompt")).toHaveText(">");
  await expect(question.locator(".faq-prompt")).toHaveAttribute("aria-hidden", "true");
  await expect(question.locator(".faq-prompt")).toHaveCSS("color", "rgb(240, 106, 58)");
  await expect(question.locator(".faq-toggle")).toHaveAttribute("aria-hidden", "true");
  await expect(first.locator(".faq-minus")).toHaveText("[−]");
  await expect(first.locator(".faq-minus")).toBeVisible();
  await expect(first.locator(".faq-plus")).toBeHidden();
  await expect(second.locator(".faq-plus")).toHaveText("[+]");
  await expect(second.locator(".faq-plus")).toBeVisible();
  await question.focus();
  await page.keyboard.press("Enter");
  await expect(first.locator(".faq-answer")).toBeHidden();
  await expect(first.locator(".faq-plus")).toBeVisible();
  await expect(first.locator(".faq-minus")).toBeHidden();
  await expect(question).toBeFocused();
  await page.keyboard.press("Space");
  await expect(first.locator(".faq-answer")).toBeVisible();
  await expect(first.locator(".faq-minus")).toBeVisible();
  await page.keyboard.press("Tab");
  await expect(second.locator("summary")).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(second.locator(".faq-answer")).toBeVisible();
  await expect(first.locator(".faq-answer")).toBeVisible();
  await faq.screenshot({ path: testInfo.outputPath(`faq-${testInfo.project.name}.png`) });
});

test.describe("FAQ without JavaScript", () => {
  test.use({ javaScriptEnabled: false });

  test("static answers remain expandable with usable documentation links", async ({ page }) => {
    await page.goto("/#faq");
    const faq = page.getByRole("region", { name: "Common questions" });
    const agentQuestion = faq.locator("details").filter({ hasText: "Does my coding agent need special integration?" });
    await expect(agentQuestion.locator(".faq-answer")).toBeHidden();
    await agentQuestion.locator("summary").click();
    await expect(agentQuestion.getByRole("link", { name: "agent setup" })).toHaveAttribute(
      "href", "https://github.com/assistant-ui/riftri/blob/main/docs/agent-integration.md",
    );
    await expect(agentQuestion.locator(".faq-answer")).toBeVisible();
    await agentQuestion.locator("summary").click();
    await expect(agentQuestion.locator(".faq-answer")).toBeHidden();
  });
});

test("repeated clipboard successes each retain a full confirmation interval", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await page.clock.install();
  await page.goto("/");
  await page.clock.pauseAt(new Date(Date.now() + 60_000));
  const button = page.locator(".hero-install").getByRole("button");
  await button.click();
  await expect(button).toHaveText("COPIED");
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(install);
  await page.clock.runFor(1500);
  await button.click();
  await expect(button).toHaveText("COPIED");
  await page.clock.runFor(500);
  await expect(button).toHaveText("COPIED");
  await page.clock.runFor(1400);
  await expect(button).toHaveText("COPY");
});

test("clipboard denial offers manual copy and a usable retry", async ({ page }) => {
  await page.addInitScript(() => {
    let attempts = 0;
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: {
      writeText: async () => { if (++attempts === 1) throw new Error("Denied for test"); },
    } });
  });
  await page.goto("/");
  const command = page.locator(".hero-install");
  await command.getByRole("button").click();
  await expect(command.getByRole("status")).toContainText("Select the command");
  await expect(command.getByRole("button")).toHaveAccessibleName(`Retry copying command: ${install}`);
  await command.getByRole("button").click();
  await expect(command.getByRole("status")).toHaveText("Command copied.");
});

test("Markdown copy control copies the whole guide without navigating", async ({ page, request, context }) => {
  const markdown = await request.get("/index.md");
  expect(markdown.status()).toBe(200);
  expect(markdown.headers()["content-type"]).toContain("text/plain");
  expect(markdown.headers()["content-disposition"]).toContain("inline");
  const guide = await markdown.text();
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  let downloads = 0;
  page.on("download", () => downloads++);
  await page.goto("/");
  const count = context.pages().length;
  const copy = page.getByRole("button", { name: "Copy the full Markdown guide for your agent" });
  await copy.click();
  await expect(copy).toHaveText("COPIED");
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  expect(copied).toBe(guide);
  expect(copied).toContain("# Riftri");
  await expect(page).toHaveURL("/");
  expect(downloads).toBe(0);
  expect(context.pages()).toHaveLength(count);
});

test("materialization backend cycles without a progress indicator", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.goto("/#overview");
  const decoration = await page.locator(".backend-cycle, .backend-cycle-item").evaluateAll((elements) =>
    elements.map((element) => getComputedStyle(element, "::after").content));
  expect(decoration.every((content) => content === "none")).toBe(true);
  await expect(page.locator(".backend-cycle-item").first()).toHaveCSS("animation-name", "backend-cycle");
});

test("savings keeps the figures and source link without the extra benchmark notes", async ({ page }, testInfo) => {
  await page.goto("/#savings");
  const chart = page.locator(".savings-map");
  await expect(chart.locator("details")).toHaveCount(0);
  await expect(chart).not.toContainText(/Creation time:|12 Sep 2026|linguist-generated|Backend names show support|Results vary/);
  await expect(chart).toContainText("APFS reference measurement");
  await expect(chart).toContainText("87.0");
  await expect(chart.getByRole("link", { name: "Read full benchmark" })).toHaveAttribute("href", "https://github.com/assistant-ui/riftri/blob/main/docs/benchmarks/assistant-ui-ten-agents-2026-09-12.md");
  await chart.screenshot({ path: testInfo.outputPath(`savings-clean-${testInfo.project.name}.png`) });
});

test("savings underline follows the width of every animated filesystem name", async ({ page }, testInfo) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.goto("/#savings");
  await page.evaluate(() => document.fonts.ready);
  const cycle = page.locator(".savings-backend-cycle");
  await expect(cycle).toHaveCSS("border-bottom-width", "0px");
  const frames = ["APFS", "Linux reflink", "ReFS"];
  const widths: number[] = [];
  const containerWidths: number[] = [];
  for (const [index, name] of frames.entries()) {
    await page.locator(".savings-backend-item").evaluateAll((elements, time) => {
      for (const element of elements) {
        for (const animation of element.getAnimations()) {
          animation.pause();
          animation.currentTime = time;
        }
      }
    }, 1000 + index * 4000);
    const item = cycle.getByText(name, { exact: true });
    await expect(item).toHaveCSS("opacity", "1");
    await expect(item).toHaveCSS("border-bottom-style", "dotted");
    await expect(item).toHaveCSS("border-bottom-width", "1px");
    const bounds = await item.evaluate((element) => {
      const range = document.createRange();
      range.selectNodeContents(element);
      return { item: element.getBoundingClientRect().width, text: range.getBoundingClientRect().width };
    });
    expect(Math.abs(bounds.item - bounds.text)).toBeLessThanOrEqual(1);
    widths.push(bounds.item);
    containerWidths.push(await cycle.evaluate((element) => element.getBoundingClientRect().width));
    await page.locator(".savings-map").screenshot({ path: testInfo.outputPath(`savings-${index}-${testInfo.project.name}.png`) });
  }
  expect(widths[1]).toBeGreaterThan(widths[0]);
  expect(widths[1]).toBeGreaterThan(widths[2]);
  expect(new Set(containerWidths).size).toBe(1);
});

test("wrapped backend status stays inside the animated diagram", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "exercise the narrow four-column layout once");
  await page.setViewportSize({ width: 761, height: 1000 });
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.goto("/");
  await page.evaluate(() => document.fonts.ready);
  const item = page.locator(".backend-cycle-item").nth(1);
  // Freeze the natural Linux frame only in the test; the diagram has no pause control.
  await expect.poll(() => item.evaluate((element) => {
    if (getComputedStyle(element).opacity !== "1") return false;
    for (const animation of element.getAnimations()) animation.pause();
    return true;
  }), { timeout: 12_000 }).toBe(true);
  await expect(item).toHaveCSS("opacity", "1");
  const overflow = await item.locator("small").evaluate((label) => {
    const clip = label.closest(".backend-cycle");
    if (!clip) throw new Error("Missing backend cycle");
    const range = document.createRange();
    range.selectNodeContents(label);
    const text = range.getBoundingClientRect();
    const bounds = clip.getBoundingClientRect();
    return Math.max(bounds.top - text.top, text.bottom - bounds.bottom);
  });
  expect(overflow).toBeLessThanOrEqual(1);
});

test("reduced motion stops loops while preserving readable diagram content", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator(".storage-map, .materialization-map").getByRole("button")).toHaveCount(0);
  const names = await page.locator(".track-counter, .backend-cycle-item, .savings-backend-item").evaluateAll((elements) => elements.map((element) => getComputedStyle(element).animationName));
  expect(names.every((name) => name === "none")).toBe(true);
  const underline = await page.locator(".savings-backend-item").first().evaluate((element) => {
    const range = document.createRange();
    range.selectNodeContents(element);
    return {
      border: getComputedStyle(element).borderBottomStyle,
      excess: element.getBoundingClientRect().width - range.getBoundingClientRect().width,
    };
  });
  expect(underline.border).toBe("dotted");
  expect(Math.abs(underline.excess)).toBeLessThanOrEqual(1);
  await expect(page.locator(".materialization-map")).toContainText("FROM TREE TO WORKSPACE");
});

test("Windows onboarding separates review from running the installer", async ({ page }, testInfo) => {
  await page.goto("/#start");
  await page.getByText("Windows / PowerShell", { exact: true }).click();
  const windows = page.locator(".windows-install");
  await expect(windows).toContainText("ReFS volume, not ordinary NTFS");
  await expect(windows.getByRole("button")).toHaveCount(2);
  await expect(windows.locator("code").first()).toContainText("Get-Content $Installer");
  await expect(windows.locator("code").first()).not.toContainText("& $Installer");
  await expect(windows.locator("code").last()).toHaveText("& $Installer");
  await expect(windows.locator("code").first()).toHaveCSS("white-space", "pre-wrap");
  expect(await windows.evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(true);
  await page.locator("#start").screenshot({ path: testInfo.outputPath(`windows-${testInfo.project.name}.png`) });
});

test("rendered sharing metadata uses the canonical public URL", async ({ page, request }) => {
  await page.goto("/");
  await expect(page.locator('link[rel="canonical"]')).toHaveAttribute("href", "https://riftri.dev/");
  await expect(page.locator('meta[property="og:image"]')).toHaveAttribute("content", "https://riftri.dev/og.png");
  await expect(page.locator('meta[name="twitter:card"]')).toHaveAttribute("content", "summary_large_image");
  const card = await request.get("/og.png");
  expect(card.ok()).toBe(true);
  expect(card.headers()["content-type"]).toBe("image/png");
});

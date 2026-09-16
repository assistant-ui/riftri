import { expect, test } from "@playwright/test";

const install = "curl -fsSL https://riftri.dev/install.sh | bash";

test("homepage renders without browser errors and captures the final layout", async ({ page }, testInfo) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => { if (message.type() === "error") errors.push(message.text()); });
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Riftri", exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Worktree disk usage" })).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  const clipped = await page.locator(".hero-install, .command, .graph-frame, .frame-title, .diagram-controls").evaluateAll((elements) =>
    elements.filter((element) => {
      const rect = element.getBoundingClientRect();
      return rect.width > 0 && (rect.left < -1 || rect.right > innerWidth + 1);
    }).map((element) => element.className));
  expect(clipped).toEqual([]);
  expect(errors).toEqual([]);
  await page.screenshot({ path: testInfo.outputPath(`hero-${testInfo.project.name}.png`) });
  await page.screenshot({ path: testInfo.outputPath(`homepage-${testInfo.project.name}.png`), fullPage: true });
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
    const clipped = await page.locator(".hero-install, .command, .graph-frame, .frame-title, .diagram-controls").evaluateAll((elements) =>
      elements.filter((element) => {
        const rect = element.getBoundingClientRect();
        return rect.width > 0 && (rect.left < -1 || rect.right > innerWidth + 1);
      }).map((element) => element.className));
    expect(clipped, `clipped content at ${width}px`).toEqual([]);
  }
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

test("Markdown link navigates in the same tab without a download", async ({ page, request, context }) => {
  const markdown = await request.get("/index.md");
  expect(markdown.status()).toBe(200);
  expect(markdown.headers()["content-type"]).toContain("text/plain");
  expect(markdown.headers()["content-disposition"]).toContain("inline");
  // Keep CI independent of production: serve the exact built response at the canonical URL.
  await page.route("https://riftri.dev/index.md", (route) => route.fulfill({ response: markdown }));
  let downloads = 0;
  page.on("download", () => downloads++);
  await page.goto("/");
  const count = context.pages().length;
  await page.getByRole("link", { name: "Open Markdown guide" }).click();
  await expect(page).toHaveURL("https://riftri.dev/index.md");
  await expect(page.locator("body")).toContainText("# Riftri");
  expect(downloads).toBe(0);
  expect(context.pages()).toHaveLength(count);
});

test("both example diagrams can pause and resume", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.goto("/");
  for (const name of ["worktree example", "storage backend"]) {
    const button = page.getByRole("button", { name: `Pause ${name} animation`, exact: true });
    await expect(button).toBeEnabled();
    await button.click();
    await expect(button).toHaveAttribute("aria-pressed", "true");
    const figure = button.locator("xpath=ancestor::figure");
    await expect(figure).toHaveAttribute("data-paused", "true");
    const states = await figure.locator(".track-counter, .backend-cycle-item").evaluateAll((elements) => elements.map((element) => getComputedStyle(element).animationPlayState));
    expect(states.every((state) => state === "paused")).toBe(true);
    await button.click();
    await expect(button).toHaveAttribute("aria-pressed", "false");
  }
});

test("reduced motion stops loops while preserving readable diagram content", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("button", { name: /motion disabled by preference/ })).toHaveCount(2);
  const names = await page.locator(".track-counter, .backend-cycle-item, .savings-backend-item").evaluateAll((elements) => elements.map((element) => getComputedStyle(element).animationName));
  expect(names.every((name) => name === "none")).toBe(true);
  await expect(page.getByText("APFS · Linux · ReFS", { exact: true })).toBeVisible();
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

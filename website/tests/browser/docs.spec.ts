import { expect, test } from "@playwright/test";
import groups from "../../content/docs.json" with { type: "json" };

test("human docs do not bundle full agent references into browser JavaScript", async ({ page, request }, info) => {
  test.skip(info.project.name !== "desktop", "same JavaScript build for every viewport");
  await page.goto("/docs");
  await expect(page.locator("#nd-page h1")).toBeVisible();
  const scripts = await page.locator("script[src]").evaluateAll((elements) =>
    elements.map((el) => (el as HTMLScriptElement).src),
  );
  expect(scripts.length).toBeGreaterThan(0);
  for (const url of scripts) {
    const response = await request.get(url);
    expect(response.ok()).toBe(true);
    const javascript = await response.text();
    expect(javascript.includes("FSCTL_DUPLICATE_EXTENTS_TO_FILE"), url).toBe(false);
    expect(javascript.includes("Full technical reference for agents and readers"), url).toBe(false);
  }
});

test("docs header reuses the landing page logo and wordmark", async ({ page }, info) => {
  await page.goto("/");
  const homeBrand = page.locator(".site-brand");
  const logo = await homeBrand.locator("img").getAttribute("src");
  const font = await homeBrand.evaluate((el) => getComputedStyle(el).fontFamily);
  await page.goto("/docs");
  const brand = page.locator(info.project.name === "mobile" ? "#nd-subnav > a" : "#nd-sidebar > div:first-child a");
  await expect(brand).toHaveText("Riftri");
  await expect(brand).toHaveAttribute("href", "/");
  await expect(brand).toHaveCSS("font-family", font);
  await expect(brand).toHaveCSS("font-size", "18px");
  const mark = await brand.evaluate((el) => {
    const style = getComputedStyle(el, "::before");
    return { image: style.backgroundImage, width: style.width, height: style.height, content: style.content };
  });
  expect(mark.image).toContain(logo);
  expect(mark.width).toBe("22px");
  expect(mark.height).toBe("22px");
  expect(mark.content).toBe('\"\"');
  await page.screenshot({ path: info.outputPath(`docs-brand-${info.project.name}.png`) });
  await brand.click();
  await expect(page).toHaveURL(/\/$/);
  await expect(page.locator(".site-brand")).toBeVisible();
});

test("page actions are one View/Copy row below the intro, above the first section", async ({ page, context }, info) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  let downloaded = false;
  page.on("download", () => { downloaded = true; });
  for (const slug of ["", "/installation"]) {
    await page.goto(`/docs${slug}`);
    const view = page.getByRole("link", { name: /^view \.md$/i });
    const copy = page.getByRole("link", { name: /^copy \.md$/i });
    // Both render as plain Markdown links (no framework button); "Copy .md" is
    // upgraded to a real clipboard copy by the client script, and its href is
    // the fallback when clipboard access is unavailable.
    await expect(page.getByRole("button", { name: /copy page|copy markdown|copy \.md/i })).toHaveCount(0);
    await expect(view).toHaveAttribute("href", `/docs${slug}.md`);
    await expect(copy).toHaveAttribute("href", `/docs${slug}.md`);
    // View and Copy sit on one right-aligned row, vertically centered with the
    // "/" separator between them.
    const layout = await view.evaluate((v) => {
      const c = document.querySelectorAll('#nd-page a[title="Copy this page as Markdown"]')[0] as HTMLElement;
      const row = v.closest("p") as HTMLElement;
      const vr = v.getBoundingClientRect(), cr = c.getBoundingClientRect();
      return {
        sameRow: Math.abs((vr.top + vr.height / 2) - (cr.top + cr.height / 2)) <= 1,
        viewLeftOfCopy: vr.right <= cr.left,
        rightAligned: getComputedStyle(row).justifyContent === "flex-end",
        aligned: getComputedStyle(row).alignItems === "center",
      };
    });
    expect(layout.sameRow).toBe(true);
    expect(layout.viewLeftOfCopy).toBe(true);
    expect(layout.rightAligned).toBe(true);
    expect(layout.aligned).toBe(true);
    // The row is below the intro paragraph and above the first section heading.
    const order = await page.evaluate(() => {
      const nodes = [...document.querySelectorAll("#nd-page .fd-docs-content > *")];
      const h1 = nodes.findIndex((n) => n.tagName === "H1");
      const row = nodes.findIndex((n) => n.querySelector?.('a[title="View this page as Markdown"]'));
      const h2 = nodes.findIndex((n, i) => i > h1 && n.tagName === "H2");
      const introBefore = nodes.slice(h1 + 1, row).some((n) => n.tagName === "P");
      return { row, h2, introBefore };
    });
    expect(order.introBefore).toBe(true);
    expect(order.h2 === -1 || order.row < order.h2).toBe(true);
    await view.scrollIntoViewIfNeeded();
    await page.screenshot({ path: info.outputPath(`page-action${slug ? "-installation" : ""}-${info.project.name}.png`) });
    for (const link of [view, copy]) {
      await expect(link).toHaveCSS("text-transform", "uppercase");
      await expect(link).toHaveCSS("font-family", /Geist Mono/);
      const icon = await link.evaluate((el) => getComputedStyle(el, "::before").maskImage);
      expect(icon).toContain("/docs-icons/");
    }
    // Copy .md writes this page's Markdown (with frontmatter) to the clipboard
    // and stays on the page instead of navigating to the `.md`.
    await copy.click();
    await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toMatch(/^---\ntitle:/);
    await expect(page).toHaveURL(new RegExp(`/docs${slug}$`));
    expect(downloaded).toBe(false);
    // View .md opens the Markdown.
    await view.click();
    await expect(page).toHaveURL(new RegExp(`/docs${slug}\\.md$`));
    await page.goBack();
    const edit = page.getByRole("link", { name: /^edit on github$/i });
    await expect(edit).toHaveAttribute("href", `https://github.com/assistant-ui/riftri/blob/main/${slug ? "website/content/guides/installation.md" : "website/content/introduction.md"}`);
    await expect(edit).toHaveCSS("text-transform", "uppercase");
    await expect(edit).toHaveCSS("font-family", /Geist Mono/);
    await expect(edit).toHaveCSS("text-decoration-style", "dotted");
    const icon = await edit.evaluate((el) => getComputedStyle(el, "::before").maskImage);
    expect(icon).toContain("/docs-icons/");
    await edit.scrollIntoViewIfNeeded();
    await page.screenshot({ path: info.outputPath(`edit-action${slug ? "-installation" : ""}-${info.project.name}.png`) });
  }
});

test("Copy .md returns to its original label after quick repeat clicks", async ({ page, context }) => {
  // Regression: clicking again while the link still reads "Copied .md" must not
  // capture that transient text as the label to restore, or the link would stay
  // stuck on "Copied .md" until a reload.
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await page.goto("/docs");
  const copy = page.locator('#nd-page a[title="Copy this page as Markdown"]');
  await expect(copy).toHaveText(/^copy \.md$/i);
  // First click copies and swaps the label to the transient "Copied .md".
  await copy.click();
  await expect(copy).toHaveText(/^copied \.md$/i);
  // A second click inside the ~1.8s restore window (link still shows "Copied
  // .md") must still restore the ORIGINAL label, not the transient one.
  await copy.click();
  await expect(copy).toHaveText(/^copied \.md$/i);
  await expect(copy).toHaveText(/^copy \.md$/i, { timeout: 4000 });
});

test("Copy .md passes modifier clicks through instead of copying", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await page.goto("/docs");
  const copy = page.locator('#nd-page a[title="Copy this page as Markdown"]');
  await expect(copy).toHaveText(/^copy \.md$/i);
  await page.evaluate(() => navigator.clipboard.writeText("sentinel-not-markdown"));
  // A meta/ctrl-click is the browser's "open in new tab" gesture. The handler
  // must ignore it: not preventDefault, not copy. Dispatch a guarded synthetic
  // click so we can read defaultPrevented without following the link.
  const prevented = await copy.evaluate((el) => {
    let seen = true;
    const guard = (e: Event) => { seen = e.defaultPrevented; e.preventDefault(); };
    el.addEventListener("click", guard, false);
    el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true, button: 0, metaKey: true }));
    el.removeEventListener("click", guard, false);
    return seen;
  });
  expect(prevented).toBe(false);
  // The clipboard was never touched and the label never flipped.
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe("sentinel-not-markdown");
  await expect(copy).toHaveText(/^copy \.md$/i);
});

test("docs use smaller headings and a constrained reading column", async ({ page }, info) => {
  for (const url of ["/docs", "/docs/installation", "/docs/cli"]) {
    await page.goto(url);
    const article = page.locator("#nd-page .fd-page-body");
    await expect(article).toBeVisible();
    expect((await article.boundingBox())!.width, url).toBeLessThanOrEqual(720);
    const headings = await page.locator("#nd-page :is(h1, h2, h3)").evaluateAll((elements) =>
      elements.map((el) => ({ level: el.tagName, size: parseFloat(getComputedStyle(el).fontSize) })),
    );
    for (const heading of headings) {
      expect(heading.size, `${url} ${heading.level}`).toBeLessThanOrEqual({ H1: 32, H2: 22, H3: 17 }[heading.level]!);
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), url).toBe(true);
  }
  await page.screenshot({ path: info.outputPath(`compact-docs-${info.project.name}.png`) });
});

test("docs prose uses a smaller size and lighter weight without flattening emphasis", async ({ page }, info) => {
  for (const url of ["/docs", "/docs/cli"]) {
    await page.goto(url);
    const body = page.locator("#nd-page .fd-docs-content");
    await expect(body).toHaveCSS("font-size", "15px");
    await expect(body).toHaveCSS("font-weight", "380");
    const proseSizes = await body.locator("p, li, td").evaluateAll((elements) =>
      [...new Set(elements.map((el) => getComputedStyle(el).fontSize))],
    );
    expect(proseSizes).toEqual(["15px"]);
    const proseWeights = await body.locator("p, li, td").evaluateAll((elements) =>
      [...new Set(elements.map((el) => getComputedStyle(el).fontWeight))],
    );
    expect(proseWeights).toEqual(["380"]);
    for (const link of await body.locator(":is(p, li, td) a:not([title])").all()) {
      await expect(link).toHaveCSS("font-weight", "450");
    }
    for (const strong of await body.locator("strong").all()) {
      await expect(strong).toHaveCSS("font-weight", "500");
    }
    await expect(body.locator("h1")).toHaveCSS("font-weight", "500");
    await expect(body.locator("pre").first()).toHaveCSS("font-weight", "400");
    await expect(body.locator("pre").first()).toHaveCSS("font-size", "12px");
    await page.screenshot({ path: info.outputPath(`lighter-${url === "/docs" ? "intro" : "cli"}-${info.project.name}.png`) });
  }
});

test("docs reading width stays bounded on ultrawide displays", async ({ page }, info) => {
  test.skip(info.project.name !== "desktop", "explicit wide-screen viewport coverage");
  for (const width of [1920, 2560]) {
    await page.setViewportSize({ width, height: 1080 });
    await page.goto("/docs/installation");
    const article = (await page.locator("#nd-page .fd-page-body").boundingBox())!;
    expect(article.width).toBeLessThanOrEqual(720);
    expect(article.width).toBeGreaterThanOrEqual(680);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    const sidebar = (await page.locator("#nd-sidebar").boundingBox())!;
    const toc = (await page.locator("#nd-toc").boundingBox())!;
    expect(article.x).toBeGreaterThanOrEqual(sidebar.x + sidebar.width);
    expect(article.x + article.width).toBeLessThanOrEqual(toc.x);
    await page.screenshot({ path: info.outputPath(`compact-docs-${width}.png`) });
  }
});

test("inline code and table headers retain dark surfaces and readable contrast", async ({ page }, info) => {
  await page.goto("/docs/cli");
  const samples = page.locator("#nd-page code:not(pre code), #nd-page th");
  expect(await samples.count()).toBeGreaterThan(10);
  const colors = await samples.evaluateAll((elements) => elements.map((element) => {
    const style = getComputedStyle(element);
    const luminance = (rgb: string) => {
      const channels = rgb.match(/[\d.]+/g)!.slice(0, 3).map(Number).map((value) => {
        const channel = value / 255;
        return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
      });
      return channels[0] * 0.2126 + channels[1] * 0.7152 + channels[2] * 0.0722;
    };
    const text = luminance(style.color);
    const background = luminance(style.backgroundColor);
    return {
      label: element.textContent?.slice(0, 60),
      background: style.backgroundColor,
      contrast: (Math.max(text, background) + 0.05) / (Math.min(text, background) + 0.05),
    };
  }));
  for (const sample of colors) {
    expect(sample.background, sample.label).toBe("rgb(16, 16, 16)");
    expect(sample.contrast, sample.label).toBeGreaterThanOrEqual(4.5);
  }
  await page.locator("#nd-page table").first().scrollIntoViewIfNeeded();
  await page.screenshot({ path: info.outputPath(`docs-contrast-${info.project.name}.png`) });
});

test("docs keep compact commands, illustrated navigation, and a quiet footer", async ({ page }, info) => {
  await page.goto("/docs");
  await expect(page.getByRole("heading", { name: "Built in the open" })).toHaveCount(0);
  await expect(page.locator("#nd-page")).not.toContainText("Documentation powered by");
  const command = page.locator("#nd-page figure.shiki").first();
  expect((await command.boundingBox())!.height).toBeLessThanOrEqual(56);
  const copy = command.getByRole("button", { name: "Copy Text", exact: true });
  const commandBox = (await command.boundingBox())!;
  const copyBox = (await copy.boundingBox())!;
  expect(copyBox.y).toBeGreaterThanOrEqual(commandBox.y);
  expect(copyBox.y + copyBox.height).toBeLessThanOrEqual(commandBox.y + commandBox.height);

  if (info.project.name === "mobile") await page.getByRole("button", { name: /Open Sidebar/i }).click();
  const sidebar = page.locator("#nd-sidebar, #nd-sidebar-mobile").filter({ visible: true });
  await expect(sidebar).not.toContainText(/Measurements|Design decisions|Running benchmarks|Allocation evidence/);
  const header = sidebar.locator(":scope > div").first();
  expect((await header.boundingBox())!.height).toBeLessThanOrEqual(info.project.name === "desktop" ? 108 : 120);
  const brand = info.project.name === "mobile"
    ? page.locator("#nd-subnav").getByRole("link", { name: /Riftri/ })
    : header.getByRole("link", { name: /Riftri/ });
  await expect(brand).toHaveCSS("font-size", "18px");
  await page.screenshot({ path: info.outputPath(`docs-navigation-${info.project.name}.png`) });
  for (const doc of groups.flatMap((group) => group.pages)) {
    const url = `/docs${doc.slug ? `/${doc.slug}` : ""}`;
    const link = page.locator(`a[data-active][href="${url}"]`).filter({ visible: true });
    await expect(link.locator('svg[aria-hidden="true"]')).toHaveCount(1);
  }
  await page.locator('a[data-active][href="/docs/installation"]').filter({ visible: true }).click();
  const navigation = page.getByRole("navigation", { name: "Page navigation" });
  for (const link of await navigation.getByRole("link").all()) {
    await expect(link).toHaveCSS("border-top-width", "0px");
    await expect(link).toHaveCSS("background-color", "rgba(0, 0, 0, 0)");
    await expect(link.locator(".fd-page-nav-title")).toHaveCSS("text-transform", "uppercase");
    expect((await link.boundingBox())!.height).toBeLessThanOrEqual(64);
  }
  await navigation.getByRole("link", { name: /Introduction/ }).click();
  await expect(page).toHaveURL(/\/docs$/);
  await page.getByRole("navigation", { name: "Page navigation" }).getByRole("link").click();
  await expect(page).toHaveURL(/\/docs\/installation$/);
});

test("docs render the real adapter with matching styles and no errors", async ({ page }, info) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const response = await page.goto("/docs");
  expect(response?.status()).toBe(200);
  await expect(page.locator('meta[name="generator"]')).toHaveAttribute("content", "@farming-labs/farmjs");
  await expect(page.getByRole("heading", { name: "Riftri documentation", exact: true })).toBeVisible();
  await expect(page.locator("#nd-page figure").first()).toHaveCSS("border-left-color", "rgb(240, 106, 58)");
  await expect(page.locator("#nd-page figure").first()).toHaveCSS("border-top-width", "0px");
  await expect(page.getByRole("button", { name: "Copy Text", exact: true }).first()).toHaveCSS("opacity", "1");
  await page.evaluate(() => document.fonts.ready);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: info.outputPath(`docs-${info.project.name}.png`) });
  expect(errors).toEqual([]);
});

test("docs navigation works on desktop and mobile, including history", async ({ page }, info) => {
  await page.goto("/docs");
  if (info.project.name === "mobile") await page.getByRole("button", { name: /Open Sidebar/i }).click();
  await page.locator('a[data-active][href="/docs/installation"]').filter({ visible: true }).click();
  await expect(page).toHaveURL(/\/docs\/installation$/);
  await expect(page.locator("#nd-page h1")).toContainText(/install/i);
  await expect(page.getByRole("link", { name: /^edit on github$/i })).toHaveAttribute("href", "https://github.com/assistant-ui/riftri/blob/main/website/content/guides/installation.md");
  await page.goBack();
  await expect(page.getByRole("heading", { name: "Riftri documentation", exact: true })).toBeVisible();
});

test("docs search finds content and opens a result", async ({ page }) => {
  await page.goto("/docs");
  const input = page.getByRole("combobox");
  // Exercise the visible control first. Sending a global key immediately after
  // SSR navigation can race the search provider's hydration on CI.
  await page.getByRole("button", { name: /search/i }).filter({ visible: true }).first().click();
  await expect(input).toBeVisible();
  for (const shortcut of ["Control+k", "Meta+k"]) {
    await input.press("Escape");
    await expect(input).not.toBeVisible();
    await page.keyboard.press(shortcut);
    await expect(input).toBeVisible();
  }
  await input.fill("OverlayFS");
  const results = page.getByRole("listbox", { name: "Search results" });
  await expect(results).toContainText("OverlayFS");
  await input.press("ArrowDown");
  await input.press("Enter");
  await expect(input).not.toBeVisible();
  await expect(page).not.toHaveURL(/\/docs$/);
  await expect(page.locator("#nd-page h1")).toBeVisible();
});

test("homepage docs link opens the adapter and reference pages fit the viewport", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("link", { name: "Docs", exact: true }).click();
  await expect(page).toHaveURL(/\/docs$/);
  await expect(page.getByRole("heading", { name: "Riftri documentation", exact: true })).toBeVisible();
  for (const url of ["/docs/cli", "/docs/windows-refs", "/docs/architecture"]) {
    await page.goto(url);
    await expect(page.locator("#nd-page h1")).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), url).toBe(true);
  }
});

test("docs command copying still works", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await page.goto("/docs/installation");
  await page.getByRole("button", { name: "Copy Text", exact: true }).first().click();
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toContain("curl -fsSL https://riftri.dev/install.sh | bash");
});

test("every docs page and Markdown mirror is served by the production build", async ({ request }, info) => {
  test.skip(info.project.name !== "desktop", "same server for every viewport");
  for (const icon of ["file-text", "square-pen"]) {
    const response = await request.get(`/docs-icons/${icon}.svg`);
    expect(response.status()).toBe(200);
    expect(response.headers()["content-type"]).toContain("image/svg+xml");
    expect(await response.text()).toContain("<svg");
  }
  for (const page of groups.flatMap((group) => group.pages)) {
    const url = `/docs${page.slug ? `/${page.slug}` : ""}`;
    const response = await request.get(url);
    expect(response.status(), url).toBe(200);
    const body = await response.text();
    expect(body, url).toContain("farm-docs-root");
    expect(body, url).not.toContain("Page module missing");
    // The page's own `.md` serves the full reference (there is no /agent.md).
    const markdown = await request.get(`${url}.md`);
    expect(markdown.status(), `${url}.md`).toBe(200);
    expect(markdown.headers()["content-type"]).toContain("text/plain");
    expect(markdown.headers()["content-disposition"]).toBe("inline");
    expect(markdown.headers()["x-robots-tag"]).toBe("noindex");
    expect(await markdown.text()).toContain(`Generated from ${page.source}`);
    const head = await request.head(`${url}.md`);
    expect(head.status()).toBe(200);
    expect(await head.body()).toHaveLength(0);
  }
  expect((await request.get("/docs/does-not-exist")).status()).toBe(404);
  expect((await request.get("/docs/does-not-exist.md")).status()).toBe(404);
  const search = await request.get("/api/docs?query=OverlayFS");
  expect(search.status()).toBe(200);
  expect((await search.json()).length).toBeGreaterThan(0);
  expect(search.headers()["cache-control"]).toBe("no-store");
  const agentOnlySearch = await request.get("/api/docs?query=FSCTL_DUPLICATE_EXTENTS_TO_FILE");
  expect(await agentOnlySearch.json()).toEqual([]);
});

test("public guides stay concise while full installation and CLI details remain available", async ({ page, request }, info) => {
  for (const doc of groups.flatMap((group) => group.pages)) {
    await page.goto(`/docs${doc.slug ? `/${doc.slug}` : ""}`);
    const body = page.locator("#nd-page .fd-docs-content");
    await expect(body.locator("h1")).toBeVisible();
    const words = (await body.innerText()).trim().split(/\s+/).length;
    expect(words, doc.title).toBeLessThan(500);
    await expect(page.getByRole("link", { name: /^copy \.md$/i })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), doc.title).toBe(true);
  }
  await page.goto("/docs/installation");
  await expect(page.locator("#nd-page")).not.toContainText("RIFTRI_INSTALL_DIR");
  await expect(page.locator("#nd-page")).toContainText("riftri setup");
  await page.screenshot({ path: info.outputPath(`simple-installation-${info.project.name}.png`), fullPage: true });
  const installation = await request.get("/docs/installation.md");
  expect(await installation.text()).toContain("RIFTRI_INSTALL_DIR");
  const cli = await request.get("/docs/cli.md");
  expect(await cli.text()).toContain("## Exit codes");
});

import { expect, test } from "@playwright/test";

test("unknown paths show the branded 404 and offer a working route home", async ({ page }) => {
  const response = await page.goto("/missing-preview-regression");
  expect(response?.status()).toBe(404);
  await expect(page.getByRole("heading", { name: "404", exact: true })).toBeVisible();
  await expect(page.locator('meta[name="robots"]')).toHaveAttribute("content", /noindex/);
  await page.getByRole("link", { name: "Back to the homepage" }).click();
  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByRole("heading", { name: "Git worktrees. Shared storage.", exact: true })).toBeVisible();
});

test("the emitted 404 route resolves through its build-output override", async ({ page }) => {
  const response = await page.goto("/404");
  expect(response?.status()).toBe(200);
  await expect(page.getByRole("heading", { name: "404", exact: true })).toBeVisible();
});

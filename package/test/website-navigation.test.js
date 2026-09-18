const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");
const source = fs.readFileSync(path.join(root, "website/src/components/section-link.tsx"), "utf8");
// Exercise the DOM helper without loading React or needing website dependencies.
const helper = source.slice(source.indexOf("function scrollToSection"), source.indexOf("export function SectionLink"))
  .replace("hash: string", "hash");

function fixture({ reduced = false, missing = false } = {}) {
  const calls = [];
  const target = {
    focus: (options) => calls.push(["focus", options]),
    scrollIntoView: (options) => calls.push(["scroll", options]),
  };
  const context = vm.createContext({
    document: { getElementById: () => missing ? null : target },
    window: { matchMedia: () => ({ matches: reduced }) },
  });
  vm.runInContext(helper, context);
  return { calls, navigate: (focus = false) => context.scrollToSection("#start", focus) };
}

test("explicit section navigation transfers focus without a second scroll", () => {
  const { calls, navigate } = fixture();
  assert.equal(navigate(true), true);
  assert.equal(calls[0][0], "focus");
  assert.equal(calls[0][1].preventScroll, true);
  assert.equal(calls[1][1].behavior, "smooth");
});

test("initial hash restoration does not steal focus and respects reduced motion", () => {
  const { calls, navigate } = fixture({ reduced: true });
  navigate();
  assert.equal(calls.length, 1);
  assert.equal(calls[0][0], "scroll");
  assert.equal(calls[0][1].behavior, "auto");
  assert.equal(fixture({ missing: true }).navigate(true), false);
});

test("quick start is a named programmatic focus target, outside normal tab order", () => {
  const page = fs.readFileSync(path.join(root, "website/src/app/page.tsx"), "utf8");
  assert.match(page, /id="start"[^>]*tabIndex=\{-1\}[^>]*aria-labelledby="start-title"/);
  assert.match(page, /<h2 id="start-title">/);
  assert.match(source, /scrollToSection\(href, true\)/);
});

test("header navigation has named, keyboard-focusable section destinations", () => {
  const page = fs.readFileSync(path.join(root, "website/src/app/page.tsx"), "utf8");
  assert.match(page, /<nav aria-label="Main navigation">/);
  for (const id of ["top", "overview", "savings", "faq"]) {
    assert.match(page, new RegExp(`id="${id}"[^>]*tabIndex=\\{-1\\}[^>]*aria-labelledby="${id}-title"`));
    assert.match(page, new RegExp(`<h[12] id="${id}-title">`));
  }
  assert.match(page, /className="skip-link" href="#main-content"/);
  assert.match(page, /<main id="main-content" tabIndex=\{-1\}>/);
});

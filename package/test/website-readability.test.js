const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const css = fs.readFileSync(path.resolve(__dirname, "../../website/src/app/globals.css"), "utf8");
const token = (name) => css.match(new RegExp(`--${name}: (#\\w{6});`))[1];
const luminance = (hex) => hex.slice(1).match(/../g)
  .map((channel) => parseInt(channel, 16) / 255)
  .map((channel) => channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4)
  .reduce((sum, channel, index) => sum + channel * [0.2126, 0.7152, 0.0722][index], 0);

test("secondary text meets normal-text contrast on every page surface", () => {
  for (const ink of ["ink-muted", "ink-faint"]) {
    for (const background of ["canvas", "canvas-deep", "surface"]) {
      const ratio = (luminance(token(ink)) + 0.05) / (luminance(token(background)) + 0.05);
      assert.ok(ratio >= 4.5, `${ink} on ${background}: ${ratio.toFixed(2)}:1`);
    }
  }
});

test("small fixed-size text does not shrink below 0.7rem", () => {
  for (const match of css.matchAll(/font-size:\s*([\d.]+)rem/g)) {
    assert.ok(Number(match[1]) >= 0.7, match[0]);
  }
});

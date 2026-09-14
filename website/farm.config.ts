import { defineConfig } from "@farm.js/core";

export default defineConfig({
  theme: {
    default: "dark",
  },
  images: {
    provider: "none",
  },
  deploy: {
    target: "vercel",
  },
});

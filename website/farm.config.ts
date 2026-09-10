import { defineConfig } from "@farm.js/core";

export default defineConfig({
  theme: {
    default: "dark",
  },
  docs: {
    enabled: true,
  },
  deploy: {
    target: "vercel",
  },
});

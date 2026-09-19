import { defineConfig } from "@farm.js/core";
import { withDocs } from "@farming-labs/farmjs/config";

export default withDocs(defineConfig({
  theme: {
    default: "dark",
  },
  images: {
    provider: "none",
  },
  deploy: {
    target: "vercel",
  },
}));

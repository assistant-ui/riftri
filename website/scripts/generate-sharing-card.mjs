import { fileURLToPath } from "node:url";
import sharp from "sharp";

await sharp(fileURLToPath(new URL("../assets/og.svg", import.meta.url)))
  .png()
  .toFile(fileURLToPath(new URL("../public/og.png", import.meta.url)));

#!/usr/bin/env tsx
// Generate brand SVG assets from vector paths + color tokens.
// Run: npx tsx docs/scripts/generate-brand.ts
// Output: brand/icon.svg, brand/favicon.svg, docs/public/logo.svg

import { writeFileSync } from "fs";
import { resolve, dirname } from "path";
import { fileURLToPath } from "url";
import { A_PATH, HIRAGANA_A_PATH, MAGENTA, PURPLE } from "./brand-paths.js";

const __dirname = dirname(fileURLToPath(import.meta.url));

// Full logo — two overlapping speech bubbles with A + あ.
// Used in nav bar, hero image, OG share image.
function buildLogo(): string {
  const aScale = 0.32;
  const aX = 33;
  const aY = 46;

  const hScale = 0.28;
  const hX = 72;
  const hY = 78;

  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128" fill="none">
  <!-- Back bubble -->
  <rect x="8" y="8" width="72" height="56" rx="12" fill="${MAGENTA}" opacity="0.85"/>
  <polygon points="24,64 36,64 28,78" fill="${MAGENTA}" opacity="0.85"/>

  <!-- "A" glyph -->
  <g transform="translate(${aX}, ${aY}) scale(${aScale})">
    <path d="${A_PATH}" fill="white"/>
  </g>

  <!-- Front bubble -->
  <rect x="48" y="40" width="72" height="56" rx="12" fill="${PURPLE}" opacity="0.95"/>
  <polygon points="96,96 108,96 100,110" fill="${PURPLE}" opacity="0.95"/>

  <!-- "あ" glyph -->
  <g transform="translate(${hX}, ${hY}) scale(${hScale})">
    <path d="${HIRAGANA_A_PATH}" fill="white"/>
  </g>
</svg>`;
}

// Favicon mark — magenta disc with あ centered.
// Clean and readable at 16-96px.
function buildFavicon(): string {
  // あ glyph bounding box at size 100: x≈11..87 (w=76), y≈-78..6 (h=84)
  // Center of glyph: x=49, y=-36
  // Target: centered in 128x128 viewbox (center=64,64)
  // At scale 0.45: glyph is ~34x38px
  // translateX = 64 - (49 * 0.45) = 64 - 22 = 42
  // translateY = 64 - (-36 * 0.45) = 64 + 16 = 80
  // At scale 1: glyph is 76x84px, center at (49, -36)
  // Center in 128x128: translateX = 64 - 49 = 15, translateY = 64 + 36 = 100
  const hScale = 1;
  const hX = 15;
  const hY = 100;

  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128" fill="none">
  <circle cx="64" cy="64" r="60" fill="${MAGENTA}"/>
  <g transform="translate(${hX}, ${hY}) scale(${hScale})">
    <path d="${HIRAGANA_A_PATH}" fill="white"/>
  </g>
</svg>`;
}

// OG share image (1280x640) — logo + tagline on gradient background
function buildOgImageSvg(): string {
  const w = 1280;
  const h = 640;

  // Logo scaled and centered
  const logoScale = 0.8;
  const logoSize = 128 * logoScale;
  const logoX = (w - logoSize) / 2;
  const logoY = h * 0.2;

  // Title
  const titleY = logoY + logoSize + 60;

  // Tagline
  const taglineY = titleY + 40;

  return `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}">
  <rect width="${w}" height="${h}" fill="#1a1a2e" />
  <defs>
    <radialGradient id="og-glow" cx="50%" cy="35%" r="40%">
      <stop offset="0%" stop-color="${MAGENTA}" stop-opacity="0.15" />
      <stop offset="100%" stop-color="${MAGENTA}" stop-opacity="0" />
    </radialGradient>
  </defs>
  <ellipse cx="${w / 2}" cy="${h * 0.35}" rx="${w * 0.35}" ry="${
    h * 0.4
  }" fill="url(#og-glow)" />

  <!-- Logo -->
  <g transform="translate(${logoX}, ${logoY}) scale(${logoScale})">
    <rect x="8" y="8" width="72" height="56" rx="12" fill="${MAGENTA}" opacity="0.85"/>
    <polygon points="24,64 36,64 28,78" fill="${MAGENTA}" opacity="0.85"/>
    <g transform="translate(33, 46) scale(0.32)">
      <path d="${A_PATH}" fill="white"/>
    </g>
    <rect x="48" y="40" width="72" height="56" rx="12" fill="${PURPLE}" opacity="0.95"/>
    <polygon points="96,96 108,96 100,110" fill="${PURPLE}" opacity="0.95"/>
    <g transform="translate(72, 78) scale(0.28)">
      <path d="${HIRAGANA_A_PATH}" fill="white"/>
    </g>
  </g>

  <!-- Title -->
  <text x="${w / 2}" y="${titleY}"
    text-anchor="middle"
    font-family="system-ui, -apple-system, sans-serif"
    font-size="42" font-weight="700"
    fill="white"
  >multilinguarr</text>

  <!-- Tagline -->
  <text x="${w / 2}" y="${taglineY}"
    text-anchor="middle"
    font-family="system-ui, -apple-system, sans-serif"
    font-size="20" font-weight="300"
    letter-spacing="2" fill="white" opacity="0.6"
  >MULTI-LANGUAGE AUDIO FOR THE *ARR STACK</text>
</svg>`;
}

async function generateOgImage(publicDir: string): Promise<void> {
  const { default: sharp } = await import("sharp");
  const svg = buildOgImageSvg();
  await sharp(Buffer.from(svg))
    .png()
    .toFile(resolve(publicDir, "og-share.png"));
  console.log("wrote docs/public/og-share.png");
}

// --- Main ---

const logo = buildLogo();
const favicon = buildFavicon();

const brandDir = resolve(__dirname, "..", "..", "brand");
const publicDir = resolve(__dirname, "..", "public");

// Full logo
writeFileSync(resolve(brandDir, "icon.svg"), logo);
console.log("wrote brand/icon.svg");

// Favicon mark (used by realfavicon)
writeFileSync(resolve(brandDir, "favicon.svg"), favicon);
console.log("wrote brand/favicon.svg");

// Nav logo for VitePress
writeFileSync(resolve(publicDir, "logo.svg"), logo);
console.log("wrote docs/public/logo.svg");

// OG share image
await generateOgImage(publicDir);
console.log("done");

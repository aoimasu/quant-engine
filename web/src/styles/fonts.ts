/*
 * Self-hosted webfonts (bundled by Vite — NO runtime CDN).
 *
 * The Claude Design source loads these three families from the Google Fonts
 * CDN via @import (tokens/fonts.css). The prod server serves static assets
 * with a strict origin, so we instead bundle the .woff2 binaries through the
 * @fontsource packages. The family names ('Space Grotesk' / 'Hanken Grotesk' /
 * 'JetBrains Mono') are unchanged, so every ported --font-* token resolves.
 *
 * Weights match the design's @import: Space Grotesk 400-700,
 * Hanken Grotesk 400-800, JetBrains Mono 400-700.
 */

// Space Grotesk — display / headings
import '@fontsource/space-grotesk/400.css';
import '@fontsource/space-grotesk/500.css';
import '@fontsource/space-grotesk/600.css';
import '@fontsource/space-grotesk/700.css';

// Hanken Grotesk — UI / body
import '@fontsource/hanken-grotesk/400.css';
import '@fontsource/hanken-grotesk/500.css';
import '@fontsource/hanken-grotesk/600.css';
import '@fontsource/hanken-grotesk/700.css';
import '@fontsource/hanken-grotesk/800.css';

// JetBrains Mono — numeric data / code / tickers
import '@fontsource/jetbrains-mono/400.css';
import '@fontsource/jetbrains-mono/500.css';
import '@fontsource/jetbrains-mono/600.css';
import '@fontsource/jetbrains-mono/700.css';

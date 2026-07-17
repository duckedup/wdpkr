// @ts-check
import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";

// wdpkr docs — Astro + Starlight, deployed to GitHub Pages at a custom domain.
// Custom domain serves from the root, so `base` stays "/".
export default defineConfig({
  site: "https://wdpkr.duckedup.org",
  integrations: [
    starlight({
      title: "wdpkr",
      description:
        "Semantic code search for AI agents. Taps through your codebase to find exactly where things live.",
      logo: {
        // Nord woodpecker — ink body on light, snow body on dark.
        light: "./src/assets/woodpecker-ink.svg",
        dark: "./src/assets/woodpecker-snow.svg",
        alt: "wdpkr",
      },
      // Code blocks always wear Nord — keeps the brand consistent in both themes.
      expressiveCode: {
        themes: ["nord"],
        styleOverrides: {
          borderRadius: "0.5rem",
          borderColor: "var(--sl-color-gray-5)",
        },
      },
      social: [
        {
          icon: "github",
          label: "GitHub",
          href: "https://github.com/duckedup/wdpkr",
        },
      ],
      customCss: [
        "@fontsource-variable/fraunces",
        "@fontsource-variable/hanken-grotesk",
        "@fontsource/jetbrains-mono/400.css",
        "@fontsource/jetbrains-mono/500.css",
        "./src/styles/zen.css",
      ],
      sidebar: [
        {
          label: "Start here",
          items: [
            { label: "Introduction", link: "/" },
            { label: "Getting started", link: "/getting-started/" },
          ],
        },
        {
          label: "Guides",
          items: [
            { label: "How it works", link: "/guides/how-it-works/" },
            { label: "Configuration", link: "/guides/configuration/" },
            { label: "Providers", link: "/guides/providers/" },
            { label: "Taps", link: "/guides/taps/" },
            { label: "Decision recall", link: "/guides/decisions/" },
            { label: "Storage", link: "/guides/storage/" },
          ],
        },
        {
          label: "Reference",
          items: [
            { label: "CLI commands", link: "/reference/commands/" },
            { label: "Architecture", link: "/reference/architecture/" },
            { label: "Evaluation", link: "/reference/evaluation/" },
          ],
        },
      ],
    }),
  ],
});

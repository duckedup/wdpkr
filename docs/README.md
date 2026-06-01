# wdpkr docs

The documentation site for [wdpkr](https://github.com/duckedup/wdpkr), built
with [Astro](https://astro.build) + [Starlight](https://starlight.astro.build)
and deployed to GitHub Pages at **https://wdpkr.duckedup.org**.

## Local development

From the repo root:

```bash
just docs          # dev server with live reload (http://localhost:4321)
just docs-build    # production build → docs/dist/
just docs-preview  # preview the production build
```

Or directly with [Bun](https://bun.sh):

```bash
cd docs
bun install
bun run dev
```

## Structure

```
docs/
├── astro.config.mjs          # site config, sidebar, Nord code theme
├── src/
│   ├── content/docs/         # the docs pages (Markdown / MDX)
│   ├── styles/zen.css        # the "zen woodpecker" Nord theme
│   └── assets/woodpecker.svg # the logo mark
└── public/
    ├── CNAME                 # custom domain
    └── favicon.svg
```

## Theme

The "zen woodpecker" theme maps the [Nord](https://www.nordtheme.com) palette
onto Starlight's design tokens (`src/styles/zen.css`): Polar Night for dark
mode, Snow Storm for light, Frost as the single accent, and Aurora red reserved
for the woodpecker's crest. Code blocks use the built-in Nord syntax theme in
both modes.

## Deployment

Pushing to `main` with changes under `docs/**` triggers
`.github/workflows/docs.yml`, which builds the site and publishes it to GitHub
Pages.

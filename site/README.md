# omastorm.com

Static HTML, with no build step. `index.html` summarizes the user guide in
[../README.md](../README.md); keep installation, onboarding, search, playback,
and update instructions in sync with it. The header links to releases without
pinning a version that can become stale.

## Assets

`media/{window,window-light,hero,search-city,treatments}.png` are committed
copies of the corresponding README screenshots in `docs/media/readme/`.
Refresh both sets when the interface changes. Dark Tokyo Night and light
Flexoki Light window stills lead the page side by side, stacking on mobile.
The window/popover hero, search, and treatment stills illustrate the guide.
The hero is also the Open Graph sharing image.
`media/omastorm-europe.png` mirrors the Europe demo poster from
`docs/media/readme/omastorm-europe.png` and shows OPERA coverage over Warsaw.

The site uses still images only. Every required image is committed; no video
capture or generated media is needed for deployment. Cloudflare Pages caps
each file at 25 MiB.

The favicon and touch icons come from `branding/mark`; the header uses the
same monochrome mark as the app. The page follows the visitor’s color scheme. The page uses JetBrains Mono throughout. Treatment names are HTML labels
in that same font; CSS clips the older montage’s baked-in cursive labels. Decorative icons are SVG
so they do not depend on a Nerd Font or symbol-font fallback. The dedicated
sponsorship section links directly to GitHub Sponsors.
`_headers` sets cache lifetimes. `robots.txt`, `sitemap.xml`, the canonical URL,
and SoftwareApplication JSON-LD describe the public homepage.

## Local review

From the repository root:

```sh
python -m http.server 8765 --directory site --bind 127.0.0.1
```

Open http://127.0.0.1:8765. Check desktop and mobile widths, dark and light
mode, the install/update copy buttons, and linked assets.

## Deploy

Hosted on Cloudflare Pages, project `omastorm`, by direct upload. The zone has
proxied CNAME records for the root and `www` pointing at `omastorm.pages.dev`.
Deployment requires Wrangler authentication with Pages write access.

After review and approval, from the repository root:

```sh
npx wrangler pages deploy site --project-name omastorm --branch main
```

After deployment, check `/robots.txt` and `/sitemap.xml` return their actual
files, then submit the sitemap in Google Search Console. Account settings
and outreach follow-ups are in [docs/discovery.md](../docs/discovery.md).

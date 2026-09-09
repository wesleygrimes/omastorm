# omastorm.com

Static, no build step: `index.html`, the live take as `take.mp4` with
`take-poster.png` as its poster and the Open Graph image, and `_headers` for
cache lifetimes, plus the mark as favicon and touch icon: `mark.svg` is the
16-grid app-colored SVG from `branding/mark` with its cells merged into one
path per color, and `mark-32.png` and `mark-256.png` are copies of the
branding exports. The header draws the same 16 px mark inline in the page
foreground, its cells from `ui/RadarMark.qml`. The two media files are generated and ignored, like the
README media, so the plugin clone stays small; cut them before a deploy. The
page follows the visitor's color scheme; the take was
filmed in the dark theme and stays dark in both.

Regenerate the media after a new take (`scripts/capture-demo.sh` writes
`docs/media/omastorm-demo.mp4`):

```sh
ffmpeg -y -i docs/media/omastorm-demo.mp4 -c:v libx264 -preset slow -crf 26 -pix_fmt yuv420p -movflags +faststart -an site/take.mp4
ffmpeg -y -ss 2.5 -i docs/media/omastorm-demo.mp4 -frames:v 1 site/take-poster.png
```

The site copy is encoded lighter than the release asset (about 6 MB against
under ~12 MB); the caption links the full-quality file on the `media-2026-09-09` release.

`popover.png` is the bar-popover still shown under the take. Keep it next to
`take.mp4` when deploying; it is small enough to commit if you want the site
folder self-contained without regenerating stills.
Cloudflare Pages caps a file at 25 MiB.

When the plugin version changes, update the tag in the header and its release
link, both in `index.html`.

Hosted on Cloudflare Pages, project `omastorm`, deployed by direct upload so
the page stays independent of the git repo. The zone omastorm.com has proxied
CNAME records for the root and `www` pointing at `omastorm.pages.dev`.
Redeploy after any change:

```sh
npx wrangler pages deploy site --project-name omastorm --branch main
```

Deployment requires Wrangler authentication with Pages write access. Manage
DNS through the Cloudflare connector or dashboard.

Review a change without deploying:

```sh
chromium --headless=new --disable-gpu --hide-scrollbars --window-size=1280,1500 --screenshot=review/site.png file://$PWD/site/index.html
```

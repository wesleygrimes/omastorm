# README media

The pictures the README shows are not in the repository. The plugin is a full
clone of this repository, so media travels as assets on the plugin's GitHub
Release (`v0.1.0`), and the README links to them by URL.

Regenerate from a working desktop OpenGL session:

```sh
bash scripts/capture-readme.sh   # window-live.png, popover.png (live KTLX)
bash scripts/capture-demo.sh     # omastorm-demo.mp4 and omastorm-preview.gif (live KJAX; SITE=KXXX for another)
```

Both write into this directory, which is ignored except for this file. They
use isolated daemons and change no desktop or system configuration. FFmpeg is
required; the demo also needs Ruby for its temporary harness. Frames are
grabbed as the scene settles, so the video runs a little faster than real
time and is not a latency measurement.

- `omastorm-demo.mp4`: one take, about 37 s, 1280×720, H.264, no audio:
  the home view, the loop, a pan and zoom to the coast, the three
  treatments, weak returns, the picker switching station, the keys sheet.
  The script records a live station; the `v0.1.0` assets the README links
  were taken from the archived KTLX 2013-05-20 fixture, as that release's
  notes say, and the README captions them so.
- `omastorm-preview.gif`: the home view and the zoom, cut from the video.
- `window-live.png`, `popover.png`: live KTLX with the actual scan time.

Upload only the generated media files with `gh release upload <tag> <files...>`
and point the README URLs at that tag. For immutable releases, upload media
while the release is still a draft; published assets cannot be replaced.

Radar: NOAA NEXRAD. Map: © OpenStreetMap contributors
([ODbL](https://opendatacommons.org/licenses/odbl/1-0/)); Natural Earth, public
domain.

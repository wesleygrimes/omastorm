# Radar fetch

How the engine gets Level II bytes for a live picture and for archived
pictures.

Related: [DESIGN.md](../DESIGN.md), [data/README.md](../data/README.md),
[NEXRAD on AWS](https://registry.opendata.aws/noaa-nexrad/).

The buckets are public. No AWS account. No request signing.

## Job

The user picks a site. Find the right objects, download them, decode the
lowest tilt, paint a texture.

Live is a folder of **chunks** for the scan in progress. Archive is **one
file** per finished volume.

A volume takes a few minutes: about 4.3 in severe weather (VCP 12; AVSET
can end one after ~3.2), 4.6–6 in typical rain (VCP 212, 215), ~7 in
clear air (VCP 35). That is the radar, not the fetch.

## Buckets

| Bucket | Host | Keys |
|---|---|---|
| Live chunks | `unidata-nexrad-level2-chunks.s3.amazonaws.com` | `KFCX/412/20260913-232324-001-S` |
| Archive | `unidata-nexrad-level2.s3.amazonaws.com` | `2026/09/13/KFCX/KFCX20260913_231906_V06` |

S3 list accepts prefix, delimiter, start-after, and page size. Dates are
not a list filter.

## Live slots

Chunk keys sit under `SITE/VOLUME/`. `VOLUME` is `1`–`999`, then `1`
again. Old folders remain until they expire. A folder listing has no
dates. The date is in the object name (`YYYYMMDD-HHMMSS`).

The newest name timestamp is the live volume. Folder number is not
recency. After `999` the next slot is `1`.

## Live picture

The archive bucket names the last finished volume. The live folder is
the next slot.

Clock is **UTC**. Example site `KFCX`.

**1. Last finished file**

```
GET https://unidata-nexrad-level2.s3.amazonaws.com
  ?list-type=2
  &prefix=2026/09/13/KFCX/
  &max-keys=1000
```

Use the last `*_V06` key. Ignore `*_MDM`.

If today’s prefix is empty, use yesterday’s date.

**2. Volume id (first 24 bytes)**

```
GET https://unidata-nexrad-level2.s3.amazonaws.com/2026/09/13/KFCX/KFCX20260913_231906_V06
Range: bytes=0-23
```

Archive II volume header: 9-byte format (`AR2V0006.`), 3-byte ASCII
volume number (`411`), then date, time, and ICAO. That number is the
chunk-folder slot.

**3. List the next slot**

`411` finished → list `412`. If that folder is empty or its names are
not newer than the archive file, list `413`, then `414`. After `999`,
list `1`.

Three slots because the archive lags a finished volume by seconds to a
couple of minutes and a volume takes at least about three minutes: the
slot after the archived one is the volume just finished or the one
underway, the next is the one underway, and the third is margin for a
skipped slot. If none of the three has names newer than the archive
file, the site is unavailable. Watch this bound in live checks; a longer
archive lag would need a fourth.

```
GET https://unidata-nexrad-level2-chunks.s3.amazonaws.com
  ?list-type=2
  &prefix=KFCX/412/
  &start-after=KFCX/412/20260913-231906
  &max-keys=1000
```

`start-after` is the finished scan’s clock, so older keys in a reused
slot stay out. Confirm the names are newer than the archive file.

If there is no usable archive file or header, the site is unavailable.

Newer than the archive is not the same as live. If the dish and the
archive both stopped hours ago, the newest slot still wins the join and
its picture is painted. The LIVE / STALE / UNAVAILABLE label comes from
the age of the newest radial on screen, not from the join: `ok` under
ten minutes, `stale` to thirty, `unavailable` past that. The join never
labels. In practice chunk keys expire within a few hours, so a site quiet
longer than that lists no names newer than its archive file and is
unavailable rather than stale.

**4. Download chunks**

```
GET https://unidata-nexrad-level2-chunks.s3.amazonaws.com/KFCX/412/20260913-232324-001-S
GET https://unidata-nexrad-level2-chunks.s3.amazonaws.com/KFCX/412/20260913-232324-002-I
…
```

The `S` chunk and the following `I` chunks are the lowest tilt. Decode
those into the first live image.

**5. Follow the volume**

Poll step 3 every few seconds. GET only new keys. When an `E` chunk
arrives, the volume is done: the next slot is the next number (or `1`
after `999`). Return to step 1 only if the slot goes stale or the site
has no data.

## Archived pictures

Same archive bucket. Path is the calendar day.

```
GET https://unidata-nexrad-level2.s3.amazonaws.com
  ?list-type=2
  &prefix=YYYY/MM/DD/SITE/
  &max-keys=1000
```

```
GET https://unidata-nexrad-level2.s3.amazonaws.com/YYYY/MM/DD/SITE/SITEYYYYMMDD_HHMMSS_V06
```

Download the whole file. Decode the lowest tilt. Label it archived.

A day can have many `*_V06` files. Playback lists that day and fetches
each file needed.

The file appears seconds to a couple of minutes after a volume ends.
That picture is one scan behind the dish (~2–10 minutes).

Example: `2013/05/20/KTLX/KTLX20130520_201643_V06.gz`.

## Live vs archive

| | Live | Archive |
|---|---|---|
| Bucket | `unidata-nexrad-level2-chunks` | `unidata-nexrad-level2` |
| Find the object | Archive header + 1, then list that folder | List `YYYY/MM/DD/SITE/`, take `*_V06` |
| Object | Chunks (`S` / `I` / `E`) | One volume file |
| When it exists | While the dish is turning | After the volume completes |
| Label | LIVE | ARCHIVED |

Live is now. Archive is a finished scan or a past day.

## HTTP

One client for archive list, range-get, chunk list, and chunk get.
Unsigned HTTPS. Timeouts on every call.

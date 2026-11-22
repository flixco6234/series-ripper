# Series Ripper

Rips TV series from **DVD, Blu-ray and 4K UHD** discs with
[MakeMKV](https://www.makemkv.com/) and names the episodes the Plex/Jellyfin way:

```
Serien/Kare Kano/Season 1/Kare Kano - S01E14.mkv
Serien/Kare Kano/Season 1/Kare Kano - S01E15 - Episode Title.mkv
Serien/Kare Kano/Season 0/Kare Kano - S00E01.mkv          (Special)
```

The season folders can also be `Season 01`, or left out (Settings → Folder layout).

It runs as a Docker container with a web UI, made for Unraid. It started as a
server version of a Windows "DVD Series Ripper" script.

- **Manual Rip:** scan a disc, check the proposed episode numbers, rip.
- **Automatic Mode:** insert a disc and walk away. It scans and rips on its own,
  ejects the disc, and continues the episode count for the next disc. A disc it
  is not sure about goes to the **Review Queue** instead of being guessed.
- **Episode detection**, each one switchable in Settings:
  - menus and extras (too short) become Specials;
  - **"Play All" titles** are unticked automatically (a title exactly as long as
    several episodes added up);
  - titles over 1 hour, or much longer or shorter than the disc's other
    episodes, are flagged;
  - episode numbers written in the disc's own title names are used.
- **MakeMKV key:** the free beta key is fetched automatically and refreshed daily.
- **Unraid-friendly:** temp files live on your share, not in `docker.img`, and
  files belong to `nobody:users`.

> **AI disclaimer:** most of this project (code, tests and this README) was
> written by an AI coding assistant, Anthropic's Claude in Claude Code, at the
> direction of the repository owner. It has automated tests and was tried in
> Docker; see [What has been tested](#what-has-been-tested-and-what-hasnt) for
> what that does and doesn't cover. Treat it like any code you didn't write
> yourself: read it before relying on it, and use it at your own risk. It comes
> with no warranty. It only writes inside the folders you map to the container,
> never overwrites an existing episode (a duplicate gets ` (1)` added to its
> name), and only deletes its own temp files.

## What you need

- Unraid (or any Docker host), x86-64.
- An optical drive. USB is fine.
- For **4K UHD**, the drive must be **"UHD-friendly" (LibreDrive)**. See the next section
  before you buy one.

### Choosing a 4K UHD USB drive

From the MakeMKV forum's
[UHD drive guide](https://forum.makemkv.com/forum/viewtopic.php?f=16&t=19634),
as of November 2026:

- Most 4K UHD drives refuse to give MakeMKV the raw data. Only drives that
  MakeMKV can switch into **LibreDrive** mode work. The chipset (platform
  "MT1959") matters most.
- **External drives that work out of the box** (no firmware flashing):
  Pioneer **BDR-XD08UMB-S**, **BDR-XD07UHD**, **BDR-XD06JUHD** (slim USB drives).
- **Other drives need MakeMKV's patched firmware**, for example LG WH16NS60
  (firmware 1.02MK), LG BU40N (1.03MK), ASUS BW-16D1HT (3.10MK). You can put an
  internal drive in a USB enclosure. Enclosures the guide lists as working:
  OWC Mercury Pro 5.25, ICY BOX IB-550StU3S, Silverstone TS14 (slim drives).
  The guide lists the archgon MD-8107S-U3-UHD and Buffalo BR3D-12U3 as having
  **no** LibreDrive support.
- **Flash on your Windows PC, not in this container.** The guide says Linux
  flashing only works with MakeMKV 1.17.7.
- Check the guide again right before you buy; the list changes.

Once the drive is connected, the MakeMKV log shows whether LibreDrive is active.

### Drive access

MakeMKV on Linux finds a drive only through its SCSI generic device
(`/dev/sgN`), so pass that to the container, plus the `/dev/srN` device for
ejecting. `lsscsi -g` on the host shows both at the end of the drive's line.
The drive must be connected when the container starts. A USB drive can get a
different number after being re-plugged.

`unraid/series-ripper.xml` (Unraid template) and `docker-compose.yml` are
examples of a working setup.

## First run

1. Open the web UI, go to **Settings**, and set the show name, season and the
   episode number the next disc starts at. Save.
2. **Manual Rip → Scan Disc.** A DVD takes a minute or two; Blu-ray and UHD
   discs can take several minutes. Check the table:
   - Tick/untick titles; **Episode #** renumbers itself.
   - **Special?** puts a title in Season 0.
   - The **Notes** column says why something was unticked or flagged.
3. **Rip Selected.** Watch progress on the Dashboard. After the disc, "Next
   episode #" moves on by itself.
4. When a couple of discs went the way you wanted, try **Automatic Mode**
   (Settings).

### Automatic Mode and the Review Queue

The container checks the drive every few seconds. When a disc goes in, it
scans and decides:

- **Rips on its own** when nothing is flagged on the titles it would rip.
  Play All titles, menus and extras are dropped automatically.
- **Parks the disc in the Review Queue** (nothing ripped) if a title it
  would rip is over 1 hour, much longer or shorter than the other episodes,
  or if the disc's own names contradict the numbering. Leave the disc in
  (or put it back), fix the table in the Review Queue, click **Rip These**.
- **Skips a disc it already ripped completely**, same label and same titles.
  This covers a container restart with a disc in the tray. Use Manual Rip
  to rip it again on purpose.

## 4K UHD and Blu-ray notes

- **Size:** a UHD episode is often 10–20 GB and a UHD movie 50–90 GB. The
  rip is first written to `<TV library>/.series-ripper-tmp` and then renamed
  into place. That's on the same share, so it's instant. Before each title the
  app checks there is enough free space, and skips the title with a log entry
  if not. The temp folder contains `.plexignore`/`.ignore` so Plex and
  Jellyfin don't pick up half-finished files.
- **Unraid:** a file can't be split across array disks, and the free-space
  check sees the whole array. Set the share's *Minimum free space* to at least
  your largest rip (e.g. 100 GB for UHD), or a title can fail when its disk
  fills up.
- **Sleep bug:** the forum guide says all UHD-friendly drives can stop
  reading a disc that sat idle for about 2 minutes. Automatic Mode rips right
  after the scan, so it's not affected. If a manual rip fails after you
  spent a while on the review, eject and re-insert the disc. The log says so
  too.
- **Java** is included, which MakeMKV uses for some Blu-ray menus.
- MakeMKV's data (key, settings, downloaded LibreDrive data) is kept in
  `/config/makemkv`.

## Settings reference

| Where | Setting | Default |
|---|---|---|
| Template / env | `MAKEMKV_KEY` | `BETA` (auto-fetch free key); a `T-...` key; empty = leave MakeMKV's settings alone |
| Template / env | `PUID` / `PGID` / `UMASK` | `99` / `100` / `000` (Unraid's nobody:users) |
| Template (optional) | `/temp` mapping | rips in progress go to `/temp/series-ripper` instead of `/output/.series-ripper-tmp` |
| env | `DVDRIPPER_TEMP` | overrides the temp folder completely |
| env | `DVDRIPPER_PORT` | `8080` |
| env | `DVDRIPPER_DB` | `/config/series-ripper.db` |
| Web UI | Show name, season, next episode, folder layout (`Show/Season 1`, `Show/` only, or flat), season folder name (`Season 1` / `Season 01`), min. episode length, detection switches, Automatic Mode, poll interval, auto-eject | |

The web UI has no login. Keep it on your LAN and don't forward the port to the
internet.

## Development

```bash
python3 -m unittest tests.test_units tests.test_integration -v
docker build -t series-ripper:dev .
```

`tests/fixtures/kare_kano_vol5_info.txt` is a real MakeMKV 2.0.0 scan of a
DVD. `tests/mock_makemkvcon.py` replays it and behaves like the real tool
where it matters. The integration test starts the real server against the
mock: scan, Automatic Mode → Review Queue → rip, duplicate-disc skip, and a
Special rip.

To use a newer MakeMKV, change `MAKEMKV_VERSION` in the `Dockerfile`. Versions
are announced in the "MakeMKV x.y.z for Linux is available" forum thread.

### What has been tested, and what hasn't

Tested:
- The image builds with MakeMKV 2.0.0 and starts.
- The free beta key is fetched from the live forum and installed.
- Files and folders get the right owner.
- The full scan/review/rip flow, against the mock, both in the container
  and in the web UI.

Not yet tested in the container:
- A real optical drive. The parsing was checked against real MakeMKV output
  from a DVD, but no disc was read inside Docker.
- Blu-ray and 4K UHD discs. The Blu-ray "Play All" check uses the same length
  rule as DVDs; it was tested with made-up Blu-ray-like numbers only.

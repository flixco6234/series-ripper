# Series Ripper

Rips TV series from **DVD, Blu-ray and 4K UHD** discs with
[MakeMKV](https://www.makemkv.com/) and names the episodes the Plex/Jellyfin way:

```
Serien/Kare Kano/Season 1/Kare Kano - S01E14.mkv
Serien/Kare Kano/Season 1/Kare Kano - S01E15 - Episode Title.mkv
Serien/Kare Kano/Season 0/Kare Kano - S00E01.mkv          (Special)
```

The season folders can also be `Season 01`, or left out (Settings → Folder layout).

It runs as a Docker container with a web UI, made for Unraid. The app is a
single Rust program; MakeMKV does the actual disc reading.

- **Several drives at once:** every drive gets its own card on the Dashboard,
  its own show and its own Automatic Mode switch. Two drives can scan and rip
  at the same time, also into the same show: episode numbers are reserved per
  rip, so two discs never get the same number.
- **Manual rip:** scan a disc, check the proposed episode numbers, rip.
- **Automatic Mode:** insert a disc and walk away. It scans and rips on its own,
  ejects the disc, and continues the episode count for the next disc. A disc it
  is not sure about goes to the **Review Queue** instead of being guessed.
- **Shows:** each series remembers its season, next episode and next Special,
  so switching between series is one click. A show can have its own library
  folder (e.g. `/output/Anime`).
- **Episode detection**, each one switchable in Settings:
  - menus and extras (too short) become Specials;
  - **"Play All" titles** are unticked automatically (a title exactly as long as
    several episodes added up);
  - long titles, and titles much longer or shorter than the disc's other
    episodes, are flagged;
  - episode numbers written in the disc's own title names are used.
- **Failed titles get a retry:** they go to the Review Queue with their episode
  numbers kept, so you can clean the disc and rip just those again.
- **History** of every ripped title: file, size, length, speed, result.
- **Notifications** via Discord, ntfy, Gotify or a generic JSON webhook when a
  disc is done, needs review or fails.
- **Live web UI:** progress, speed and time left update instantly (pushed by
  the server, no polling); eject / close-tray buttons; optional password.
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
- One or more optical drives. USB is fine.
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
(`/dev/sgN`), so pass that to the container. Also pass the drive's `/dev/srN`
device: the app uses it to see instantly when a disc goes in or out, and to
eject. Without it, the app has to ask MakeMKV every few seconds (only while a
drive without `/dev/srN` is in Automatic Mode), and eject doesn't work.

`lsscsi -g` on the host shows both at the end of each drive's line. The
drives must be connected when the container starts. A USB drive can get a
different number after being re-plugged.

**More than one drive:** pass each drive's `/dev/sgN` and `/dev/srN` pair. In
the Unraid template, click *Add another Path, Port, Variable, Label or Device*
and choose *Device* for each extra one. The app recognizes a drive by its
model and serial number, so its name, show and Automatic Mode setting stay
with it even if its device numbers change.

`unraid/series-ripper.xml` (Unraid template) and `docker-compose.yml` are
examples of a working setup.

## First run

1. Open the web UI, go to **Shows**, and add the series: name, season, and the
   episode number the next disc starts at.
2. On the **Dashboard**, pick that show on the drive's card.
3. **Scan** (on the card or under **Rip**). A DVD takes a minute or two;
   Blu-ray and UHD discs can take several minutes. Check the table:
   - Tick/untick titles; **Episode #** renumbers itself.
   - **Special?** puts a title in Season 0.
   - The **Notes** column says why something was unticked or flagged.
4. **Rip Selected.** Watch progress on the Dashboard. After the disc, the show's
   "next episode" moves on by itself.
5. When a couple of discs went the way you wanted, tick **Automatic Mode** on
   the drive's card.

### Automatic Mode and the Review Queue

A drive in Automatic Mode scans every disc you insert and decides:

- **Rips on its own** when nothing is flagged on the titles it would rip.
  Play All titles, menus and extras are dropped automatically.
- **Parks the disc in the Review Queue** (nothing ripped) if a title it
  would rip is too long, much longer or shorter than the other episodes,
  or if the disc's own names contradict the numbering. Leave the disc in
  (or put it back - in any drive, you can pick it), fix the table in the
  Review Queue, click **Rip These**.
- **Skips a disc it already ripped completely**, same label and same titles.
  This covers a container restart with a disc in the tray. Use Manual Rip
  to rip it again on purpose (or switch the check off in Settings).

Titles that fail to rip also land in the Review Queue, with the episode
numbers they had, so a retry fills exactly the gap.

### Notifications

Settings → Notifications. Pick the service and paste the URL:

| Service | URL |
|---|---|
| Discord | the channel's webhook URL |
| ntfy | the topic URL, e.g. `https://ntfy.sh/my-ripper` (or your own server) |
| Gotify | `https://gotify.example/message?token=<app token>` |
| Generic JSON | any URL; gets `{"event", "title", "message"}` as a POST |

"Save & send a test" checks it right away.

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
  spent a while on the review, eject and re-insert the disc, then retry it
  from the Review Queue. The log says so too.
- **Java** is included, which MakeMKV uses for some Blu-ray menus.
- MakeMKV's data (key, settings, downloaded LibreDrive data) is kept in
  `/config/makemkv`.

## Settings reference

| Where | Setting | Default |
|---|---|---|
| Template / env | `MAKEMKV_KEY` | `BETA` (auto-fetch free key); a `T-...` key; empty = leave MakeMKV's settings alone |
| Template / env | `UI_PASSWORD` | empty = no password; otherwise the browser asks for it (any user name) |
| Template / env | `PUID` / `PGID` / `UMASK` | `99` / `100` / `000` (Unraid's nobody:users) |
| Template (optional) | `/temp` mapping | rips in progress go to `/temp/series-ripper` instead of `/output/.series-ripper-tmp` |
| env | `DVDRIPPER_TEMP` | overrides the temp folder completely |
| env | `DVDRIPPER_PORT` | `8080` |
| env | `DVDRIPPER_DB` | `/config/series-ripper.db` |
| Dashboard | per drive: name, show, Automatic Mode | |
| Shows | name, season, next episode, next Special, library folder | |
| Settings | library folder, folder layout (`Show/Season 1`, `Show/` only, or flat), season folder name (`Season 1` / `Season 01`), min. episode length, long-title limit, detection switches, auto-eject, skip already-ripped discs, Automatic Mode for new drives, check interval, notifications, makemkvcon path and extra options | |

Without `UI_PASSWORD` the web UI has no login. Keep it on your LAN and don't
forward the port to the internet (with a password too, it's plain HTTP).

## Updating from the Python version (2.x)

The database in `/config` is reused as it is. On the first start:

- the show from the old Settings page becomes the first entry under **Shows**,
  with its season and next episode;
- the old global Automatic Mode switch becomes the default for drives; set it
  per drive on the Dashboard;
- the review queue, the log and the list of already-ripped discs are kept.

## Development

```bash
cargo test                       # unit tests + the integration test below
docker build -t series-ripper:dev .
```

`tests/fixtures/kare_kano_vol5_info.txt` is a real MakeMKV 2.0.0 scan of a
DVD. `src/bin/mock_makemkvcon.rs` replays it on two pretend drives and behaves
like the real tool where it matters. `tests/integration.rs` starts the real
server against the mock and goes through Automatic Mode → Review Queue → rip,
the duplicate-disc skip, two drives ripping at once, the episode-number
reservation between drives, a failed rip and its retry, cancelling, and a
Special.

Pushing a branch other than `main` publishes `ghcr.io/<owner>/series-ripper:<branch>`,
so a change can be tried on Unraid before it reaches `:latest`.

To use a newer MakeMKV, change `MAKEMKV_VERSION` in the `Dockerfile`. Versions
are announced in the "MakeMKV x.y.z for Linux is available" forum thread.

### What has been tested, and what hasn't

Tested:
- The image builds with MakeMKV 2.0.0 and starts.
- The free beta key is fetched from the live forum and installed.
- Files and folders get the right owner.
- The full scan/review/rip flow with one and two drives, against the mock,
  in the container and in the web UI.

Not yet tested in the container:
- A real optical drive. The parsing was checked against real MakeMKV output
  from a DVD, but no disc was read inside Docker. The kernel disc-detection
  (`/dev/srN`) and eject were not tried with real hardware either.
- Two real drives ripping at the same time. MakeMKV supports it, but how fast
  it is depends on your USB controller and disks.
- Blu-ray and 4K UHD discs. The Blu-ray "Play All" check uses the same length
  rule as DVDs; it was tested with made-up Blu-ray-like numbers only.

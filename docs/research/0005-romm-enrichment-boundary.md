# RomM and the Games Library: enrichment boundary

Findings for #5, part of #3. Researched 2026-10-28 against RomM's source and docs at the pinned release and a local
test run. The test used a synthetic Games Library of random bytes with No-Intro-style names, mounted read-only. No
ROMs, no Verified Set, no Operator data and no Operator credentials were used. All containers, volumes, images and
scratch files were removed afterwards (§5).

Pinned: **RomM 5.4.1** ([release](https://github.com/rommapp/romm/releases/tag/5.4.1), tag commit
`9c74d0143fbb876f2f62260b91c6fd0b3f590226`), image
`rommapp/romm:5.4.1@sha256:8677fb7b81b1d3db20b849e989a1dadc2a11159e51eb34a2acdcb01102274edf` (index; amd64
`sha256:923a808b…`, arm64 `sha256:ebfd7670…`). 5.4.1 is the latest stable release; it is a security and dependency
update on 5.4.0. Docs are cited at rommapp/docs commit `262f982`, because that repo has no tags.

## Recommendation

**Keep enrichment in RomM. No Organizer enrichment capability now.**

The Organizer stops at publishing DAT-named files into managed per-system folders of the Games Library, under an
approved Change Plan. RomM, in the Media Server Role, starts at a read-only mount of that Library. It identifies
platforms and Games from the folder and file names and payload hashes the Organizer already produces. It fetches
artwork and descriptive metadata into its own volume and Datastore, never into the Library.

Why:

- DAT names and per-system ES-DE folders are what RomM's matching is built for (§1). In the test, a scan without any
  API key identified the platform from the folder name and fetched cover art and a screenshot by exact No-Intro
  filename for every name in libretro's catalogue. Hasheous adds payload-hash matching against No-Intro and Redump
  signatures, also without a key.
- RomM runs correctly with the Library mounted `:ro` (§2). Every Library-writing feature that was tried (rename,
  delete, add platform, `gamelist.xml`/Pegasus export) failed cleanly in the test, and the fixture's checksums and
  mtimes were unchanged. Uploads use the same write primitives.
- RomM fits the Protected Service pattern through native OIDC, and the Baseline as a non-root container with a
  read-only root filesystem, given two tmpfs mounts (§3).

The remaining gaps (§4) belong to deployment or to upstream RomM, not to the Organizer. One conditional gap is noted:
if the Operator later wants ES-DE devices that read the Library directly to see artwork without scraping on the
device, that would be a separately specified capability to publish ES-DE media and `gamelist.xml` into the Library.
RomM cannot do it under `:ro` and must not be given write access. Nothing in #3 asks for this today.

| | Organizer (Title Select) | RomM (Media Server) |
| --- | --- | --- |
| Library files and names | Publishes DAT-named files, loose or one ZIP per Game, into managed per-system folders | Reads only (`:ro`) |
| Game identity | DAT Revision and Audit evidence | Its own match by folder, filename and hash, per provider |
| Artwork and descriptions | None | Fetches and stores in `/romm/resources` and its Datastore |
| Saves, states, user uploads | None | `/romm/assets`, outside the Library |
| Trigger | May call RomM's scan API after a publish (optional) | Scans on request or schedule |

## 1. Does RomM meet the artwork and metadata need?

Yes, for a Library laid out as the Organizer publishes it.

**Folder layout.** RomM scans "Structure A" by default: a `roms/` folder with one folder per platform
([folder-structure.md L10-23](https://github.com/rommapp/docs/blob/262f98248f577a2a49caa00e32669c283fdabf80/docs/getting-started/folder-structure.md)).
Platform folders are matched to RomM slugs case-insensitively, and RomM recognises the folder names that ES-DE,
Batocera and RetroBat use, "so a library that already reads `megadrive/`… scans as-is" (ibid. L52). Custom names
map through `system.platforms`, and deeper layouts use a `filesystem.structure` template (ibid. L193-216). The Games
Library's per-system ES-DE folders can therefore be mounted at `/romm/library/roms`. In the test, folder `atari2600`
was "identified as Atari 2600" (§5).

**Archives and hashing.** RomM computes CRC32, MD5 and SHA-1 per file. For a ZIP it hashes the decompressed members
and records each one as an archive member
([roms_handler.py L893-1000](https://github.com/rommapp/romm/blob/5.4.1/backend/handler/filesystem/roms_handler.py#L893-L1000),
`_calculate_rom_hashes` at
[L1099](https://github.com/rommapp/romm/blob/5.4.1/backend/handler/filesystem/roms_handler.py#L1099)). The test
confirmed this: the SHA-1 RomM stored for `Combat (USA).zip` was the member's SHA-1, not the archive's. So both
loose files and one-ZIP-per-Game are hashed by payload, which is what DAT checksums describe.
`filesystem.skip_hash_calculation` turns hashing off, which also disables hash matching.

**Providers.** Default priorities are in
[config_manager.py L830-870](https://github.com/rommapp/romm/blob/5.4.1/backend/config/config_manager.py#L830-L870).
Unless another link is given, access requirements come from
[metadata-providers.md](https://github.com/rommapp/docs/blob/262f98248f577a2a49caa00e32669c283fdabf80/docs/getting-started/metadata-providers.md),
cited by line.

| Provider | Account or key | Matches by | Gives |
| --- | --- | --- | --- |
| libretro thumbnails | None; always enabled ([libretro_handler.py L74-76](https://github.com/rommapp/romm/blob/5.4.1/backend/handler/metadata/libretro_handler.py#L74-L76)) | Exact filename, then fuzzy (≥0.8) on tag-stripped names | Box art, title screens, snaps |
| Hasheous | None (`HASHEOUS_API_ENABLED`); public or self-hosted (L102-108) | MD5/SHA-1/CRC via `/Lookup/ByHash`; records No-Intro and Redump signature matches ([hasheous_handler.py L465-475](https://github.com/rommapp/romm/blob/5.4.1/backend/handler/metadata/hasheous_handler.py#L465-L475)) | IGDB-proxied titles, descriptions and art |
| IGDB | Twitch account and phone 2FA (L52) | Name search | Metadata, art |
| ScreenScraper | Free account; hash matching since v4.4 (L14) | Hash and name | Metadata, art, manuals |
| RetroAchievements | API key; "Matching is hash-only" via RAHasher (L128-132) | Hash | Achievements |
| LaunchBox | None; downloads the whole database locally (L92-98) | Filename | Metadata, box art |
| Playmatch | None; manual matches send hashes and filenames to the community service (L110-118) | Hash | Provider IDs |
| MobyGames | Paid API key (L86-90) | Name | Metadata, art |
| SteamGridDB | API key | Name (cover search) | Art |
| ES-DE `gamelist.xml` | None; read from the platform folder | Path | Metadata and art already in the Library |

A Hasheous or RetroAchievements hash match marks a Game as verified in RomM. The docs' "Quick Starter" recommends
Hasheous alone "for users who want to avoid API keys" (L39-44). RomM's own igir guide mounts a verified tree `:ro`
and says that after a scan "everything should match cleanly against providers"
([igir.md L182-185](https://github.com/rommapp/docs/blob/262f98248f577a2a49caa00e32669c283fdabf80/docs/ecosystem/igir.md)).

**Storage.** Artwork is downloaded to `/romm/resources`. Saves, states and user uploads go to `/romm/assets`, and
config is `/romm/config/config.yml`
([config/\_\_init\_\_.py L46-55](https://github.com/rommapp/romm/blob/5.4.1/backend/config/__init__.py#L46-L55)).
Metadata is held in the Datastore. In the test, four matched Games stored a cover (big and small) and one
screenshot each under `/romm/resources/roms/<platform id>/<rom id>/`, 888 KB in total. Nothing was written next to
the ROMs.

**Inference, not tested.** Real No-Intro ROMs should also match Hasheous by hash; the test used random bytes, which
Hasheous correctly did not match. Coverage per platform depends on each provider's catalogue. A provider's match is
its own identification, not a DAT match, so the Organizer's Audit stays authoritative for identity.

## 2. Does RomM stay read-only on the Library?

Yes, if the Library is mounted `:ro`. A default scan does not write to the Library, and every feature that can
write fails cleanly against a read-only mount.

These code paths write under `/romm/library`; the write primitives are in
[base_handler.py L554-975](https://github.com/rommapp/romm/blob/5.4.1/backend/handler/filesystem/base_handler.py#L554-L975).
"Test" gives the observed result against the `:ro` mount.

| Feature | Code | Default | Test |
| --- | --- | --- | --- |
| Rename on edit (`fs_name` change) | [roms/\_\_init\_\_.py L2369-2390](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/roms/__init__.py#L2369-L2390) | User action | 500 "Failed to rename ROM 4 on disk", `Errno 30` |
| Delete from filesystem | [roms/\_\_init\_\_.py L2509-2535](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/roms/__init__.py#L2509-L2535) | User action | `failed_ids: [4]`, `Errno 30`; database row kept |
| Add platform (mkdir) | [platform.py L53](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/platform.py#L53) | User action | 500, `Errno 30` on `/romm/library/roms/nes` |
| ROM upload; manual, screenshot, soundtrack files | [roms/upload.py L252](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/roms/upload.py#L252), [roms/files.py](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/roms/files.py) | User action | Not run; same write primitives |
| Firmware upload and delete | [firmware.py L99](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/firmware.py#L99), [L359](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/firmware.py#L359) | User action | 500; targets `/romm/library/bios`, outside the Library mount |
| Setup wizard creates folders | [heartbeat.py L414](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/heartbeat.py#L414) | Until the first admin exists | Not run; admin created through the API |
| `gamelist.xml` / Pegasus export after a scan | [sockets/scan.py L1466-1505](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/sockets/scan.py#L1466-L1505) | Off (`scan.gamelist.export`, `scan.pegasus.export`) | Turned on: "Failed to export gamelist.xml… Read-only file system: '/romm/library/roms/atari2600/covers'"; the scan still completed |
| Library conversion (rom-converto) | [convert_library.py L86](https://github.com/rommapp/romm/blob/5.4.1/backend/tasks/manual/convert_library.py#L86) | Off (`ROM_CONVERTO_ENABLED`, [config L82](https://github.com/rommapp/romm/blob/5.4.1/backend/config/__init__.py#L82)) | Not run |
| Switch title-ID rename during a scan | [roms_handler.py L1283-1352](https://github.com/rommapp/romm/blob/5.4.1/backend/handler/filesystem/roms_handler.py#L1283-L1352) | Off (`filesystem.embed_switch_title_ids`) | Not applicable |

Saves and states go to `/romm/assets`, not the Library. Apart from the mount, only the Editor and Admin roles hold
`roms.write`, `platforms.write` and `firmware.write`. A Viewer can read the Library and manage only their own
collections and assets
([permissions_map.py L75-110](https://github.com/rommapp/romm/blob/5.4.1/backend/handler/auth/permissions_map.py#L75-L110),
[constants.py L38-75](https://github.com/rommapp/romm/blob/5.4.1/backend/handler/auth/constants.py#L38-L75)). The
`:ro` mount is the guarantee; roles only reduce how often users hit the 500s.

**Inference.** Leave the export, conversion and Switch-rename settings off in a deployment. If turned on they would
only fail, but leaving them off avoids error noise and keeps RomM's configuration consistent with the mount.

## 3. Does it fit the Protected Service pattern and the Baseline?

Mostly. Criteria are from home-ops ADRs 0006 (Protected Services), 0007 (Baseline via `extends`) and 0009 (Library
isolation by Role).

| Criterion | RomM 5.4.1 | Fit |
| --- | --- | --- |
| Native OIDC (ADR 0006 mechanism 1) | `OIDC_ENABLED`, `OIDC_AUTOLOGIN` and `DISABLE_USERPASS_LOGIN`; roles from a claim via `OIDC_CLAIM_ROLES` and `OIDC_ROLE_VIEWER/EDITOR/ADMIN` ([config L216-265](https://github.com/rommapp/romm/blob/5.4.1/backend/config/__init__.py#L216-L265)); callback `/api/oauth/openid`, HTTPS required ([oidc/index.md L19, L53, L166](https://github.com/rommapp/docs/blob/262f98248f577a2a49caa00e32669c283fdabf80/docs/administration/oidc/index.md)) | Yes. Set `OIDC_ALLOW_REGISTRATION` deliberately; it defaults to true ([config L253](https://github.com/rommapp/romm/blob/5.4.1/backend/config/__init__.py#L253)) |
| Forward auth with identity handoff (mechanism 2) | No trusted-header login in the code or docs. The docs mention reverse-proxy auth only as a gate for unauthenticated downloads ([downloads.md L41](https://github.com/rommapp/docs/blob/262f98248f577a2a49caa00e32669c283fdabf80/docs/using/downloads.md)) | No. A gate (mechanism 3) would mean a double login |
| Client Paths | Client API Tokens for apps and scripts ([authentication.md L14-15, L73-82](https://github.com/rommapp/docs/blob/262f98248f577a2a49caa00e32669c283fdabf80/docs/administration/authentication.md)) and HTTP Basic ([api-authentication.md L13, L46](https://github.com/rommapp/docs/blob/262f98248f577a2a49caa00e32669c283fdabf80/docs/developers/api-authentication.md)). The test drove every call with Basic | Yes, for device clients |
| First admin | `POST /api/users` is open while no admin exists ([user.py L90-140](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/user.py#L90-L140); setup wizard [heartbeat.py L290-301](https://github.com/rommapp/romm/blob/5.4.1/backend/endpoints/heartbeat.py#L290-L301)). In the test it needed only a CSRF cookie | Create the admin before the router is published |
| Digest pin | Index digest above | Yes |
| Healthcheck | The image has none; `GET /api/heartbeat` ([observability.md L61-66](https://github.com/rommapp/docs/blob/262f98248f577a2a49caa00e32669c283fdabf80/docs/administration/observability.md)); `wget` and `curl` are in the image (test) | Add a compose healthcheck |
| No anonymous volumes | `VOLUME ["/romm", "/redis-data"]` ([Dockerfile L391](https://github.com/rommapp/romm/blob/5.4.1/docker/Dockerfile#L391)) | Cover both with named volumes or binds |
| Non-root | The image creates `romm` 1000:1000 but sets no `USER` ([Dockerfile L355-360](https://github.com/rommapp/romm/blob/5.4.1/docker/Dockerfile#L355-L360)) | Yes, with `user: "1000:1000"` (tested) |
| `cap_drop: ALL`, `no-new-privileges` | Listens on unprivileged port 8080 | Yes (tested) |
| Read-only root FS (advisory) | Init copies the gunicorn config to `/tmp` "so this works on read-only root filesystems" ([init L128-131](https://github.com/rommapp/romm/blob/5.4.1/docker/init_scripts/init#L128-L131)); PID files in `/tmp` | Yes, with tmpfs at `/tmp` and `/etc/nginx/conf.d` (tested; without the second, nginx never listens on 8080) |
| Datastore | MariaDB 10.11+ (default), MySQL 8.0.17+ or Postgres; no SQLite ([databases.md L14-18](https://github.com/rommapp/docs/blob/262f98248f577a2a49caa00e32669c283fdabf80/docs/install/databases.md); [config L101](https://github.com/rommapp/romm/blob/5.4.1/backend/config/__init__.py#L101)) | One per Stack, as usual |
| Redis/Valkey | Embedded valkey when `REDIS_HOST` is unset, persisted at `/redis-data` ([config L106](https://github.com/rommapp/romm/blob/5.4.1/backend/config/__init__.py#L106)) | Embedded is enough; it holds sessions and job queues |
| Library mount (ADR 0009) | Library at `/romm/library/roms`, `read_only: true`, `create_host_path: false` (tested) | Yes |
| Docker socket, Host access | None needed | Yes |
| Outbound network | Provider APIs. libretro thumbnails is always on; Hasheous and Playmatch receive file hashes | Allow HTTPS egress. Hashes leave the Host; payloads do not |
| Memory | Test, idle after scans: RomM 958 MiB, MariaDB 143 MiB | Set a limit with headroom; the image runs gunicorn, nginx, valkey and four RQ processes |

**Inference.** The gaps are deployment work, not blockers: add a healthcheck, cover both image volumes, and create
the admin before exposing the router (or use OIDC with `DISABLE_USERPASS_LOGIN`). Whether to self-host Hasheous, so
that hashes stay on the Host, is an Operator choice.

## 4. What gap would remain for an Organizer enrichment capability?

None that the Organizer should fill now. Established:

- **libretro-only matches fail on the first scan in 5.4.1.** On a new-ROM or complete scan, when libretro is the
  only source that matches, `rom_attrs` has no `name`, and the "identified as" log line raises `KeyError: 'name'`
  ([scan_handler.py L1724-1727](https://github.com/rommapp/romm/blob/5.4.1/backend/handler/scan_handler.py#L1724-L1727)).
  The test reproduced it: four of five files logged "Error scanning ROM …: 'name'". Each ROM row was kept with a
  filename-derived name, but its artwork was dropped. An `unmatched` scan then matched all four, because that path
  falls back to the filename
  ([L1576-1612](https://github.com/rommapp/romm/blob/5.4.1/backend/handler/scan_handler.py#L1576-L1612)).
  Upstream fixed this after 5.4.1: "fall back to filename name when only libretro matches"
  ([rommapp/romm#5241](https://github.com/rommapp/romm/pull/5241), commit
  [`d8f08a6`](https://github.com/rommapp/romm/commit/d8f08a60f23f96db5fb1dcba9cacf77046d78e2c)). It only matters
  when neither Hasheous nor a keyed provider matches. Pin the first release that contains the fix, or run an
  `unmatched` scan after the first scan.
- **Identity is the provider's, not the DAT's.** RomM matches by filename and hash against each provider's
  catalogue. Hasheous records whether a hash appears in No-Intro or Redump signatures, but RomM has no input for
  "this file is Game Y of DAT Revision X". DAT-named files and their payload hashes are the strongest identity RomM
  can receive, and both already reach it through the read-only Library.

Inference:

- The only enrichment RomM cannot provide under `:ro` is ES-DE `gamelist.xml` and media inside the Library, for
  devices that read the Library without RomM. If the Operator wants that, it is a separately specified Organizer
  capability: publish ES-DE media and `gamelist.xml` into library folders under a Change Plan. RomM already reads
  `gamelist.xml` as a provider, so such files would feed RomM too.
- The Organizer could call `POST /api/tasks/scan` with `platform_fs_slugs` after a publish, so that RomM picks up
  changes promptly. This is an integration, not enrichment, and needs a token with `tasks.run`, which only Admins
  hold. The alternatives are RomM's own scheduled or filesystem-watch rescans (`ENABLE_SCHEDULED_RESCAN`,
  `ENABLE_RESCAN_ON_FILESYSTEM_CHANGE`, both off by default,
  [config L284-291](https://github.com/rommapp/romm/blob/5.4.1/backend/config/__init__.py#L284-L291)).

## 5. Hands-on log

Run on a Mac with Docker Desktop (arm64) on 2026-10-28. The scratch dir was the agent's session folder, outside the
repo, shown here as `$H`. No real ROMs were used: the files hold random bytes under No-Intro-style names, and one
name is deliberately absent from every catalogue.

Fixture:

```sh
mkdir -p "$H/games/library/atari2600" && cd "$H/games/library/atari2600"
head -c 4096 /dev/urandom > "Adventure (USA).a26"
head -c 4096 /dev/urandom > "Asteroids (Japan, USA) (En).a26"
head -c 2048 /dev/urandom > "Not A Real Game (World).a26"
mkdir ../z && head -c 2048 /dev/urandom > "../z/Combat (USA).a26" && head -c 4096 /dev/urandom > "../z/River Raid (USA).a26"
(cd ../z && zip -q -X "../atari2600/Combat (USA).zip" "Combat (USA).a26" \
          && zip -q -X "../atari2600/River Raid (USA).zip" "River Raid (USA).a26") && rm -r ../z
cd "$H" && { find games | sort | while read -r f; do stat -f '%N %z %m %p' "$f"; done
             find games -type f -exec shasum {} \; | sort; } > before.txt
```

`$H/compose.yaml`, hardened in the Baseline's style, with throwaway credentials:

```yaml
name: romm-eval
services:
  romm:
    image: rommapp/romm:5.4.1@sha256:8677fb7b81b1d3db20b849e989a1dadc2a11159e51eb34a2acdcb01102274edf
    user: "1000:1000"
    read_only: true
    security_opt: ["no-new-privileges=true"]
    cap_drop: [ALL]
    tmpfs:
      - /tmp:uid=1000,gid=1000,mode=0755
      - /etc/nginx/conf.d:uid=1000,gid=1000,mode=0755
    environment:
      DB_HOST: romm-db
      DB_NAME: romm
      DB_USER: romm-user
      DB_PASSWD: eval-only-db-password
      ROMM_AUTH_SECRET_KEY: ${ROMM_AUTH_SECRET_KEY}
      HASHEOUS_API_ENABLED: "true"
      DISABLE_EMULATOR_JS: "true"
    volumes:
      - romm_data:/romm
      - redis_data:/redis-data
      - type: bind
        source: ./games/library
        target: /romm/library/roms
        read_only: true
        bind: {create_host_path: false}
    ports: ["127.0.0.1:18080:8080"]
    depends_on:
      romm-db: {condition: service_healthy}
  romm-db:
    image: mariadb:11@sha256:c4124d1afa875e47f2892edf26b482fa54f796e2f342d3717a04ca091a22a9bd
    environment:
      MARIADB_ROOT_PASSWORD: eval-only-root-password
      MARIADB_DATABASE: romm
      MARIADB_USER: romm-user
      MARIADB_PASSWORD: eval-only-db-password
    volumes: [db_data:/var/lib/mysql]
    healthcheck:
      test: ["CMD", "healthcheck.sh", "--connect", "--innodb_initialized"]
      start_period: 30s
      start_interval: 5s
      interval: 10s
      timeout: 5s
      retries: 5
volumes: {romm_data: {}, redis_data: {}, db_data: {}}
```

Run attached, then drive the API from a second shell:

```sh
ROMM_AUTH_SECRET_KEY=$(openssl rand -hex 32) docker compose up
B=http://127.0.0.1:18080; A=(-u 'evaladmin:eval-only-Passw0rd!')
curl -s -c cj -b cj $B/api/heartbeat >/dev/null; T=$(awk '/csrftoken/{print $7}' cj)
curl -s -b cj -X POST $B/api/users -H "X-CSRFToken: $T" -H 'Content-Type: application/json' \
  -d '{"username":"evaladmin","email":"eval@example.invalid","password":"eval-only-Passw0rd!","role":"admin"}'
curl -s "${A[@]}" -X POST $B/api/tasks/scan -H 'Content-Type: application/json' -d '{"type":"complete"}'
curl -s "${A[@]}" -X POST $B/api/tasks/scan -H 'Content-Type: application/json' -d '{"type":"unmatched","apis":["libretro"]}'
curl -s "${A[@]}" "$B/api/roms?limit=50"
curl -s "${A[@]}" -X PUT $B/api/roms/4 -F 'fs_name=Renamed (World).a26'
curl -s "${A[@]}" -X POST $B/api/roms/delete -H 'Content-Type: application/json' -d '{"roms":[4],"delete_from_fs":[4]}'
curl -s "${A[@]}" -X POST $B/api/platforms -H 'Content-Type: application/json' -d '{"fs_slug":"nes"}'
```

Observations:

1. **Startup.** RomM started as UID 1000 with a read-only root, `cap_drop: ALL` and `no-new-privileges`. Migrations
   ran, and RomM created `/romm/config/config.yml` in its volume. The heartbeat reported `LIBRETRO_API_ENABLED: true`,
   `HASHEOUS_API_ENABLED: true` and every keyed provider disabled. No permission or read-only errors appeared across
   two starts.
2. **First admin.** Without the CSRF cookie and header the call returned 403 "CSRF token verification failed". With
   them it returned 201, and no other credential was needed.
3. **Complete scan, Hasheous and libretro.** The log showed "Folder atari2600[atari2600] identified as Atari 2600"
   and "5 roms found". Hasheous was queried for all five and matched none (random bytes). "Not A Real Game
   (World).a26 not identified". The other four logged "Error scanning ROM …: 'name'" (§4). All five rows exist,
   named from the filename without tags (`Adventure`, `Combat`…), with CRC32, MD5 and SHA-1. For
   `Combat (USA).zip` the stored SHA-1 `6552b41f…` equals `unzip -p … | shasum` of the member; the archive's own
   SHA-1 is `ae268e80…`.
4. **Unmatched scan, libretro only.** Four of five were "identified as" their names, with covers such as
   `https://thumbnails.libretro.com/Atari%20-%202600/Named_Boxarts/Combat%20%28USA%29.png`. Artwork was saved to
   `/romm/resources/roms/1/<id>/cover/{big,small}.png` and `screenshots/0.jpg`, 888 KB in total. `/redis-data`
   held 4 KB.
5. **Write probes against `:ro`.** Results are in the §2 table. Rename returned 500. Delete-from-filesystem reported
   `Errno 30`. Add-platform returned 500. Firmware upload returned 500 (`Permission denied: '/romm/library/bios'`,
   because Docker creates the mountpoint's parent owned by root). After `scan.gamelist.export` and
   `scan.pegasus.export` were set in `config.yml` and RomM restarted, both exports failed with `Errno 30`, and the
   scan still completed.
6. **Without the `/etc/nginx/conf.d` tmpfs.** The log showed "can't create /etc/nginx/conf.d/default.conf: Read-only
   file system". RomM's own startup succeeded, but nginx kept its stock config and port 8080 refused connections.
7. **Resources.** `docker stats` after the scans: RomM 957.8 MiB, MariaDB 143.1 MiB.
8. **Library unchanged.** After the run, `diff before.txt after.txt` was empty: same files, sizes, mtimes, modes
   and SHA-1s.

Teardown and confirmation:

```sh
docker compose down -v --rmi all
docker ps -a --filter name=romm-eval; docker volume ls --filter name=romm-eval; docker network ls --filter name=romm-eval
rm -rf "$H"
```

All three listings were empty and both images were removed. The scratch dir, and the source and docs clones used
for this research, were deleted.

## Sources

- RomM 5.4.1 release: <https://github.com/rommapp/romm/releases/tag/5.4.1>
- RomM source at 5.4.1: <https://github.com/rommapp/romm/tree/5.4.1>, files linked inline
- RomM docs at `262f982`: <https://github.com/rommapp/docs/tree/262f98248f577a2a49caa00e32669c283fdabf80/docs>, pages
  linked inline
- Upstream fix after 5.4.1: <https://github.com/rommapp/romm/pull/5241>
- Image digests: `docker buildx imagetools inspect rommapp/romm:5.4.1`, 2026-10-28
- home-ops `GLOSSARY.md` (Media Server, Organizer, Library, Protected Service, Baseline, Datastore, Client Path),
  ADRs 0006, 0007, 0009 and 0013, and `docs/research/0734-games-organizer-spike.md` (Games Library layout)
- #3 (map) and #5

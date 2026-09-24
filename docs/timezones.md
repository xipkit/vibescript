# Timezone sources

Named zones use the first source that contains valid TZif data:

1. `ZONEINFO`, when nonempty, naming a directory or an uncompressed `.zip` archive.
2. Installed platform sources: the usual Unix zoneinfo directories, Android's packed `tzdata` databases, or an iOS app's `zoneinfo.zip`. WASI has no platform source, so a guest uses `ZONEINFO` or the bundled database.
3. The bundled IANA 2026c database, containing 598 zones in 408,467 bytes.

An absent, unreadable, unsupported or malformed source falls through to the next source. Quota exhaustion and cancellation propagate immediately. ZIP loading follows Go's time-package archive conventions, including stored entries and an end record without a ZIP comment. Entry names preserve their bytes. Windows filesystem paths preserve WTF-8 surrogates and replace other invalid bytes individually, matching Go; Unix paths preserve their original bytes.

Local timezone selection depends on the platform:

| Platform | Local timezone |
| --- | --- |
| Unix | `TZ`, including a leading colon, a zone name or an absolute TZif path; `/etc/localtime` when unset; UTC when empty or unavailable |
| Windows | Win32 timezone information, with English abbreviation mappings and capital-letter fallback for unknown names |
| Android and iOS | UTC, matching Go's local-zone implementation |
| WASI | `TZ` as on Unix, with names resolved through `ZONEINFO` or the bundled database and absolute paths read from the host's preopens; UTC when unset, empty or unavailable |

`ZONEINFO` affects named lookup; it does not override Unix `TZ` selection. A WASI guest sees only the environment its host passes, so the local zone stays UTC unless the host sets `TZ`, for example with `wasmtime run --env TZ=America/New_York`. Go's `wasip1` port always uses UTC; honoring `TZ` is a deliberate hosting convenience. The configuration is captured on first use. Like Go's `time.Local`, the local zone is loaded once per process, outside any call's quotas: each resolution charges its execution for the zone storage it retains but no steps for loading it, so repeated resolutions return identical data at the same cost in every call. A loaded zone, or a UTC fallback because no source exists where the configuration points, lasts for the process. When a source exists but cannot be read or parsed, or the Windows query fails, that resolution uses UTC and the next one loads again, so a transient failure cannot fix the process at UTC. Named zones are loaded and parsed on every lookup, as Go's `LoadLocation` does, so installed data changes are seen; only the bundled database's directory, which is part of the binary, is indexed once per process, so a lookup charges a few binary-search probes instead of a directory scan. Zone data is accounted independently for each call. Windows captures the operating system's current rules and expands them over 100 years on either side of initialization, matching Go's historical behavior. Standard bias is ignored when daylight saving time is disabled. The Windows ABI and APIs follow [TIME_ZONE_INFORMATION](https://learn.microsoft.com/en-us/windows/win32/api/timezoneapi/ns-timezoneapi-time_zone_information), [DYNAMIC_TIME_ZONE_INFORMATION](https://learn.microsoft.com/en-us/windows/win32/api/timezoneapi/ns-timezoneapi-dynamic_time_zone_information) and [EnumDynamicTimeZoneInformation](https://learn.microsoft.com/en-us/windows/win32/api/timezoneapi/nf-timezoneapi-enumdynamictimezoneinformation).

ZIP and Android readers scan metadata using fixed buffers and allocate only the selected payload. Standalone TZif files retain the existing 10 MiB source limit; archive payloads are bounded by the archive and the call's memory budget. File reads, seeks, metadata scans, rule construction and copies check cancellation and charge work. These checkpoints cannot preempt a blocking operating-system call. Source failures release temporary allocations; returned times retain only their timezone data and headers.

The bundled database and Windows abbreviations come from the pinned Go 1.27.1 distribution. `scripts/generate-timezones.py` verifies both source hashes before regeneration; `src/time/zone/data/manifest.json` records the provenance. The IANA database is public domain; adapted code and mappings carry Go's BSD license. See [the notices](../licenses/tzdata-NOTICE.txt).

Native regression tests compare all 598 bundled zones at 13 recorded instants against Go and exercise malformed archives, source limits, I/O cancellation, Windows layouts, abbreviations, seasonal transitions and disabled DST. Windows, Android and iOS code has also been checked with their target standard libraries. Actual Windows/Android/iOS system integration and the browser-Wasm local-time adapter remain unverified or pending; cross-compilation does not establish runtime conformance on those platforms.

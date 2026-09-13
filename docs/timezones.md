# Timezone sources

Named zones use the first source that contains valid TZif data:

1. `ZONEINFO`, when nonempty, naming a directory or an uncompressed `.zip` archive.
2. Installed platform sources: the usual Unix zoneinfo directories, Android's packed `tzdata` databases, or an iOS app's `zoneinfo.zip`.
3. The bundled IANA 2026c database, containing 598 zones in 408,467 bytes.

An absent, unreadable, unsupported or malformed source falls through to the next source. Quota exhaustion and cancellation propagate immediately. ZIP loading follows Go's time-package archive conventions, including stored entries and an end record without a ZIP comment. Entry names preserve their bytes. Windows filesystem paths preserve WTF-8 surrogates and replace other invalid bytes individually, matching Go; Unix paths preserve their original bytes.

Local timezone selection depends on the platform:

| Platform | Local timezone |
| --- | --- |
| Unix | `TZ`, including a leading colon, a zone name or an absolute TZif path; `/etc/localtime` when unset; UTC when empty or unavailable |
| Windows | Win32 timezone information, with English abbreviation mappings and capital-letter fallback for unknown names |
| Android and iOS | UTC, matching Go's local-zone implementation |
| WASI | UTC |

`ZONEINFO` affects named lookup; it does not override Unix `TZ` selection. The configuration is captured on first use. Zone data is accounted independently for each call. Windows captures the operating system's current rules and expands them over 100 years on either side of initialization, matching Go's historical behavior. Standard bias is ignored when daylight saving time is disabled. The Windows ABI and APIs follow [TIME_ZONE_INFORMATION](https://learn.microsoft.com/en-us/windows/win32/api/timezoneapi/ns-timezoneapi-time_zone_information), [DYNAMIC_TIME_ZONE_INFORMATION](https://learn.microsoft.com/en-us/windows/win32/api/timezoneapi/ns-timezoneapi-dynamic_time_zone_information) and [EnumDynamicTimeZoneInformation](https://learn.microsoft.com/en-us/windows/win32/api/timezoneapi/nf-timezoneapi-enumdynamictimezoneinformation).

ZIP and Android readers scan metadata using fixed buffers and allocate only the selected payload. Standalone TZif files retain the existing 10 MiB source limit; archive payloads are bounded by the archive and the call's memory budget. File reads, seeks, metadata scans, rule construction and copies check cancellation and charge work. These checkpoints cannot preempt a blocking operating-system call. Source failures release temporary allocations; returned times retain only their timezone data and headers.

The bundled database and Windows abbreviations come from the pinned Go 1.27.1 distribution. `scripts/generate-timezones.py` verifies both source hashes before regeneration; `src/time/zone/data/manifest.json` records the provenance. The IANA database is public domain; adapted code and mappings carry Go's BSD license. See [the notices](../licenses/tzdata-NOTICE.txt).

Native regression tests compare all 598 bundled zones at 13 recorded instants against Go and exercise malformed archives, source limits, I/O cancellation, Windows layouts, abbreviations, seasonal transitions and disabled DST. Windows, Android and iOS code has also been checked with their target standard libraries. Actual Windows/Android/iOS system integration and the browser-Wasm local-time adapter remain unverified or pending; cross-compilation does not establish runtime conformance on those platforms.
